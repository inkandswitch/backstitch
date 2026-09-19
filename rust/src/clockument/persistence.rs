/// OVERALL THOUGHTS FROM THIS FIRST PASS:
/// I think it mostly works, but I think there's a better fundamental architecture...
/// Right now, the tasks kind of arbitrarily nest and have awkward cancellation dynamics.
/// I want to CONSIDER an alternative:
///  - Each persistence target has a driver thread
///  - For dependencies, these targets aggressively persist to each other, announcing whenever their
///    heads have been altered (i.e. a persist makes a change). This can occur from the user via
///    insert_dependency (i.e. remote heads come in). Use broadcasts for these. Spawn off the actual persist
///    task, so nothing hangs.
///  - For the root, when new heads come in (insert), we assume the deps are persisted. Announce the new heads
///    to others. When receiving others' root heads, put it in a state object per PersistenceId, and merge it in
///    only when it's actually ready. If new heads supersede before it's ready, just replace it and recheck
///    dependencies.
///
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use automerge::{Automerge, ChangeHash, PatchLog, transaction::Transaction};
use autosurgeon::{Hydrate, Reconcile};
use sedimentree_core::{id::SedimentreeId, sedimentree::Sedimentree};
use tokio::{
    select,
    sync::{
        Mutex,
        broadcast::{self, error::RecvError},
        watch,
    },
};
use tokio_util::sync::CancellationToken;

use crate::project::repo::heads::Heads;

/// Desired API (pseudocode for now)
///
/// First, we need to implement our persitence targets, with the [PersistenceTarget] struct.
///
/// Implement [PersistenceTarget] for disk, server, etc.
///
/// We need a way to get the dependencies from a clockument at a particular heads.
/// get_dependencies must return all current dependencies.
/// If revert-safety is desired, it must return all current and past dependencies.
///
/// ```
///
/// // TODO: Currently for simplicity we're assuming dependencies are not deeply nested.
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
/// let disk_persistence = clockument.add_persistence_target(disk: DiskPersistenceTarget)
/// let remote_persistence = clockument.add_persistence_target(remote: RemotePersistenceTarget)
/// // If this is from disk, insert it from disk. If it's a brand new clockument, use Transient to persist to all targets.
/// clockument.insert(my_data: Automerge, PersistenceSource::Transient);
/// // Insert all the dependencies from disk...
///
/// ```
///
/// When we receive new remote heads:
/// ```
/// let id: SedimentreeId;
/// let some_doc: Automerge;
/// // This has different behaviors for a new root clockument versus a new dependency.
/// // For a dependency, it will immediately propagate to other persistence targets.
/// // For a root clockument ID, it will hold it in an in-memory cache until all incoming dependencies have persisted.
/// // This method assumes that it already exists at remote_persistence with the given heads.
/// if id == root {
///     clockument.insert(some_doc, PersistenceSource::Persisted(remote_persistence))
/// }
/// else {
///     clockument.insert_dependency(id, some_doc, PersistenceSource::Persisted(remote_persistence))
/// }
/// ```
///
/// When we want to commit:
/// ```
/// // To alter dependencies, first get the ref of the dependency from the clockument.
/// // The current transaction heads are guaranteed to be valid on disk_persistence (e.g. `tx.get()`).
/// // Historical heads are guaranteed to be valid as well, because we always track all previous dependencies.
/// let dep_1_ref = clockument.transact(disk_persistence, async move |tx| {
///     doc.get("dep1")
/// });
///
/// // Then we can directly alter it on disk_persistence, getting a new ref.
/// let dep_1_ref = clockument.transact_dependency(dep_1_ref, disk_persistence, async move |mut tx| {
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
/// clockument.insert_dependency(dep_2_ref.id, dep_2_doc, PersistenceSource::Transient);
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
//  - All deps are shallowly nested inside the clockument
//  - Users are using Subduction, Tokio, Automerge
//  - 2PC to a document is not required by the user
//  - Persistence targets are added immediatley after construction and before insertion of dependencies,
//    and never removed.
//  - Persistence targets are never negligent and never have broken dependencies persisted.

/// Persistence targets, like disk, or peers, or a memory store
#[async_trait]
pub trait PersistenceTarget: Send + Sync {
    /// Persist a document to the source.
    /// Implementers should ensure this is cancellation-safe, and parallelizable.
    /// If there is an existing doc at the source, it should be merged into the doc.
    /// Additionally, any actual persistent writes should appear atomic (such as to-disk).
    async fn persist(
        &self,
        id: SedimentreeId,
        doc: &Automerge,
    ) -> Result<(), Box<dyn Error + Send + Sync>>;

