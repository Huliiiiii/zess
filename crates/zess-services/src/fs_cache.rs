use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Weak};

use crate::query::{QueryPoll, QueryRequest, QueryResult};
use anyhow::{Result, bail};

use crate::filesystem::{Request, Snapshot};
use crate::jobs::{Cancellation, Job, JobScheduler};
use crate::watcher::{Query, Registration};
use crate::workspace::Workspace;

type Outcome = Result<Arc<Snapshot>, String>;

#[derive(Clone)]
struct CacheBinding {
    query: Weak<Query>,
    registration: Arc<Registration>,
}

impl CacheBinding {
    fn new(query: &Arc<Query>) -> Self {
        Self {
            query: Arc::downgrade(query),
            registration: query.registration(),
        }
    }

    fn is_current(&self) -> bool {
        self.query
            .upgrade()
            .is_some_and(|query| query.cache_is_current(&self.registration))
    }
}

struct CachedSnapshot {
    snapshot: Arc<Snapshot>,
    binding: CacheBinding,
}

struct Subscription {
    path: PathBuf,
    relative: String,
    request: Request,
    outcome: Option<Outcome>,
    query: Arc<Query>,
    registration: Arc<Registration>,
}

impl Subscription {
    fn ready(&mut self) -> Result<bool, String> {
        self.registration.ready()?;
        self.registration = self.query.registration();
        self.registration.ready()
    }
}

struct Flight {
    job: Job<Outcome>,
    subscribers: HashSet<u64>,
    binding: CacheBinding,
}

#[derive(Default)]
pub(crate) struct FsCache {
    ready: HashMap<PathBuf, CachedSnapshot>,
    flights: HashMap<PathBuf, Flight>,
    subscriptions: HashMap<u64, Subscription>,
    revision: u64,
}

impl FsCache {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn len(&self) -> usize {
        self.ready.len()
    }

    pub(crate) fn jobs(&self) -> usize {
        self.subscriptions.len()
    }

    pub(crate) fn cached_query(&self, query_request: &QueryRequest) -> Result<Option<QueryResult>> {
        if !matches!(
            query_request,
            QueryRequest::List { .. } | QueryRequest::Preview { .. }
        ) {
            return Ok(None);
        }
        let (relative, request) = Request::from_query(query_request)?;
        let path = key(relative)?;
        let Some(cached) = self
            .ready
            .get(&path)
            .filter(|cached| cached.binding.is_current())
        else {
            return Ok(None);
        };
        cached.snapshot.response(relative, request).map(Some)
    }

    pub(crate) fn invalidate(&mut self) {
        self.ready.clear();
        // Dropping each receiver prevents its worker from publishing into a newer flight.
        for (_, flight) in self.flights.drain() {
            flight.job.cancel();
        }
        for subscription in self.subscriptions.values_mut() {
            subscription.outcome = None;
        }
        self.revision += 1;
    }

    pub(crate) fn invalidate_path(&mut self, path: &Path) {
        self.ready.remove(path);
        if let Some(flight) = self.flights.remove(path) {
            flight.job.cancel();
        }
        for subscription in self.subscriptions.values_mut() {
            if subscription.path == path {
                subscription.outcome = None;
            }
        }
    }

    pub(crate) fn cancel_all(&mut self) {
        for (_, flight) in self.flights.drain() {
            flight.job.cancel();
        }
        self.subscriptions.clear();
    }

    pub(crate) fn cancel(&mut self, handle: u64) {
        let Some(subscription) = self.subscriptions.remove(&handle) else {
            return;
        };
        if let Some(flight) = self.flights.get_mut(&subscription.path) {
            flight.subscribers.remove(&handle);
            if flight.subscribers.is_empty() {
                self.flights
                    .remove(&subscription.path)
                    .unwrap()
                    .job
                    .cancel();
            }
        }
    }

    pub(crate) fn start(
        &mut self,
        query_request: &QueryRequest,
        handle: u64,
        scheduler: &JobScheduler,
        query: Arc<Query>,
    ) -> Result<()> {
        let (relative, request) = Request::from_query(query_request)?;
        let path = key(relative)?;
        self.pump(scheduler);
        let registration = query.registration();
        let outcome = if registration.ready() == Ok(true) {
            self.ready
                .get(&path)
                .map(|cached| Ok(cached.snapshot.clone()))
        } else {
            None
        };
        self.subscriptions.insert(
            handle,
            Subscription {
                path,
                relative: relative.to_owned(),
                request,
                outcome,
                query,
                registration,
            },
        );
        self.attach(handle, scheduler);
        Ok(())
    }

