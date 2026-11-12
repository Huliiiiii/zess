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
    state: SubscriptionState,
    query: Arc<Query>,
    registration: Arc<Registration>,
}

enum SubscriptionState {
    Waiting,
    Reading,
    Ready(Outcome),
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
struct Entry {
    snapshot: Option<CachedSnapshot>,
    flight: Option<Flight>,
}

impl Entry {
    fn invalidate(&mut self) {
        self.snapshot = None;
        if let Some(flight) = self.flight.take() {
            flight.job.cancel();
        }
    }
}

#[derive(Default)]
pub(crate) struct FsCache {
    entries: HashMap<PathBuf, Entry>,
    subscriptions: HashMap<u64, Subscription>,
    revision: u64,
}

impl FsCache {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn len(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.snapshot.is_some())
            .count()
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
            .entries
            .get(&path)
            .and_then(|entry| entry.snapshot.as_ref())
            .filter(|cached| cached.binding.is_current())
        else {
            return Ok(None);
        };
        cached.snapshot.response(relative, request).map(Some)
    }

    pub(crate) fn invalidate(&mut self) {
        // Dropping each receiver prevents its worker from publishing into a newer read.
        for (_, mut entry) in self.entries.drain() {
            entry.invalidate();
        }
        for subscription in self.subscriptions.values_mut() {
            subscription.state = SubscriptionState::Waiting;
        }
        self.revision += 1;
    }

    pub(crate) fn invalidate_path(&mut self, path: &Path) {
        if let Some(mut entry) = self.entries.remove(path) {
            entry.invalidate();
        }
        for subscription in self.subscriptions.values_mut() {
            if subscription.path == path {
                subscription.state = SubscriptionState::Waiting;
            }
        }
    }

    pub(crate) fn cancel_all(&mut self) {
        for entry in self.entries.values_mut() {
            if let Some(flight) = entry.flight.take() {
                flight.job.cancel();
            }
        }
        self.subscriptions.clear();
        self.entries.retain(|_, entry| entry.snapshot.is_some());
    }

    fn detach(&mut self, handle: u64) -> Option<Subscription> {
        let subscription = self.subscriptions.remove(&handle)?;
        if let Some(entry) = self.entries.get_mut(&subscription.path) {
            if let Some(flight) = &mut entry.flight {
                flight.subscribers.remove(&handle);
                if flight.subscribers.is_empty() {
                    entry.flight.take().unwrap().job.cancel();
                }
            }
            if entry.snapshot.is_none() && entry.flight.is_none() {
                self.entries.remove(&subscription.path);
            }
        }
        Some(subscription)
    }

    pub(crate) fn cancel(&mut self, handle: u64) {
        self.detach(handle);
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
        let state = if registration.ready() == Ok(true) {
            self.entries
                .get(&path)
                .and_then(|entry| entry.snapshot.as_ref())
                .map(|cached| SubscriptionState::Ready(Ok(cached.snapshot.clone())))
                .unwrap_or(SubscriptionState::Waiting)
        } else {
            SubscriptionState::Waiting
        };
        self.subscriptions.insert(
            handle,
            Subscription {
                path,
                relative: relative.to_owned(),
                request,
                state,
                query,
                registration,
            },
        );
        self.attach(handle, scheduler);
        Ok(())
    }

    fn attach(&mut self, handle: u64, scheduler: &JobScheduler) {
        let subscription = self.subscriptions.get_mut(&handle).unwrap();
        if !matches!(subscription.state, SubscriptionState::Waiting) {
            return;
        }
        match subscription.ready() {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                subscription.state = SubscriptionState::Ready(Err(error));
                return;
            }
        }
        let entry = self.entries.entry(subscription.path.clone()).or_default();
        if let Some(flight) = &mut entry.flight {
            flight.subscribers.insert(handle);
            subscription.state = SubscriptionState::Reading;
            return;
        }
        match scheduler.start_fs(subscription.path.to_string_lossy().into_owned()) {
            Ok(Some(job)) => {
                entry.flight = Some(Flight {
                    job,
                    subscribers: HashSet::from([handle]),
                    binding: CacheBinding::new(&subscription.query),
                });
                subscription.state = SubscriptionState::Reading;
            }
            // A full queue leaves a live subscription to retry on the next poll.
            Ok(None) => {}
            Err(error) => {
                subscription.state = SubscriptionState::Ready(Err(format!("{error:#}")));
            }
        }
    }

    fn publish(&mut self, path: &Path, outcome: Outcome, binding: CacheBinding) {
        if let Ok(snapshot) = &outcome {
            self.entries.entry(path.to_owned()).or_default().snapshot = Some(CachedSnapshot {
                snapshot: snapshot.clone(),
                binding,
            });
        }
        for subscription in self.subscriptions.values_mut() {
            if subscription.path == path
                && !matches!(subscription.state, SubscriptionState::Ready(_))
            {
                subscription.state = match subscription.ready() {
                    Ok(true) => SubscriptionState::Ready(outcome.clone()),
                    Ok(false) => SubscriptionState::Waiting,
                    Err(error) => SubscriptionState::Ready(Err(error)),
                };
            }
        }
    }

    fn pump(&mut self, scheduler: &JobScheduler) {
        let completed = self
            .entries
            .iter()
            .filter_map(|(path, entry)| {
                let flight = entry.flight.as_ref()?;
                let outcome = match flight.job.result.try_recv() {
                    Ok(outcome) => outcome,
                    Err(TryRecvError::Empty) => return None,
                    Err(TryRecvError::Disconnected) => {
                        Err("background service stopped without a result".into())
                    }
                };
                Some((path.clone(), outcome))
            })
            .collect::<Vec<_>>();
        for (path, outcome) in completed {
            let flight = self.entries.get_mut(&path).unwrap().flight.take().unwrap();
            self.publish(&path, outcome, flight.binding);
        }
        let pending = self
            .subscriptions
            .iter()
            .filter_map(|(handle, subscription)| {
                matches!(subscription.state, SubscriptionState::Waiting).then_some(*handle)
            })
            .collect::<Vec<_>>();
        for handle in pending {
            self.attach(handle, scheduler);
        }
        self.entries
            .retain(|_, entry| entry.snapshot.is_some() || entry.flight.is_some());
    }

    pub(crate) fn poll(&mut self, handle: u64, scheduler: &JobScheduler) -> Option<QueryPoll> {
        if !self.subscriptions.contains_key(&handle) {
            return None;
        }
        self.pump(scheduler);
        if !matches!(
            self.subscriptions[&handle].state,
            SubscriptionState::Ready(_)
        ) {
            return Some(QueryPoll::Pending);
        }
        let subscription = self.detach(handle).unwrap();
        let SubscriptionState::Ready(outcome) = subscription.state else {
            unreachable!()
        };
        let result = outcome.and_then(|snapshot| {
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
        let (outcome, binding) = if let Some(cached) = self
            .entries
            .get(&path)
            .and_then(|entry| entry.snapshot.as_ref())
        {
            (Ok(cached.snapshot.clone()), cached.binding.clone())
        } else if let Some(flight) = self
            .entries
            .get_mut(&path)
            .and_then(|entry| entry.flight.take())
        {
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