    /// Hard-lookup to see if a document's heads are persisted.
    async fn is_persisted(&self, r#ref: &DocumentRef)
    -> Result<bool, Box<dyn Error + Send + Sync>>;

    /// Materialize a persisted document.
    async fn get(&self, id: SedimentreeId) -> Result<Automerge, Box<dyn Error + Send + Sync>>;
}

/// The persistence source for an incoming document.
pub enum PersistenceSource {
    /// The document is new, or has new heads that we must persist everywhere.
    Transient,
    /// The document is incoming from a specific persistence source, such as new heads from a peer,
    /// or a document read off a disk.
    Persisted(PersistenceId),
}

/// A function to get the dependencies, given a transaction.
/// Users should implement this based on their own schema, which may vary.
/// There are two options for a contract: Users can either provide all dependencies at the transaction heads,
/// or they can provide all dpeendencies at the transaction heads AND all previous heads.
/// The former upholds the clockument contract for any monotonically-advancing dependencies, but not for reverts.
/// The latter supports arbitrary reversion of dependencies.
pub type DependenciesFn = Box<dyn Fn(&Transaction) -> HashSet<DocumentRef>>;

#[derive(Clone)]
struct PersistenceTargetWrapper {
    token: CancellationToken,
    target: Arc<dyn PersistenceTarget>,
    root_tx: broadcast::Sender<()>,
    dependency_tx: broadcast::Sender<SedimentreeId>,
}

pub struct Clockument {
    id: SedimentreeId,
    last_target_id: AtomicU64,
    dependencies_fn: DependenciesFn,

    /// The tracked persistence targets.
    targets: Arc<Mutex<HashMap<PersistenceId, PersistenceTargetWrapper>>>,
}

pub enum ClockumentError {
    NoSuchTarget(PersistenceId),
    NoSuchDocument(SedimentreeId),
    NoSuchHeads(Heads),
    NoHeadsReady,
    TargetRemoved,
    Persist(Box<dyn Error + Send + Sync>),
}

impl Clockument {
    pub async fn new(id: SedimentreeId, dependencies_fn: DependenciesFn) -> Self {
        Self {
            dependencies_fn,
            targets: Default::default(),
            id,
            last_target_id: Default::default(),
        }
    }

    /// Add a new persistence target to the clockument.
    /// Optionally, reconcile from an existing target. This will begin a background task
    /// that ensures the newly-added target has been informed of the data from the target specified
    /// in `reconcile_from`.
    pub async fn add_persistence_target(
        &self,
        target: Box<dyn PersistenceTarget>,
        reconcile_from: Option<PersistenceId>,
    ) -> PersistenceId {
        let mut targets = self.targets.lock().await;
        let id = self.last_target_id.fetch_add(1, Ordering::Relaxed);
        let id = PersistenceId(id);
        let (dependency_tx, _) = broadcast::channel(4096);
        let (root_tx, _) = broadcast::channel(4096);
        targets.insert(
            id,
            PersistenceTargetWrapper {
                token: Default::default(),
                target: Arc::from(target),
                dependency_tx,
                root_tx,
            },
        );

        // TODO: This reconcile idea works, but there's a problem: What if we're already inserting
        // some data in `insert` but it hasn't made its way to reconcile_from?
        // Then, we're reconciling an out of date document....

        // TODO: Do the actual reconcile

        id
    }

    /// Remove a persisence target
    pub async fn remove_persistence_target(
        &self,
        id: PersistenceId,
    ) -> Result<(), ClockumentError> {
        let mut targets = self.targets.lock().await;
        let target = targets
            .remove(&id)
            .ok_or(ClockumentError::NoSuchTarget(id))?;
        target.token.cancel();
        Ok(())
    }

