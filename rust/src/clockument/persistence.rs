use std::{
    collections::{HashMap, HashSet},
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use automerge::{Automerge, PatchLog, transaction::Transaction};
use futures::{StreamExt, stream::BoxStream};
use sedimentree_core::id::SedimentreeId;
use thiserror::Error;
use tokio::{
    select,
    sync::{Mutex, mpsc, watch},
};
use tokio_util::sync::CancellationToken;

use crate::project::repo::heads::Heads;

/// Desired API (pseudocode for now)
///
/// First, we need to implement our persitence targets, with the [PersistenceTarget] struct.
///
/// Implement [PersistenceTarget] for disk, server, peers, etc.
///
/// Then, make a Clockument. A Clockument is defined as an ID and a dependencies function.
///
/// We need a way to get the dependencies from a clockument at a particular heads.
/// get_dependencies must return all current dependencies.
/// If revert-safety is desired, it must return all current and past dependencies.
///
/// ```
///
/// fn get_dependencies(doc: &Automerge, heads: Heads) -> HashSet<DocumentRef> {
///     return doc.deps; // get all dependencies here
/// }
///
/// ```
///
/// To initialize the clockument:
/// ```
/// // root clockument ID
/// let clockument = Clockument::new(clockument_id, get_dependencies);
///
/// let coordinator = ClockumentCoordinator::new(clockument);
/// // Persistences immediately begin syncing with each other as needed
/// let disk_persistence = coordinator.add_persistence(disk: DiskPersistenceTarget)
/// let remote_persistence = coordinator.add_persistence(remote: RemotePersistenceTarget)
/// // If this is a brand new clockument, insert the data:
/// coordinator.insert(my_data: Automerge);
/// // Otherwise, the clockument and dependencies will automatically be loaded from the persistence.
///
/// ```
///
/// When we want to commit:
/// ```
/// // To alter dependencies, first get the ref of the dependency from the coordinator.
/// // The current transaction heads are guaranteed to be valid on disk_persistence (e.g. `tx.get()`).
/// // Historical heads are guaranteed to be valid as well, because we always track all previous dependencies.
/// let dep_1_ref = coordinator.transact(disk_persistence, async move |tx| {
///     doc.get("dep1")
/// });
///
/// // Then we can directly alter it on disk_persistence, getting a new ref.
/// let dep_1_ref = coordinator.transact_at(dep_1_ref, disk_persistence, async move |mut tx| {
///     // do some work, and then commit:
///     let new_dep_head = tx.commit();
///     DocumentRef { dep_1_ref.id, new_dep_head }
/// });
///
/// // Alternatively, we could add a new dependency.
/// let dep_2_doc = Automerge::new("some content");
/// let dep_2_ref = DocumentRef { id: SedimentreeId::generate(), heads: dep_2_doc.get_heads() };
/// // If somehow there's already a document in here, it'll just get merged and we'll end up using our new, unrelated heads.
/// // This effectively overwrites the document at our new clockument heads.
/// coordinator.insert(dep_2_ref.id, dep_2_doc);
///
/// // Or perhaps, we could roll back a dependency (revert).
/// // If get_dependencies provides the historical dependency support, this is always safe for well-behaved peers.
/// let dep_3_ref = clockument.transact(disk_persistence, async move |tx| {
///     doc.get_at("dep3", some_old_heads)
/// });
///
/// // Add both documents as a dependencies to the clockument.
/// // This will automatically begin discovery and propagation of dep3.
/// let new_head = clockument.transact(disk_persistence, async move |mut tx| {
///     tx.insert("dep1", dep_1_ref);
///     tx.insert("dep2", dep_2_ref);
///     tx.insert("dep3", dep_3_ref);
///     tx.commit() // return the new_head
/// });
///
/// // Wait for everything to be fully persisted.
/// // Do this before transacting anymore stuff, because it might commit to the old heads and cause
/// // a merge conflict.
/// clockument.heads_ready(clockument_id, new_head, disk_persistence).await;
/// ```
///
///

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocumentRef {
    id: SedimentreeId,
    heads: Heads,
}

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct PersistenceId(u64);

// Initial assumptions:
//  - Users are using Subduction, Tokio, Automerge
//  - 2PC to a document is not required by the user
//  - Persistence targets are never negligent and never have broken dependencies persisted.

/// Persistence targets, like disk, or peers, or a memory store
#[async_trait]
pub trait PersistenceTarget: Send + Sync {
    /// Persist a document to the source.
    /// Implementers should ensure this is cancellation-safe, and parallelizable.
    /// If there is an existing doc at the source, it should be merged into the doc.
    /// Null-op persistence should do nothing (i.e. if the destination document is identical to
    /// or a superset of the source document).
    /// Additionally, any actual persistent writes should appear atomic (such as to-disk).
    async fn put(
        &self,
        id: SedimentreeId,
        doc: Automerge,
    ) -> Result<(), Box<dyn Error + Send + Sync>>;

    /// Check to see if the document is durably available at the provided ref (i.e. can be acquired by
    /// [PersistenceTarget::has])
    async fn has(&self, doc_ref: &DocumentRef) -> Result<bool, Box<dyn Error + Send + Sync>>;

    /// Materialize a persisted document.
    async fn get(&self, id: SedimentreeId) -> Result<Automerge, Box<dyn Error + Send + Sync>>;

    /// A stream returning new heads when they are persisted to the source.
    /// This MUST be called by the implementation of [PersistenceTarget::put], when it puts new heads.
    fn new_heads(&self) -> BoxStream<'_, DocumentRef>;
}

/// A function to get the dependencies, given a transaction.
/// Users should implement this based on their own schema, which may vary.
/// There are two options for a contract: Users can either provide all dependencies at the transaction heads,
/// or they can provide all dpeendencies at the transaction heads AND all previous heads.
/// The former upholds the clockument contract for any monotonically-advancing dependencies, but not for reverts.
/// The latter supports arbitrary reversion of dependencies.
pub type DependenciesFn = Arc<dyn Fn(&Transaction) -> HashSet<DocumentRef> + Send + Sync>;

#[derive(Clone)]
struct Insertion {
    id: SedimentreeId,
    doc: Automerge,
    // If this cancels, we give up an insert, if it is pending.
    // Only used for pending from other sources, not transient.
    // Only used for root clockuments.
    token: CancellationToken,
}

struct PutResult {
    doc_ref: DocumentRef,
    source: PersistenceId,
    // todo: track failure here?
}

struct PendingInsertion {
    insertion: Insertion,
    remaining_deps: HashSet<DocumentRef>,
}

type WorkerPool = Arc<Mutex<HashMap<PersistenceId, Arc<PersistenceWorker>>>>;

#[derive(Clone)]
struct PersistenceWorker {
    clockument: Clockument,
    id: PersistenceId,

    token: CancellationToken,
    target: Arc<dyn PersistenceTarget>,

    /// Channel for insertions this worker needs to process
    insert_tx: mpsc::UnboundedSender<Insertion>,

    /// Channel for when new data is available, i.e. when we put stuff
    put_tx: mpsc::UnboundedSender<PutResult>,

    /// Sent when we get a rebroadcast request
    rebroadcast_tx: watch::Sender<()>,

    pending_insertions: Arc<Mutex<Vec<PendingInsertion>>>,
}

impl PersistenceWorker {
    pub fn new(
        id: PersistenceId,
        clockument: Clockument,
        target: Arc<dyn PersistenceTarget>,
        put_tx: mpsc::UnboundedSender<PutResult>,
    ) -> Arc<Self> {
        let (insert_tx, insert_rx) = mpsc::unbounded_channel();
        let (rebroadcast_tx, _) = watch::channel(());

        let this = Self {
            id,
            clockument,
            token: CancellationToken::new(),
            target,
            insert_tx,
            put_tx,
            pending_insertions: Default::default(),
            rebroadcast_tx,
        };

        {
            let this = this.clone();
            tokio::task::spawn(async move {
                this.notifier_driver().await;
            });
        }
        {
            let this = this.clone();
            tokio::task::spawn(async move {
                this.inserter_driver(insert_rx).await;
            });
        }
        Arc::new(this)
    }

    pub fn insert(&self, data: Insertion) {
        let _ = self.insert_tx.send(data);
    }

    pub fn rebroadcast(&self) {
        let _ = self.rebroadcast_tx.send_replace(());
    }

    async fn notify_rebroadcast(&self) {
        // If we don't have a copy of the base clockument, reconciliation is not possible.
        if !self
            .target
            .has(&DocumentRef {
                id: self.clockument.id(),
                heads: Heads::default(), // empty heads for any
            })
            .await
            .unwrap()
        {
            return;
        }

        // Grab our version of the clockument
        // TODO: don't panic
        let mut doc = self.target.get(self.clockument.id()).await.unwrap();
        // Get the dependencies. If any aren't valid, this persistence is negligent.
        // TODO: handle the negligent case or something
        let deps = self.clockument.get_dependencies(&mut doc);
        // Notify everyone of the dependencies
        for dep in deps {
            let _ = self.put_tx.send(PutResult {
                doc_ref: dep,
                source: self.id,
            });
        }
        // Notify everyone of our clockument at the current heads
        let _ = self.put_tx.send(PutResult {
            doc_ref: DocumentRef {
                heads: doc.get_heads().into(),
                id: self.clockument.id(),
            },
            source: self.id,
        });
    }

    /// Driver for notifying the parent about puts
    async fn notifier_driver(&self) {
        let mut heads_stream = self.target.new_heads();
        let mut rebroadcast_rx = self.rebroadcast_tx.subscribe();
        loop {
            select! {
                _ = self.token.cancelled() => { break; }
                res = heads_stream.next() => match res {
                    Some(heads) => self.notify_put(heads).await,
                    None => break,
                },
                _ = rebroadcast_rx.changed() => {
                    self.notify_rebroadcast().await;
                }
            }
        }
    }

    async fn notify_put(&self, heads: DocumentRef) {
        let _ = self.put_tx.send(PutResult {
            doc_ref: heads.clone(),
            source: self.id,
        });

        // Since we persisted a potential dependency, check to see if it resolves anything.
        self.try_resolve_pending(heads.id).await;
    }

    /// Driver for handling new insertions coming in from other sources
    async fn inserter_driver(&self, mut insert_rx: mpsc::UnboundedReceiver<Insertion>) {
        loop {
            let insertion = select! {
                _ = self.token.cancelled() => { break; }
                res = insert_rx.recv() => match res {
                    Some(r) => r,
                    None => break,
                }
            };
            // we may want to semaphore this?
            let this = self.clone();
            tokio::task::spawn(async move {
                this.try_insert(insertion).await;
            });
        }
    }

    async fn try_insert(&self, insertion: Insertion) {
        // Special-case the root clockument
        if insertion.id == self.clockument.id() {
            self.try_insert_root(insertion).await;
            return;
        }

        // For anything that's not the root, greedily persist it.
        // If it's a dependency, we'll resolve pendings during the later put event.
        // TODO: don't panic on failure
        self.target.put(insertion.id, insertion.doc).await.unwrap();
    }

    async fn try_insert_root(&self, mut insertion: Insertion) {
        // If we're the root, we must check dependencies first.
        let deps: HashSet<DocumentRef> = self.clockument.get_dependencies(&mut insertion.doc);
        let remaining_deps = self.retain_pending_deps(deps, None).await;
        if remaining_deps.is_empty() {
            // TODO: don't panic on failure
            self.target.put(insertion.id, insertion.doc).await.unwrap();
            return;
        }

        // If the dependencies are not resolved, we gotta wait on them first.
        // Push it to our pending array.
        // When others are inserted, we'll re-check and try again.
        let mut pendings = self.pending_insertions.lock().await;
        pendings.push(PendingInsertion {
            insertion,
            remaining_deps,
        });
    }

    /// Called when we've inserted a new potential dependency and should check
    /// to see if any pending clockument writes can go through.
    async fn try_resolve_pending(&self, persisted_id: SedimentreeId) {
        // TODO: This is awkward; this could take some time if persist() or retain_pending_deps()
        // takes some time. This mutex locks up the put threading, which sucks.
        let mut pendings = self.pending_insertions.lock().await;

        let mut i = 0;
        while i < pendings.len() {
            let pending = &mut pendings[i];

            pending.remaining_deps = self
                .retain_pending_deps(
                    std::mem::take(&mut pending.remaining_deps),
                    Some(persisted_id),
                )
                .await;

            // Once we've drained remaining_deps, re-check get_dependencies in case persisting
            // a dependency has caused a different behavior in the clockument.
            // This might occur in the case of deeply-nested dependencies, where we can only
            // verify a dependency once a further-nested dependency is seen.
            // TODO (important): This recheck seems correct, but I think DependenciesFn should have
            // some reference to the persistence at-hand...
            // That's weird with its transaction-based flow, though.
            if pending.remaining_deps.is_empty() {
                pending.remaining_deps = self
                    .retain_pending_deps(
                        self.clockument.get_dependencies(&mut pending.insertion.doc),
                        None,
                    )
                    .await;

                if pending.remaining_deps.is_empty() {
                    let pending = pendings.remove(i);
                    // TODO: don't panic on failure
                    self.target
                        .put(pending.insertion.id, pending.insertion.doc)
                        .await
                        .unwrap();
                    continue;
                }
            }

            // If we still have more deps, and it's been canceled, give up actually.
            if pending.insertion.token.is_cancelled() {
                let _ = pendings.remove(i);
                continue;
            }

            i += 1;
        }
    }

    async fn retain_pending_deps(
        &self,
        deps: HashSet<DocumentRef>,
        scope_to_id: Option<SedimentreeId>,
    ) -> HashSet<DocumentRef> {
        let mut remaining_deps = HashSet::new();

        for dep in deps {
            if let Some(id) = scope_to_id
                && dep.id != id
            {
                remaining_deps.insert(dep);
                continue;
            }

            // TODO: don't panic on error
            if !self.target.has(&dep).await.unwrap() {
                remaining_deps.insert(dep);
            }
        }

        remaining_deps
    }
}

#[derive(Clone)]
pub struct Clockument {
    id: SedimentreeId,
    dependencies_fn: DependenciesFn,
}

impl Clockument {
    pub fn new(id: SedimentreeId, dependencies_fn: DependenciesFn) -> Self {
        Self {
            id,
            dependencies_fn,
        }
    }

    pub fn id(&self) -> SedimentreeId {
        self.id
    }

    fn get_dependencies(&self, doc: &mut Automerge) -> HashSet<DocumentRef> {
        let tx = doc
            .transaction_at(PatchLog::inactive(), doc.get_heads().as_slice())
            .unwrap();
        (self.dependencies_fn)(&tx)
    }
}

pub struct ClockumentCoordinator {
    clockument: Clockument,

    last_persistence_id: AtomicU64,
    workers: WorkerPool,

    puts_tx: mpsc::UnboundedSender<PutResult>,

    clockument_put_tx: watch::Sender<()>,

    token: CancellationToken,
}

#[derive(Debug, Error)]
pub enum ClockumentError {
    #[error("the clockument didn't have an added persistence of ID {0:?}")]
    NoSuchTarget(PersistenceId),
    #[error("no document was found matching the ID {0}")]
    NoSuchDocument(SedimentreeId),
    #[error("no heads {0:?} were found on the document")]
    NoSuchHeads(Heads),
    #[error("the document did not have any heads ready")]
    NoHeadsReady,
    #[error("the target was removed")]
    TargetRemoved,
    #[error("there was an error in persistence: {0}")]
    Persist(Box<dyn Error + Send + Sync>),
}

impl ClockumentCoordinator {
    pub fn new(clockument: Clockument) -> Self {
        let (puts_tx, puts_rx) = mpsc::unbounded_channel();
        let (clockument_put_tx, _) = watch::channel(());
        let token = CancellationToken::new();
        let workers = WorkerPool::new(Default::default());

        {
            let workers = workers.clone();
            let token = token.clone();
            let clockument = clockument.clone();
            let clockument_put_tx = clockument_put_tx.clone();
            {
                tokio::task::spawn(async move {
                    Self::driver_loop(puts_rx, token, clockument, clockument_put_tx, workers).await;
                });
            }
        }

        Self {
            clockument,
            last_persistence_id: Default::default(),
            workers,
            puts_tx,
            token: token.clone(),
            clockument_put_tx: clockument_put_tx.clone(),
        }
    }

    /// Add a new persistence target to the coordinator.
    /// Optionally, reconcile from an existing best target, ensuring that the target is
    /// fully synced up and ready for more events.
    pub async fn add_persistence(
        &self,
        target: Box<dyn PersistenceTarget>,
    ) -> Result<PersistenceId, ClockumentError> {
        let mut workers = self.workers.lock().await;

        let id = PersistenceId(self.last_persistence_id.fetch_add(1, Ordering::Relaxed));
        let worker = PersistenceWorker::new(
            id,
            self.clockument.clone(),
            target.into(),
            self.puts_tx.clone(),
        );

        workers.insert(id, worker.clone());

        for (_, worker) in &*workers {
            // Ask every worker to re-announce its persisted data to everyone else.
            // This is intended to catch the new worker up to everyone, and also inform everyone
            // of the new worker's data.
            worker.rebroadcast();
        }

        Ok(id)
    }

    /// Remove a persistence target
    pub async fn remove_persistence(&self, id: PersistenceId) -> Result<(), ClockumentError> {
        let mut workers = self.workers.lock().await;
        let worker = workers
            .remove(&id)
            .ok_or(ClockumentError::NoSuchTarget(id))?;

        // Shuts down all the inner stuff, AND notifies pending clockuments that are expecting stuff from the worker
        // to give up.
        worker.token.cancel();
        Ok(())
    }

    async fn driver_loop(
        mut puts_rx: mpsc::UnboundedReceiver<PutResult>,
        token: CancellationToken,
        clockument: Clockument,
        clockument_put_tx: watch::Sender<()>,
        workers: WorkerPool,
    ) {
        loop {
            // Whenever we see new heads incoming from somewhere, broadcast it to every driver.
            let put = select! {
                _ = token.cancelled() => { break; }
                put = puts_rx.recv() => match put {
                    Some(put) => put,
                    None => { break; }
                }
            };

            // Notify subscribers if the clockument changed
            if put.doc_ref.id == clockument.id {
                clockument_put_tx.send_replace(());
            }

            let workers = workers.lock().await;
            let Some(worker) = workers.get(&put.source) else {
                // Worker was removed, therefore we don't bother
                continue;
            };

            // TODO: don't panic
            // TODO: do we really always need to clone this into every worker? definitely not.
            // We should instead store the heads of the inserted doc during put (the actual heads that would occur
            // on materialization), pass them through to here, and get ad-hoc if needed.
            // But that causes tricky conditions where a clockument is updated on the persistence again,
            // and the merged result is then bad. So maybe we could only pull the doc through here for clockuments?
            let doc = worker.target.get(put.doc_ref.id).await.unwrap();
            let token = worker.token.clone();

            for (id, worker) in &*workers {
                // Don't do extra work
                if *id == put.source {
                    continue;
                }
                worker.insert(Insertion {
                    doc: doc.clone(),
                    id: put.doc_ref.id,
                    // This will be used to cancel a PendingInsertion if the source worker drops.
                    // TODO: Investigate issues here -- is there ever a situation where the source worker
                    // drops after broadcasting all dependencies, and the pending cancels anyways?
                    // That's OK if it's just one worker -- but the issue is that maybe, a source worker A drops
                    // and doesn't insert, then a source worker B successfully inserts. But then the source worker C
                    // would persist heads successfully... unless the heads didn't actually change on C!
                    // But if that's the case, presumably B would already know about the heads from C anyways.
                    // So I think we're good? Double check this.
                    token: token.clone(),
                });
            }
        }
    }

    /// Insert new data into the coordinator. If a document with `id` already exists,
    /// it will be merged into the existing copy.
    /// This method will return when the insertion is queued, but not when any actual
    /// insertion is done.
    /// If `id` is that of the root clockument, it will not be persisted into any location until
    /// ALL dependencies have also been persisted to that location.
    /// As such, if `id` is a root clockument, the user MUST [Self::insert] or [Self::transact] ALL
    /// untracked dependencies (including heads) into the coordinator!
    pub async fn insert(&self, id: SedimentreeId, doc: Automerge) -> Result<(), ClockumentError> {
        let workers = self.workers.lock().await;
        for (_, worker) in &*workers {
            worker.insert(Insertion {
                token: CancellationToken::new(), // can never be cancelled!
                id,
                doc: doc.clone(),
            })
        }
        Ok(())
    }

    /// Transact over the clockument, scoped to the most recent valid heads on a persistence.
    /// If new dependencies are added, they MUST be added with [Self::insert] or [Self::transact].
    /// The transaction will occur at the most-recently persisted heads. If transactions occur on the same heads,
    /// they will both be persisted, and will both be merged.
    pub async fn transact<F, R>(
        &self,
        persistence: PersistenceId,
        f: F,
    ) -> Result<R, ClockumentError>
    where
        F: AsyncFnOnce(&mut Transaction) -> R,
    {
        let workers = self.workers.lock().await;
        let worker = workers
            .get(&persistence)
            .ok_or(ClockumentError::NoSuchTarget(persistence))?
            .clone();

        drop(workers);

        // TODO: handle missing docs
        let mut doc = worker
            .target
            .get(self.clockument.id)
            .await
            .map_err(|e| ClockumentError::Persist(e))?;
        let persisted_heads = doc.get_heads();
        let res = {
            let mut tx = doc
                .transaction_at(
                    PatchLog::inactive(),
                    persisted_heads.clone().into_iter().as_slice(),
                )
                .unwrap();

            f(&mut tx).await
        };

        // Nothing changed
        if doc.get_heads() == persisted_heads {
            return Ok(res);
        }

        // Persist to all sources. If something else persists in the meantime,
        // our changes will be merged in.
        self.insert(self.clockument.id, doc).await?;

        Ok(res)
    }

    pub async fn transact_at<F, R>(
        &self,
        doc_ref: DocumentRef,
        persistence: PersistenceId,
        f: F,
    ) -> Result<R, ClockumentError>
    where
        F: AsyncFnOnce(&mut Transaction) -> R,
    {
        let workers = self.workers.lock().await;
        let worker = workers
            .get(&persistence)
            .ok_or(ClockumentError::NoSuchTarget(persistence))?
            .clone();

        drop(workers);

        // TODO: handle missing docs
        let mut doc = worker
            .target
            .get(doc_ref.id)
            .await
            .map_err(|e| ClockumentError::Persist(e))?;
        let persisted_heads = doc.get_heads();
        let res = {
            let mut tx = doc
                .transaction_at(
                    PatchLog::inactive(),
                    doc_ref.heads.clone().into_iter().as_slice(),
                )
                .unwrap();

            f(&mut tx).await
        };

        // Nothing changed
        if doc.get_heads() == persisted_heads {
            return Ok(res);
        }

        // Persist to all sources. If something else persists in the meantime,
        // our changes will be merged in.
        self.insert(doc_ref.id, doc).await?;

        Ok(res)
    }

    /// After transacting, await this to ensure the transaction has persisted.
    pub async fn heads_ready(
        &self,
        heads: Heads,
        persistence: PersistenceId,
    ) -> Result<(), ClockumentError> {
        let mut rx = self.clockument_put_tx.subscribe();

        let workers = self.workers.lock().await;
        let worker = workers
            .get(&persistence)
            .ok_or(ClockumentError::NoSuchTarget(persistence))?
            .clone();

        drop(workers);

        // If we're already fully persisted at the desired heads, we're OK.
        if worker
            .target
            .has(&DocumentRef {
                id: self.clockument.id,
                heads: heads.clone(),
            })
            .await
            .map_err(|e| ClockumentError::Persist(e))?
        {
            return Ok(());
        }

        loop {
            select! {
                _ = worker.token.cancelled() => {
                    return Err(ClockumentError::TargetRemoved);
                }
                // This is always emitted AFTER a put -- so if we must wait some time before this fires, that's OK.
                // This will occur on the next put, when we actually put the clockument.
                res = rx.changed() => {
                    match res {
                        Ok(()) => {
                            if worker
                                .target
                                .has(&DocumentRef { id: self.clockument.id, heads: heads.clone() })
                                .await
                                .map_err(|e| ClockumentError::Persist(e))?
                            {
                                return Ok(());
                            }

                        },
                        _ => return Err(ClockumentError::TargetRemoved),
                    }
                }
            }
        }
    }
}
