// TODO NEXT TIME:
// - Figure out how to correctly cancel insertion with a CancellationToken
//   - What if one peer successfully finishes the thing, but another doesn't? Then it's canceled and everyone is sad
//   - Can we make the handoff more explicit..?
// - Figure out the stupid race condition during reconciliation; that will need restructuring
//   - I think the actual solution here is to avoid reconciling in-flight state at ALL. On persist, then we
//     notify to everyone anyways? and avoid overdoing it
// - Validate deeply nested documents -- does it work? should we provide the relevant persistence to DependenciesFn?
// -

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
    async fn persist(
        &self,
        id: SedimentreeId,
        doc: Automerge,
    ) -> Result<(), Box<dyn Error + Send + Sync>>;

    /// Hard-lookup to see if a document's heads are persisted.
    async fn is_persisted(
        &self,
        doc_ref: &DocumentRef,
    ) -> Result<bool, Box<dyn Error + Send + Sync>>;

    /// Materialize a persisted document.
    async fn get(&self, id: SedimentreeId) -> Result<Automerge, Box<dyn Error + Send + Sync>>;

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

struct PendingInsertion {
    insertion: Insertion,
    remaining_deps: HashSet<DocumentRef>,
}

type WorkerPool = Arc<Mutex<HashMap<PersistenceId, Arc<PersistenceWorker>>>>;

#[derive(Clone)]
struct PersistenceWorker {
    clockument: Clockument,
    workers: WorkerPool,
    id: PersistenceId,

    token: CancellationToken,
    target: Arc<dyn PersistenceTarget>,

    /// Heads coming in from other persistences, desiring to insert them
    insert_tx: mpsc::UnboundedSender<Insertion>,

    clockument_persist_tx: watch::Sender<()>,

    pending_insertions: Arc<Mutex<Vec<PendingInsertion>>>,
}

impl PersistenceWorker {
    pub fn insert(&self, data: Insertion) {
        let _ = self.insert_tx.send(data);
    }

    /// Subscribe to a notification
    pub fn clockument_persisted(&self) -> watch::Receiver<()> {
        self.clockument_persist_tx.subscribe()
    }