    /// Insert the clockument data from the `source`.
    /// The source is assumed to be already persisted: it should be used for incoming heads.
    /// To insert everywhere, use [`PersistenceSource::Transient`].
    /// When the heads are ready, it will be persisted (and merged) into all other sources.
    /// If the heads can never become ready (i.e. `source` is removed), the persistence is cancelled.
    /// All untracked document dependencies MUST be eventually inserted with [`Clockument::insert_dependency`].
    // TODO: How do we handle fallibility here, like if a peer is negligent and has a broken dependency?
    // Then, insert_dependency may never be called, and this waits forever. A simple timeout is not enough,
    // because we need to allow the user to resolve the fragmented clockument.
    pub async fn insert(
        &self,
        doc: Automerge,
        source: PersistenceSource,
    ) -> Result<(), ClockumentError> {
        let targets = self.targets.clone();
        let target_source = if let PersistenceSource::Persisted(id) = source {
            let targets = self.targets.lock().await;
            // This is potentialy fallible if a persistence target gets removed (i.e. connection gets dropped).
            // If so, we don't want to proceed.
            Some(
                targets
                    .get(&id)
                    .ok_or(ClockumentError::NoSuchTarget(id))?
                    .clone(),
            )
        } else {
            None
        };

        let source_id = match source {
            PersistenceSource::Transient => None,
            PersistenceSource::Persisted(persistence_id) => Some(persistence_id),
        };

        let tok = target_source
            .map(|s| s.token)
            .unwrap_or(CancellationToken::new());

        let mut doc = doc;
        let dependencies = self.get_dependencies(&mut doc);

        let targets = self.targets.lock().await;
        for (id, target) in targets.iter() {
            // We assume the source already has the dependencies, if we're pulling in from the source.
            if let Some(source_id) = source_id
                && &source_id == id
            {
                continue;
            };
            let tgt = target.clone();

            let deps = dependencies.clone();
            let doc = doc.clone();
            let id = self.id;
            let tok = tok.clone();

            // TODO: Once we no longer assume targets are kept forever, we need to fully handle the
            // case where a source target is dropped midway through insertion into other targets.
            // Right now, we use tok to cancel the task if the source target gets canceled.
            // That's fine.
            // But, if dependencies have all resolved, insert_into could totally be dropped for persistence target
            // B and not C, after A is canceled. So B would get a full copy, but A never gets the memo.
            // In that case, we have two options:
            // 1. Be smarter about the cancellation. Make sure if ANY target gets to resolve this call,
            // they all do (assuming those targets aren't themself canceled.)
            // 2. If B gets a full copy, allow B to inform C, somehow.
            tokio::task::spawn(async move {
                select! {
                    _ = tok.cancelled() => {}
                    _ = Self::insert_into(id, doc, tgt, deps) => {}
                }
            });
        }

        Ok(())
    }

    async fn insert_into(
        clockument_id: SedimentreeId,
        doc: Automerge,
        target: PersistenceTargetWrapper,
        deps: HashSet<DocumentRef>,
    ) -> Result<(), ClockumentError> {
        select! {
            _ = target.token.cancelled() => { return Err(ClockumentError::TargetRemoved); }
            _ = Self::dependencies_ready(target.clone(), deps) => {}
        }

        // We're confident the dependencies are ready, so we can persist!
        target
            .target
            .persist(clockument_id, &doc)
            .await
            .map_err(|e| ClockumentError::Persist(e))?;

        // A merge might've occurred during persistence.
        // Invariant: If dependencies([headA]) and dependencies([headB]) are persisted,
        // dependencies([headA, headB]) must be persisted.
        // So that's OK.

        let _ = target.root_tx.send(());

        Ok(())
    }

    async fn insert_dep_into(
        id: SedimentreeId,
        doc: Automerge,
        target: PersistenceTargetWrapper,
    ) -> Result<(), ClockumentError> {
        target
            .target
            .persist(id, &doc)
            .await
            .map_err(|e| ClockumentError::Persist(e))?;

        let _ = target.dependency_tx.send(id);

        Ok(())
    }

    async fn dependencies_ready(
        target: PersistenceTargetWrapper,
        // TODO: this implies deeply-nested dependencies can't be a thing
        // Perhaps we force clients to track deeply-nested dependencies at the root
        dependencies: HashSet<DocumentRef>,
    ) {
        let mut rx = target.dependency_tx.subscribe();

        // First, we loop through dependencies to filter out those already persisted.
        let mut untracked = HashSet::new();
        for dependency in dependencies {
            // TODO: don't panic on error
            if !target.target.is_persisted(&dependency).await.unwrap() {
                untracked.insert(dependency);
            }
        }

        loop {
            match rx.recv().await {
                Ok(v) => {
                    for dp in untracked.clone() {
                        if dp.id == v {
                            // TODO: don't panic
                            if target.target.is_persisted(&dp).await.unwrap() {
                                untracked.remove(&dp);
                            }
                        }
                    }
                }
                Err(e) => {
                    match e {
                        // TODO: does this need special handling? probly not.
                        broadcast::error::RecvError::Closed => {
                            std::future::pending::<()>().await;
                        }
                        // if we lagged, just check the whole array
                        broadcast::error::RecvError::Lagged(e) => {
                            for dp in untracked.clone() {
                                // TODO: don't panic
                                if target.target.is_persisted(&dp).await.unwrap() {
                                    untracked.remove(&dp);
                                }
                            }
                        }
                    }
                }
            }

            if untracked.is_empty() {
                break;
            }
        }
    }

