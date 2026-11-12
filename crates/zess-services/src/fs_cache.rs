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
    Failed(String),
}

impl Subscription {
    fn ready(&mut self) -> Result<bool, String> {
        self.registration.ready()?;
        self.registration = self.query.registration();
        self.registration.ready()?;
        Ok(self.query.cache_is_current(&self.registration))
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
    fn current_snapshot(&self) -> Option<&CachedSnapshot> {
        self.snapshot
            .as_ref()
            .filter(|cached| cached.binding.is_current())
    }

    fn retain_live_data(&mut self) -> bool {
        if self
            .snapshot
            .as_ref()
            .is_some_and(|cached| cached.binding.query.strong_count() == 0)
        {
            self.snapshot = None;
        }
        self.snapshot.is_some() || self.flight.is_some()
    }

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

    pub(crate) fn remove_unused_entry(&mut self, path: &Path) {
        if let Some(entry) = self.entries.get_mut(path)
            && !entry.retain_live_data()
        {
            self.entries.remove(path);
        }
    }

    pub(crate) fn remove_unused_entries(&mut self) {
        self.entries.retain(|_, entry| entry.retain_live_data());
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
        let Some(cached) = self.entries.get(&path).and_then(Entry::current_snapshot) else {
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
        self.remove_unused_entries();
    }

    fn detach(&mut self, handle: u64) -> Option<(String, Request, SubscriptionState)> {
        let Subscription {
            path,
            relative,
            request,
            state,
            query,
            ..
        } = self.subscriptions.remove(&handle)?;
        if let Some(entry) = self.entries.get_mut(&path)
            && let Some(flight) = &mut entry.flight
        {
            flight.subscribers.remove(&handle);
            if flight.subscribers.is_empty() {
                entry.flight.take().unwrap().job.cancel();
            }
        }
        drop(query);
        self.remove_unused_entry(&path);
        Some((relative, request, state))
    }

    pub(crate) fn cancel(&mut self, handle: u64) {
        self.detach(handle);
    }

    pub(crate) fn query(&self, handle: u64) -> Option<Arc<Query>> {
        self.subscriptions
            .get(&handle)
            .map(|subscription| subscription.query.clone())
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
        self.subscriptions.insert(
            handle,
            Subscription {
                path,
                relative: relative.to_owned(),
                request,
                state: SubscriptionState::Waiting,
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
                subscription.state = SubscriptionState::Failed(error);
                return;
            }
        }
        let entry = self.entries.entry(subscription.path.clone()).or_default();
        if let Some(cached) = entry.current_snapshot() {
            subscription.state = SubscriptionState::Ready(Ok(cached.snapshot.clone()));
            return;
        }
        if let Some(flight) = &mut entry.flight {
            if flight.binding.is_current() {
                flight.subscribers.insert(handle);
                subscription.state = SubscriptionState::Reading;
            }
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
                subscription.state = SubscriptionState::Failed(format!("{error:#}"));
            }
        }
    }

    fn publish(&mut self, path: &Path, outcome: Outcome, binding: CacheBinding) -> bool {
        if !binding.is_current() {
            self.invalidate_path(path);
            return false;
        }
        if let Ok(snapshot) = &outcome {
            self.entries.entry(path.to_owned()).or_default().snapshot = Some(CachedSnapshot {
                snapshot: snapshot.clone(),
                binding,
            });
        }
        for subscription in self.subscriptions.values_mut() {
            if subscription.path == path
                && matches!(
                    subscription.state,
                    SubscriptionState::Waiting | SubscriptionState::Reading
                )
            {
                subscription.state = match subscription.ready() {
                    Ok(true) => SubscriptionState::Ready(outcome.clone()),
                    Ok(false) => SubscriptionState::Waiting,
                    Err(error) => SubscriptionState::Failed(error),
                };
            }
        }
        true
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
        self.remove_unused_entries();
    }

    pub(crate) fn poll(&mut self, handle: u64, scheduler: &JobScheduler) -> QueryPoll {
        self.pump(scheduler);
        let subscription = &self.subscriptions[&handle];
        match subscription.state {
            SubscriptionState::Waiting | SubscriptionState::Reading => return QueryPoll::Pending,
            SubscriptionState::Ready(_)
                if !subscription
                    .query
                    .cache_is_current(&subscription.registration) =>
            {
                return QueryPoll::Pending;
            }
            SubscriptionState::Ready(_) | SubscriptionState::Failed(_) => {}
        }
        let (relative, request, state) = self.detach(handle).unwrap();
        let outcome = match state {
            SubscriptionState::Ready(outcome) => outcome,
            SubscriptionState::Failed(error) => Err(error),
            SubscriptionState::Waiting | SubscriptionState::Reading => unreachable!(),
        };
        let result = outcome.and_then(|snapshot| {
            snapshot
                .response(&relative, request)
                .map_err(|error| format!("{error:#}"))
        });
        QueryPoll::Ready(result)
    }

    pub(crate) fn read(
        &mut self,
        workspace: &Workspace,
        query_request: &QueryRequest,
        query: &Arc<Query>,
    ) -> Result<Option<QueryResult>> {
        let (relative, request) = Request::from_query(query_request)?;
        let path = key(relative)?;
        let (outcome, binding) =
            if let Some(cached) = self.entries.get(&path).and_then(Entry::current_snapshot) {
                (Ok(cached.snapshot.clone()), cached.binding.clone())
            } else if let Some(flight) = self
                .entries
                .get_mut(&path)
                .and_then(|entry| entry.flight.take())
            {
                if !flight.binding.is_current() {
                    flight.job.cancel();
                    self.invalidate_path(&path);
                    return Ok(None);
                }
                let outcome =
                    flight.job.result.recv().unwrap_or_else(|_| {
                        Err("background service stopped without a result".into())
                    });
                (outcome, flight.binding)
            } else {
                let binding = CacheBinding::new(query);
                let outcome = workspace
                    .read_fs(&path.to_string_lossy(), &Cancellation::default())
                    .map(Arc::new)
                    .map_err(|error| format!("{error:#}"));
                (outcome, binding)
            };
        if !self.publish(&path, outcome.clone(), binding) {
            return Ok(None);
        }
        outcome
            .map_err(anyhow::Error::msg)?
            .response(relative, request)
            .map(Some)
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