    fn attach(&mut self, handle: u64, scheduler: &JobScheduler) {
        let subscription = self.subscriptions.get_mut(&handle).unwrap();
        if subscription.outcome.is_some() {
            return;
        }
        match subscription.ready() {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                subscription.outcome = Some(Err(error));
                return;
            }
        }
        if let Some(flight) = self.flights.get_mut(&subscription.path) {
            flight.subscribers.insert(handle);
            return;
        }
        match scheduler.start_fs(subscription.path.to_string_lossy().into_owned()) {
            Ok(Some(job)) => {
                self.flights.insert(
                    subscription.path.clone(),
                    Flight {
                        job,
                        subscribers: HashSet::from([handle]),
                        binding: CacheBinding::new(&subscription.query),
                    },
                );
            }
            // A full queue leaves a live subscription to retry on the next poll.
            Ok(None) => {}
            Err(error) => {
                self.subscriptions.get_mut(&handle).unwrap().outcome =
                    Some(Err(format!("{error:#}")));
            }
        }
    }

    fn publish(&mut self, path: &Path, outcome: Outcome, binding: CacheBinding) {
        if let Ok(snapshot) = &outcome {
            self.ready.insert(
                path.to_owned(),
                CachedSnapshot {
                    snapshot: snapshot.clone(),
                    binding,
                },
            );
        }
        for subscription in self.subscriptions.values_mut() {
            if subscription.path == path && subscription.outcome.is_none() {
                subscription.outcome = match subscription.ready() {
                    Ok(true) => Some(outcome.clone()),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                };
            }
        }
    }

    fn pump(&mut self, scheduler: &JobScheduler) {
        let completed = self
            .flights
            .iter()
            .filter_map(|(path, flight)| match flight.job.result.try_recv() {
                Ok(outcome) => Some((path.clone(), outcome)),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some((
                    path.clone(),
                    Err("background service stopped without a result".into()),
                )),
            })
            .collect::<Vec<_>>();
        for (path, outcome) in completed {
            let flight = self.flights.remove(&path).unwrap();
            self.publish(&path, outcome, flight.binding);
        }
        let mut pending = Vec::new();
        for (handle, subscription) in &self.subscriptions {
            if subscription.outcome.is_some() {
                continue;
            }
            if self.flights.contains_key(&subscription.path) {
                continue;
            }
            pending.push(*handle);
        }
        for handle in pending {
            self.attach(handle, scheduler);
        }
    }

    pub(crate) fn poll(&mut self, handle: u64, scheduler: &JobScheduler) -> Option<QueryPoll> {
        if !self.subscriptions.contains_key(&handle) {
            return None;
        }
        self.pump(scheduler);
        if self.subscriptions[&handle].outcome.is_none() {
            return Some(QueryPoll::Pending);
        }
        let subscription = self.subscriptions.remove(&handle).unwrap();
        let result = subscription.outcome.unwrap().and_then(|snapshot| {
            snapshot
                .response(&subscription.relative, subscription.request)
                .map_err(|error| format!("{error:#}"))
        });
        Some(QueryPoll::Ready(result))
    }

    pub(crate) fn read(
        &mut self,
        workspace: &Workspace,
        query_request: &QueryRequest,
        refresh: bool,
        query: &Arc<Query>,
    ) -> Result<QueryResult> {
        let (relative, request) = Request::from_query(query_request)?;
        let path = key(relative)?;
        if refresh {
            self.invalidate();
        }
        let (outcome, binding) = if let Some(cached) = self.ready.get(&path) {
            (Ok(cached.snapshot.clone()), cached.binding.clone())
        } else if let Some(flight) = self.flights.remove(&path) {
            let outcome = flight
                .job
                .result
                .recv()
                .unwrap_or_else(|_| Err("background service stopped without a result".into()));
            (outcome, flight.binding)
        } else {
            let binding = CacheBinding::new(query);
            let outcome = workspace
                .read_fs(&path.to_string_lossy(), &Cancellation::default())
                .map(Arc::new)
                .map_err(|error| format!("{error:#}"));
            (outcome, binding)
        };
        self.publish(&path, outcome.clone(), binding);
        outcome
            .map_err(anyhow::Error::msg)?
            .response(relative, request)
    }
}

pub(crate) fn key(relative: &str) -> Result<PathBuf> {
    let mut path = PathBuf::new();
    for part in Path::new(relative).components() {
        match part {
            Component::Normal(name) => path.push(name),
            Component::CurDir => {}
            _ => bail!("path must remain relative to the workspace"),
        }
    }
    Ok(path)
}