    fn get_dependencies(&self, doc: &mut Automerge) -> HashSet<DocumentRef> {
        let tx = doc
            .transaction_at(PatchLog::inactive(), doc.get_heads().as_slice())
            .unwrap();
        (self.dependencies_fn)(&tx)
    }

    /// Insert data for a potentially-dependent document.
    /// The source is assumed to be already persisted: it should be used for incoming heads.
    /// To insert everywhere, use [PersistenceSource::Transient].
    /// It will be greedily persisted (and merged) into all other sources.
    pub async fn insert_dependency(
        &self,
        doc: Automerge,
        id: SedimentreeId,
        source: PersistenceSource,
    ) -> Result<(), ClockumentError> {
        let source_id = match source {
            PersistenceSource::Transient => None,
            PersistenceSource::Persisted(persistence_id) => Some(persistence_id),
        };

        let targets = self.targets.lock().await;
        for (pid, target) in targets.iter() {
            // We assume the source already has the dependency, if we're pulling in from the source.
            if let Some(source_id) = source_id
                && &source_id == pid
            {
                continue;
            };
            let tgt = target.clone();

            let doc = doc.clone();
            tokio::task::spawn(async move {
                Self::insert_dep_into(id, doc, tgt).await;
            });
        }
        Ok(())
    }

    /// Transact over the clockument, scoped to the most recent valid heads.
    /// If new dependencies are added, they MUST be added with insert_dependency or transact_dependency.
    /// The transaction will occur at the most-recently persisted heads. If transactions occur on the same heads,
    /// they will both be persisted, and will both be merged.
    // TODO: What can callers do to avoid NoHeadsReady?
    pub async fn transact<F, R>(
        &self,
        persistence: PersistenceId,
        f: F,
    ) -> Result<R, ClockumentError>
    where
        F: AsyncFnOnce(&mut Transaction) -> R,
    {
        let targets = self.targets.lock().await;
        let target = targets
            .get(&persistence)
            .ok_or(ClockumentError::NoSuchTarget(persistence))?
            .clone();

        drop(targets);

        let mut doc = target
            .target
            .get(self.id)
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
        self.insert(doc, PersistenceSource::Transient).await?;

        Ok(res)
    }

    /// Transact over a clockument dependency, scoped to the provided head.
    pub async fn transact_dependency<F, R>(
        &self,
        persistence: PersistenceId,
        r#ref: DocumentRef,
        f: F,
    ) -> Result<R, ClockumentError>
    where
        F: AsyncFnOnce(&mut Transaction) -> R,
    {
        let targets = self.targets.lock().await;
        let target = targets
            .get(&persistence)
            .ok_or(ClockumentError::NoSuchTarget(persistence))?
            .clone();

        drop(targets);

        let mut doc = target
            .target
            .get(r#ref.id)
            .await
            .map_err(|e| ClockumentError::Persist(e))?;

        let heads_before = doc.get_heads();

        let res = {
            let mut tx = doc
                .transaction_at(
                    PatchLog::inactive(),
                    r#ref.heads.clone().into_iter().as_slice(),
                )
                .unwrap();

            f(&mut tx).await
        };

        // Nothing changed
        if heads_before == doc.get_heads() {
            return Ok(res);
        }

        // Persist to all sources
        self.insert_dependency(doc, r#ref.id, PersistenceSource::Transient)
            .await?;

        Ok(res)
    }

    /// After transacting, await this to ensure the transaction has persisted.
    pub async fn heads_ready(
        &self,
        heads: Heads,
        persistence: PersistenceId,
    ) -> Result<(), ClockumentError> {
        let targets = self.targets.lock().await;
        let target = targets
            .get(&persistence)
            .ok_or(ClockumentError::NoSuchTarget(persistence))?
            .clone();

        drop(targets);

        let mut rx = target.root_tx.subscribe();

        let doc = target
            .target
            .get(self.id)
            .await
            .map_err(|e| ClockumentError::Persist(e))?;

        // TODO: does this work?
        // We gotta check the doc if the heads exist in it, because we might miss a heads persistence.

        let r#ref = &DocumentRef { id: self.id, heads };
        if target
            .target
            .is_persisted(r#ref)
            .await
            .map_err(|e| ClockumentError::Persist(e))?
        {
            return Ok(());
        }

        loop {
            select! {
                _ = target.token.cancelled() => {
                    return Err(ClockumentError::TargetRemoved);
                }
                res = rx.recv() => {
                    match res {
                        Ok(()) | Err(RecvError::Lagged(_)) => {
                            if target
                                .target
                                .is_persisted(r#ref)
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