    pub async fn new(
        id: PersistenceId,
        clockument: Clockument,
        target: Arc<dyn PersistenceTarget>,
        workers: WorkerPool,
    ) -> Arc<Self> {
        let (insert_tx, insert_rx) = mpsc::unbounded_channel();
        let (persisted_tx, _) = watch::channel(());

        let this = Self {
            id,
            clockument,
            token: CancellationToken::new(),
            target,
            insert_tx,
            clockument_persist_tx: persisted_tx,
            workers,
            pending_insertions: Default::default(),
        };

        // Then we can start the drivers.
        // TODO: I think this is desirable to do before reconciliation -- is that actually true?
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

    pub async fn reconcile(&self, from: Option<Arc<PersistenceWorker>>) {
        if let Some(worker) = from {
            self.reconcile_from(worker).await;
        }

        self.reconcile_to_all().await;
    }

    async fn reconcile_to_all(&self) {
        // If we don't have a copy of the base clockument, reconciliation is not possible.
        if !self
            .target
            .is_persisted(&DocumentRef {
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
        let deps = self.clockument.get_dependencies(&mut doc);
        // Notify everyone of the dependencies
        for dep in deps {
            self.notify_others(dep).await;
        }
        // Notify everyone of our clockument at the current heads
        self.notify_others(DocumentRef {
            id: self.clockument.id(),
            heads: doc.get_heads().into(),
        })
        .await;
    }

    async fn reconcile_from(&self, other: Arc<PersistenceWorker>) {
        let other_pending = other.pending_insertions.lock().await;
        // If the other has a copy of the clockument persisted, notify ourself about it.
        // This will guaranteed (I think?) get us up to date with the actual persisted clockument.
        if other
            .target
            .is_persisted(&DocumentRef {
                id: self.clockument.id(),
                heads: Heads::default(), // empty heads for any
            })
            .await
            .unwrap()
        {
            // Grab other version of the clockument
            // TODO: don't panic
            let mut doc = other.target.get(self.clockument.id()).await.unwrap();
            // Get the dependencies. If any aren't valid, this persistence is negligent.
            let deps = other.clockument.get_dependencies(&mut doc);
            // Notify self of the dependencies
            for dep in deps {
                self.insert(Insertion {
                    id: dep.id,
                    // TODO: don't panic
                    doc: other.target.get(dep.id).await.unwrap(),
                    token: CancellationToken::new(), // dependencies don't hang
                });
            }
            self.insert(Insertion {
                id: self.clockument.id(),
                doc: doc.clone(),
                token: other.token.clone(),
            });
        }

        // There may be a root insertion whose persistence is still pending.
        for pending in &*other_pending {
            // Add any dependencies that have already been resolved.
            let mut doc = pending.insertion.doc.clone();
            let deps = other.clockument.get_dependencies(&mut doc);

            // Notify self of the dependencies
            for dep in deps {
                // TODO: don't panic
                if other.target.is_persisted(&dep).await.unwrap() {
                    self.insert(Insertion {
                        id: dep.id,
                        // TODO: don't panic
                        doc: other.target.get(dep.id).await.unwrap(),
                        token: CancellationToken::new(), // dependencies don't hang
                    });
                }
            }

            // Do the insertion -- this will check the dependencies for ourself, etc, and
            // probably make our own PendingInsertion
            self.insert(pending.insertion.clone());
        }

        // TODO: Potential race condition here...
        // self (1) is reconciling from other (0).
        // (0) has a PendingInsertion with deps {A,B,C}, from (2)
        // Deps {A, B} is persisted to (0), {C} is not
        // (2) has already looped through the targets and asked it to persist {C}.
        // (0) is simply still waiting to process the event.
        // Therefore, (1) never gets {C} from (0), and since (2) has finished notifying, it never
        // gets it from (2).
        //
        // Is this a real race condition?
        //
        // If so, there's also probably a related, trickier one, where (0) is just taking
        // a while to process/persist {C} -- it's removed from (0)'s recv queue, but not persisted!
        //
        // There's no way for us to figure out where {C} went?!?!
    }

    /// Driver for notifying other drivers of incoming heads
    async fn notifier_driver(&self) {
        let mut heads_stream = self.target.new_heads();
        loop {
            let heads = select! {
                _ = self.token.cancelled() => { break; }
                res = heads_stream.next() => match res {
                    Some(heads) => heads,
                    None => break,
                }
            };

            self.notify_others(heads).await;
        }
    }

    async fn notify_others(&self, doc_ref: DocumentRef) {
        let workers = self.workers.lock().await;
        // TODO: don't panic. If this is'nt found, the peer is negligent.
        let doc = self.target.get(doc_ref.id).await.unwrap();
        for (id, worker) in &*workers {
            if id == &self.id {
                continue;
            }
            worker.insert(Insertion {
                id: doc_ref.id,
                doc: doc.clone(),
                token: self.token.clone(),
            });
        }
    }

    /// Driver for handling new data coming in from other persistence sources
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
        // TODO: don't panic on failure
        self.target
            .persist(insertion.id, insertion.doc)
            .await
            .unwrap();

        // Since we persisted a potential dependency, check to see if it resolves anything.
        self.try_resolve_pending(insertion.id).await;
    }

    async fn try_insert_root(&self, mut insertion: Insertion) {
        // If we're the root, we must check dependencies first.
        let deps = self.clockument.get_dependencies(&mut insertion.doc);
        let remaining_deps = self.retain_pending_deps(deps, None).await;
        if remaining_deps.is_empty() {
            // TODO: don't panic on failure
            self.target
                .persist(insertion.id, insertion.doc)
                .await
                .unwrap();

            self.clockument_persist_tx.send_replace(());

            // Don't need to notify anyone here (except users).
            // The reason for this:
            // - If existing in persistence is heads [A], we assume [A] is already being tracked.
            // - If incoming heads are [B], we assume that's been sent to other persistence targets.
            // - If the final persisted resolves to heads [B], that's fine, we sent it.
            // - If the final persisted resolves to heads [A, B], then others will receive [A] and [B]
            //   individually and do their own merge to the same heads [A, B].
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
        return;
    }

    /// Called when we've inserted a new potential dependency and should check
    /// to see if any pending clockument writes can go through.
    async fn try_resolve_pending(&self, persisted_id: SedimentreeId) {
        // TODO: This is awkward; this could take some time if persist() or retain_pending_deps()
        // takes some time. This mutex locks up the persistence threading, which sucks.
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
                        .persist(pending.insertion.id, pending.insertion.doc)
                        .await
                        .unwrap();
                    self.clockument_persist_tx.send_replace(());
                    continue;
                }
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
            if !self.target.is_persisted(&dep).await.unwrap() {
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
}

pub enum ClockumentError {
    NoSuchTarget(PersistenceId),
    NoSuchDocument(SedimentreeId),
    NoSuchHeads(Heads),
    NoHeadsReady,
    TargetRemoved,
    Persist(Box<dyn Error + Send + Sync>),
}

impl ClockumentCoordinator {
    pub async fn new(clockument: Clockument) -> Self {
        Self {
            clockument,
            last_persistence_id: Default::default(),
            workers: Default::default(),
        }
    }

    /// Add a new persistence target to the coordinator.
    /// Optionally, reconcile from an existing best target, ensuring that the target is
    /// fully synced up and ready for more events.
    pub async fn add_persistence(
        &self,
        target: Box<dyn PersistenceTarget>,
        reconcile_from: Option<PersistenceId>,
    ) -> Result<PersistenceId, ClockumentError> {
        let mut workers = self.workers.lock().await;

        let reconcile_worker = match reconcile_from {
            Some(id) => Some(
                workers
                    .get(&id)
                    .cloned()
                    .ok_or(ClockumentError::NoSuchTarget(id))?,
            ),
            None => None,
        };

        let id = PersistenceId(self.last_persistence_id.fetch_add(1, Ordering::Relaxed));
        let worker = PersistenceWorker::new(
            id,
            self.clockument.clone(),
            target.into(),
            self.workers.clone(),
        )
        .await;

        workers.insert(id, worker.clone());
        drop(workers);

        worker.reconcile(reconcile_worker).await;

        Ok(id)
    }

    /// Remove a persistence target
    pub async fn remove_persistence(&self, id: PersistenceId) -> Result<(), ClockumentError> {
        let mut workers = self.workers.lock().await;
        let worker = workers
            .remove(&id)
            .ok_or(ClockumentError::NoSuchTarget(id))?;
        worker.token.cancel();
        Ok(())
    }

    /// Insert new data into the coordinator. If a document with `id` already exists,
    /// it will be merged into the existing copy.
    /// The data is assumed to be transient: i.e. it is not already persisted anywhere.
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
        let workers = self.workers.lock().await;
        let worker = workers
            .get(&persistence)
            .ok_or(ClockumentError::NoSuchTarget(persistence))?
            .clone();

        drop(workers);

        let mut rx = worker.clockument_persisted();

        // TODO: handle case where no clockument at persistence yet
        let doc = worker
            .target
            .get(self.clockument.id)
            .await
            .map_err(|e| ClockumentError::Persist(e))?;

        // If we're already fully persisted at the desired heads, we're OK.
        if worker
            .target
            .is_persisted(&DocumentRef {
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
                res = rx.changed() => {
                    match res {
                        Ok(()) => {
                            if worker
                                .target
                                .is_persisted(&DocumentRef { id: self.clockument.id, heads: heads.clone() })
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
