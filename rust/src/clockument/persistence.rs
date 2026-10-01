// BIGGEST TODO: This class currently relies on a single known root clockument.
// It uses this clockument to determine dependencies at every step.
// In particular, with rebroadcast, it explicitly broadcasts all current dependencies
// to all other persistence targets.
//
// Soon, we want to allow tracking clockuments dynamically. Ideally, we don't have to think
// about clockuments at all -- a clockument is simply defined by a document *that has DependencyRef dependencies*.
// So, all documents would have get_dependencies called on them -- if no dependencies are returned, it's synced greedily.
//
// However, if we do that, we suddenly have to think about *discovery.* Remote targets usually don't allow you to just
// ask for all document IDs, so our rebroadcast() solution fails. There MUST be a point where the user specifies one
// or more IDs to track.
//
// Also, it gets expensive -- if we're required to identify clockuments via contents, that means we have to check the
// content of every single document, with potentially-expensive `get` calls, every time we persist it...
//
// One solution is to explicitly track a mutable set of root clockuments. But while that solves the discovery and
// efficiency problems, it opens up failure modes. For example, what if we add a clockument with hash X, but we've
// already tracked X as a regular document with no dependencies (let's say, from incoming heads)? The network is polluted!
//
// A solution there is to disallow incoming heads from being tracked unless they are explicitly needed as a dependency...
// But that smells bad.
//
// For now, I'm forcing the user to declare a single clockument root.

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
use futures::{StreamExt, stream::BoxStream};
use indextree::Arena;
use sedimentree_core::id::SedimentreeId;
use thiserror::Error;
use tokio::{
    select,
    sync::{Mutex, mpsc, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{clockument::document_ref::DocumentRef, project::repo::heads::Heads};

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
///
///

#[cfg(test)]
mod tests;

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct PersistenceId(u64);

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("document {0} not found")]
    NotFound(SedimentreeId),

    #[error("operation failed")]
    Other(#[source] Box<dyn std::error::Error + Send + Sync>),
}

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
    async fn put(&self, id: SedimentreeId, doc: Automerge) -> Result<(), PersistenceError>;

    /// Check to see if the document is durably available at the provided ref (i.e. can be acquired by
    /// [PersistenceTarget::has])
    async fn has(&self, doc_ref: &DocumentRef) -> Result<bool, PersistenceError>;

    /// Materialize a persisted document.
    async fn get(&self, id: SedimentreeId) -> Result<Automerge, PersistenceError>;

    /// A stream returning new heads when they are persisted to the source.
    /// This MUST be called by the implementation of [PersistenceTarget::put], when it puts new heads.
    fn new_heads(&self) -> BoxStream<'_, DocumentRef>;
}

pub enum HealResult {
    RollBack(Heads),
    Remove,
}

/// Describes how a negligent [PersistenceTarget] should be handled.
/// A negligent [PersistenceTarget] is a target that has persisted some clockument with heads `A`,
/// but one or more of `dependencies(A)` are not persisted.
/// Negligence will never occur under normal conditions, but disk errors, permission changes, or
/// similar may cause negligence.
pub enum NegligenceDecision {
    /// The target should propagate the document, despite the negligent behavior.
    /// This will likely cause an explosion of negligence across every persistence target.
    /// The dependency will be cached as negligent, and other targets will remove the dependency
    /// when checking if it is safe to propagate
    Propagate,
    /// The target should halt propagation of the clockument. This means that the negligence
    /// will never propagate to other targets, but it also means that we can never receive changes from
    /// the negligent target for as long as it is negligent.
    DoNotPropagate,
}

/// Provides utilities to manage dependencies, given a transaction.
#[async_trait]
pub trait DependencyResolver: Send + Sync {
    /// Get an array of shallowly-nested dependencies from the [Transaction].
    /// Users should implement this based on their own schema, which may vary.
    /// Sub-documents may include more dependencies, so this method must handle arbitrary sizes.
    /// There are two options for a contract: Users can either provide all dependencies at the transaction heads,
    /// or they can provide all dpeendencies at the transaction heads AND all previous heads.
    /// The former upholds the clockument contract for any monotonically-advancing dependencies, but not for reverts.
    /// The latter supports arbitrary reversion of dependencies.
    fn get_dependencies(&self, tx: &Transaction) -> HashSet<DocumentRef>;

    /// The [NegligenceDecision] to enact if a negligent dependency is detected for the clockument.
    fn negligence(&self, clockument_id: SedimentreeId) -> NegligenceDecision;
}

#[derive(Clone)]
struct Insertion {
    id: SedimentreeId,
    doc: Automerge,
    // If this cancels, we give up an insert, if it is pending.
    // Only used for pending from other sources, not transient.
    // Only used for root clockuments.
    token: CancellationToken,
    // A set of the original dependencies from the target's put.
    // Allows us to keep track of poisoned dependencies, so we don't end up
    // relying on dependencies that might never arrive.
    original_dependencies: DependencyTree,
}

struct PendingInsertion {
    insertion: Insertion,
    dependencies: DependencyTree,
}

struct PutResult {
    doc_ref: DocumentRef,
    source: PersistenceId,
    dependencies: DependencyTree,
}

type WorkerPool = Arc<Mutex<HashMap<PersistenceId, Arc<PersistenceWorker>>>>;

// TODO: eventually we'll need rebroadcast data probably (like which clockument to broadcast deps of)
struct Rebroadcast;

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

    /// Channel for rebroadcast requests
    rebroadcast_tx: mpsc::UnboundedSender<Rebroadcast>,

    pending_insertions: Arc<Mutex<Vec<PendingInsertion>>>,
}

#[derive(Clone)]
struct DependencyTreeItem {
    // If a dependency tree item is poisoned, it means we no longer wait for the dependency or any of its children.
    poisoned: bool,
    resolved: bool,
    document_ref: DocumentRef,
}

type DependencyTree = Arena<DependencyTreeItem>;

enum DependencyResolution {
    Resolved { dependencies: HashSet<DocumentRef> },
    NotFound,
    Failed,
}

impl PersistenceWorker {
    pub fn new(
        id: PersistenceId,
        clockument: Clockument,
        target: Arc<dyn PersistenceTarget>,
        put_tx: mpsc::UnboundedSender<PutResult>,
    ) -> Arc<Self> {
        let (insert_tx, insert_rx) = mpsc::unbounded_channel();
        let (rebroadcast_tx, rebroadcast_rx) = mpsc::unbounded_channel();

        let this = Self {
            clockument,
            id,
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
                select! {
                    _ = this.token.cancelled() => {}
                    _ = this.notifier_driver(rebroadcast_rx) => {}
                };
            });
        }
        {
            let this = this.clone();
            tokio::task::spawn(async move {
                select! {
                    _ = this.token.cancelled() => {}
                    _ = this.inserter_driver(insert_rx) => {}
                };
            });
        }
        Arc::new(this)
    }

    pub fn insert(&self, data: Insertion) {
        let _ = self.insert_tx.send(data);
    }

    pub fn rebroadcast(&self, data: Rebroadcast) {
        let _ = self.rebroadcast_tx.send(data);
    }

    async fn notify_rebroadcast(&self) -> Result<(), ClockumentError> {
        // Grab our version of the clockument. If it doesn't exist, we can't rebroadcast it to anyone.
        let heads: Heads = match self.target.get(self.clockument.id).await {
            Ok(doc) => doc.get_heads().into(),
            Err(e) => match e {
                // this is ok!
                PersistenceError::NotFound(_) => return Ok(()),
                PersistenceError::Other(error) => return Err(ClockumentError::Persist(error)),
            },
        };

        // Explicitly poison any not-found elements -- if we've already got a clockument, we'd better have the
        // dependencies!
        let tree = self.initialize_dependencies(heads.clone(), None);
        let tree = self.resolve_dependencies(tree, true).await;

        if tree.iter().any(|d| d.get().poisoned) {
            // TODO: We may want to pause here, and figure out a way to receive other info
            // from other targets. They may have the DocumentRef that is unresolved.
            // Currently, the negligence propagates always. If the target DOES have the dependency,
            // it's removed from the negligence array for future propagations, so eventually targets *should*
            // get informed...
            match self.clockument.dependencies.negligence(self.clockument.id) {
                NegligenceDecision::Propagate => {}
                NegligenceDecision::DoNotPropagate => return Ok(()),
            }
        }

        // Notify everyone of the dependencies that we do have
        for node in &tree {
            let data = node.get();
            if !data.resolved {
                continue;
            }
            let _ = self.put_tx.send(PutResult {
                doc_ref: data.document_ref.clone(),
                source: self.id,
                dependencies: Default::default(),
            });
        }

        // Notify everyone of our clockument at the current heads
        let _ = self.put_tx.send(PutResult {
            doc_ref: DocumentRef::new(self.clockument.id, heads),
            source: self.id,
            dependencies: tree,
        });
        Ok(())
    }

    /// Driver for notifying the parent about puts
    async fn notifier_driver(&self, mut rebroadcast_rx: mpsc::UnboundedReceiver<Rebroadcast>) {
        let mut heads_stream = self.target.new_heads();
        loop {
            select! {
                res = heads_stream.next() => match res {
                    Some(heads) => self.notify_put(&heads).await,
                    None => break,
                },
                res = rebroadcast_rx.recv() => {
                    let _ = match res {
                        Some(_) => (),
                        None => break,
                    };

                    match self.notify_rebroadcast().await {
                        Ok(()) => {},
                        Err(e) => tracing::error!("Error requesting rebroadcast: {e:?}"),
                    }
                }
            }
        }
    }

    async fn notify_put(&self, doc_ref: &DocumentRef) {
        let dependencies = if self.clockument.id == doc_ref.id() {
            let tree = self.initialize_dependencies(doc_ref.heads().clone(), None);

            // Explicitly poison any not-found elements -- if we've put a clockument, we'd better have the
            // dependencies!
            let tree = self.resolve_dependencies(tree, true).await;

            if tree.iter().any(|d| d.get().poisoned) {
                match self.clockument.dependencies.negligence(self.clockument.id) {
                    NegligenceDecision::Propagate => {}
                    NegligenceDecision::DoNotPropagate => return,
                }
            }
            tree
        } else {
            DependencyTree::new()
        };

        let _ = self.put_tx.send(PutResult {
            doc_ref: doc_ref.clone(),
            source: self.id,
            dependencies,
        });

        // Since we persisted a potential dependency, check to see if it resolves anything else.
        self.try_resolve_pending(doc_ref.id()).await;
    }

    /// Returns an unresolved [DependencyTree] with a single dependency, the root Clockument.
    fn initialize_dependencies(
        &self,
        heads: Heads,
        known_root: Option<&mut Automerge>,
    ) -> DependencyTree {
        let mut tree = DependencyTree::new();
        let root = tree.new_node(DependencyTreeItem {
            poisoned: false,
            resolved: known_root.is_some(),
            document_ref: DocumentRef::new(self.clockument.id, heads.clone()),
        });
        if let Some(known_root) = known_root {
            let deps = self.clockument.get_dependencies(known_root, &heads);
            for dep in deps {
                root.append_value(
                    DependencyTreeItem {
                        poisoned: false,
                        resolved: false,
                        document_ref: dep,
                    },
                    &mut tree,
                );
            }
        }
        tree
    }

    // TODO: Currently, every check of this checks the entire fringe. Add a scope_to_id parameter
    // that allows us to ONLY check the incoming document (and whatever children it might produce when resolved).
    /// Resolve the [DependencyTree], inserting new dependencies as they're discovered.
    /// The root item is expected to be a dependency for the clockument itself.
    /// If should_poison is true, marks any not-found dependencies (and their children) as "poisoned".
    /// "Poisoned" just means that the dependency wasn't resolved at the target it's coming from -- meaning the target was negligent.
    /// While this won't prevent future lookups, it ensures that chlid dependencies remain fallible, if their parents are fallible.
    /// In fact, the ability to poison a tree is the entire reason we're using a tree structure at all!
    async fn resolve_dependencies(
        &self,
        mut tree: Arena<DependencyTreeItem>,
        should_poison: bool,
    ) -> Arena<DependencyTreeItem> {
        let mut pending = JoinSet::new();

        let mut frontier: Vec<_> = tree.roots().collect();
        while let Some(item) = frontier.pop() {
            let data = tree.get_data(item).expect("node exists");

            // If the data has already been resolved by a previous invocation, we move immediately onto its children.
            if data.resolved {
                frontier.extend(item.children(&tree));
                continue;
            }

            // Only spawn tasks for unresolved items
            let depth = item.depth(&tree);
            let clockument = self.clockument.clone();
            let target = self.target.clone();
            let doc_ref = data.document_ref.clone();
            pending.spawn(async move {
                (
                    item,
                    Self::resolve_dependency(clockument.clone(), target.clone(), depth, doc_ref)
                        .await,
                )
            });
        }

        while let Some(res) = pending.join_next().await {
            let (item, resolution) = res.expect("dependency resolution task panicked");
            let depth = item.depth(&tree);
            match resolution {
                DependencyResolution::Resolved { dependencies } => {
                    let data = tree.get_data_mut(item).expect("node exists");
                    data.resolved = true;
                    let poisoned = data.poisoned;
                    for dep in dependencies {
                        let child = item.append_value(
                            DependencyTreeItem {
                                document_ref: dep.clone(),
                                poisoned,
                                resolved: false,
                            },
                            &mut tree,
                        );
                        let clockument = self.clockument.clone();
                        let target = self.target.clone();
                        pending.spawn(async move {
                            (
                                child,
                                Self::resolve_dependency(clockument, target, depth + 1, dep).await,
                            )
                        });
                    }
                }
                DependencyResolution::NotFound => {
                    let data = tree.get_data_mut(item).expect("node exists");
                    data.poisoned = data.poisoned || should_poison;
                }
                DependencyResolution::Failed => {
                    let data = tree.get_data_mut(item).expect("node exists");
                    data.poisoned = true;
                }
            }
        }
        tree
    }

    async fn resolve_dependency(
        clockument: Clockument,
        target: Arc<dyn PersistenceTarget>,
        depth: usize,
        document_ref: DocumentRef,
    ) -> DependencyResolution {
        // If we're beyond a certain level, replace the expensive get check with a cheap has check.
        if clockument
            .dependency_search_depth
            .is_some_and(|d| d < depth)
        {
            let resolved = target
                .has(&document_ref)
                .await
                .inspect_err(|e| tracing::error!("Unknown error during has: {e}"))
                .unwrap_or(false);
            if resolved {
                return DependencyResolution::Resolved {
                    dependencies: Default::default(),
                };
            } else {
                return DependencyResolution::NotFound;
            }
        }

        // We assume this is cheap on a 404. Then, once we've resolved, we never need to run it again.
        let mut doc = match target.get(document_ref.id()).await {
            Ok(doc) => doc,
            Err(e) => match e {
                PersistenceError::NotFound(_) => {
                    return DependencyResolution::NotFound;
                }
                PersistenceError::Other(error) => {
                    tracing::error!("Unknown error during dependency tree fetch: {error}");
                    // Assume that we'll never get this doc, so give up.
                    return DependencyResolution::Failed;
                }
            },
        };

        return DependencyResolution::Resolved {
            dependencies: clockument.get_dependencies(&mut doc, document_ref.heads()),
        };
    }

    fn propagate_poison(
        mut dependencies: DependencyTree,
        source_dependencies: &DependencyTree,
    ) -> DependencyTree {
        // Notes the heads that have been poisoned.
        // Warning: this method doesn't check derivative heads...
        // If a parent head has been poisoned, the child MIGHT be poisoned (unless we've explicitly repaired it)!
        // So, callers just assume that poisoned data has been merged alongside non-poisoned data.
        let mut poisoned: HashMap<SedimentreeId, HashSet<ChangeHash>> = HashMap::new();

        for dep in source_dependencies {
            let data = dep.get();
            if data.poisoned {
                let entry = poisoned
                    .entry(data.document_ref.id())
                    .or_insert(HashSet::new());
                entry.extend(data.document_ref.heads().iter());
            }
        }

        for node in &mut dependencies {
            let data = node.get_mut();
            let Some(poisoned_heads) = poisoned.get(&data.document_ref.id()) else {
                continue;
            };
            // The heads are only poisoned if all the poisoned heads are included.
            data.poisoned = poisoned_heads
                .iter()
                .all(|h| data.document_ref.heads().iter().any(|head| head == h));
        }

        // Now that we've seeded the poison from the original set, make sure all children have that poison.
        let mut processing = Vec::new();
        for root in dependencies.roots() {
            processing.push(root);
        }
        while let Some(item) = processing.pop() {
            let parent = item.parent(&dependencies);
            let poisoned = parent
                .map(|node| dependencies.get_data(node).expect("parent exists").poisoned)
                .unwrap_or(false);
            let data = dependencies.get_data_mut(item).expect("id exists");
            data.poisoned = data.poisoned || poisoned;

            for child in item.children(&dependencies) {
                processing.push(child);
            }
        }

        dependencies
    }

    /// Driver for handling new insertions coming in from other sources
    async fn inserter_driver(&self, mut insert_rx: mpsc::UnboundedReceiver<Insertion>) {
        loop {
            let insertion = match insert_rx.recv().await {
                Some(r) => r,
                None => break,
            };
            // we may want to semaphore this?
            let this = self.clone();
            let tok = self.token.clone();
            tokio::task::spawn(async move {
                select! {
                    _ = tok.cancelled() => {}
                    _ = this.try_insert(insertion) => {}
                }
            });
        }
    }

    // TODO: See if we can spawn off a task to do this method -- this could be v slow!
    async fn try_insert(&self, mut insertion: Insertion) {
        if insertion.id == self.clockument.id {
            let tree = self.initialize_dependencies(
                insertion.doc.get_heads().into(),
                Some(&mut insertion.doc),
            );

            // We MUST lock this here. At any point, we might get a new_heads notification that resolves a dependency.
            // As such, we need to lock our pending insertions array at the same time we're checking for the dependency.
            // That way, we never miss a try_resolve_pending.
            let mut pendings = self.pending_insertions.lock().await;

            // Don't poison 404s here -- we're just still waiting on missing deps.
            // This still could get poisoned if our target fails.
            let tree = self.resolve_dependencies(tree, false).await;

            // Propagate the poison from the original insertion
            let tree = Self::propagate_poison(tree, &insertion.original_dependencies);

            if self.try_put_tree(&tree).await {
                self.do_put(insertion).await;
                return;
            }

            // If the dependencies are not resolved, we gotta wait on them first.
            // Push it to our pending array.
            // When others are inserted, we'll re-check and try again.
            pendings.push(PendingInsertion {
                insertion,
                dependencies: tree,
            });
        } else {
            self.do_put(insertion).await;
        }
    }

    async fn try_put_tree(&self, tree: &DependencyTree) -> bool {
        // don't do anything until the whole tree is either poisoned or resolved
        if !tree
            .iter()
            .all(|item| item.get().poisoned || item.get().resolved)
        {
            return false;
        }

        // If we resolved any poisoned deps, announce them to everyone!
        for node in tree {
            let data = node.get();
            if !data.poisoned || !data.resolved {
                continue;
            }
            let _ = self.put_tx.send(PutResult {
                doc_ref: data.document_ref.clone(),
                source: self.id,
                dependencies: Default::default(),
            });
        }
        true
    }

    async fn do_put(&self, insertion: Insertion) {
        match self.target.put(insertion.id, insertion.doc).await {
            Ok(()) => {}
            Err(e) => tracing::error!("Error putting: {e}"),
        }
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

            // TODO: Scope this by persisted_id
            pending.dependencies = self
                .resolve_dependencies(std::mem::take(&mut pending.dependencies), false)
                .await;

            if self.try_put_tree(&pending.dependencies).await {
                let pending = pendings.remove(i);
                self.do_put(pending.insertion).await;
                continue;
            }

            // If we still have more deps, and it's been canceled, give up actually.
            if pending.insertion.token.is_cancelled() {
                let _ = pendings.remove(i);
                continue;
            }

            i += 1;
        }
    }
}

#[derive(Clone)]
pub struct Clockument {
    id: SedimentreeId,
    /// Specifies a depth which to stop searching for dependencies.
    /// 0 means that only the root clockument is checked for dependencies. None has no depth, which means
    /// ALL documents are checked for dependencies, which may incur a performance penalty.
    dependency_search_depth: Option<usize>,
    dependencies: Arc<dyn DependencyResolver>,
}

// TODO: Add "identify clockument" feature to support adding arbitrary clockuments rather than just one
// TODO: Add a way to handle errors
impl Clockument {
    pub fn new(
        id: SedimentreeId,
        dependencies: Arc<dyn DependencyResolver>,
        dependency_search_depth: Option<usize>,
    ) -> Self {
        Self {
            id,
            dependencies,
            dependency_search_depth,
        }
    }

    pub fn id(&self) -> SedimentreeId {
        self.id
    }

    fn get_dependencies(&self, doc: &mut Automerge, heads: &Heads) -> HashSet<DocumentRef> {
        let tx = doc
            .transaction_at(PatchLog::inactive(), heads.iter().as_slice())
            .unwrap();
        self.dependencies.get_dependencies(&tx)
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
                    select! {
                        _ = token.cancelled() => {}
                        _ = Self::driver_loop(puts_rx, clockument, clockument_put_tx, workers) => {}
                    }
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
        target: Arc<dyn PersistenceTarget>,
    ) -> Result<PersistenceId, ClockumentError> {
        let mut workers = self.workers.lock().await;

        let id = PersistenceId(self.last_persistence_id.fetch_add(1, Ordering::Relaxed));
        let worker =
            PersistenceWorker::new(id, self.clockument.clone(), target, self.puts_tx.clone());

        workers.insert(id, worker.clone());

        for (_, worker) in &*workers {
            // Ask every worker to re-announce its persisted data to everyone else.
            // This is intended to catch the new worker up to everyone, and also inform everyone
            // of the new worker's data.
            worker.rebroadcast(Rebroadcast);
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
        clockument: Clockument,
        clockument_put_tx: watch::Sender<()>,
        workers: WorkerPool,
    ) {
        // TODO: I think we can spawn off subtasks for the put ops... maybe
        loop {
            // Whenever we see new heads incoming from somewhere, broadcast it to every driver.
            let put = match puts_rx.recv().await {
                Some(put) => put,
                None => {
                    break;
                }
            };

            // Notify subscribers if the clockument changed
            if put.doc_ref.id() == clockument.id() {
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
            let doc = select! {
                _ = worker.token.cancelled() => { continue; }
                res = worker.target.get(put.doc_ref.id()) => res,
            };

            let doc = match doc {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("error getting putted doc: {e}");
                    continue;
                }
            };

            for (id, worker) in &*workers {
                // Don't do extra work
                if *id == put.source {
                    continue;
                }
                worker.insert(Insertion {
                    doc: doc.clone(),
                    id: put.doc_ref.id(),
                    // This will be used to cancel a PendingInsertion if the source worker drops.
                    // TODO: Investigate issues here -- is there ever a situation where the source worker
                    // drops after broadcasting all dependencies, and the pending cancels anyways?
                    // That's OK if it's just one worker -- but the issue is that maybe, a source worker A drops
                    // and doesn't insert, then a source worker B successfully inserts. But then the source worker C
                    // would persist heads successfully... unless the heads didn't actually change on C!
                    // But if that's the case, presumably B would already know about the heads from C anyways.
                    // So I think we're good? Double check this.
                    token: worker.token.clone(),
                    original_dependencies: put.dependencies.clone(),
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
                // We're expecting users to ALWAYS insert/transact dependencies into the coordinator.
                // So, we don't need any poisonable dependencies.
                original_dependencies: Default::default(),
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

        let mut doc = worker
            .target
            .get(self.clockument.id)
            .await
            .map_err(|e| match e {
                PersistenceError::NotFound(sedimentree_id) => {
                    ClockumentError::NoSuchDocument(sedimentree_id)
                }
                PersistenceError::Other(error) => ClockumentError::Persist(error),
            })?;

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

        let mut doc = select! {
            _ = worker.token.cancelled() => {
                return Err(ClockumentError::TargetRemoved);
            }
            doc = worker.target.get(doc_ref.id()) => {
                doc
            }
        }
        .map_err(|e| match e {
            PersistenceError::NotFound(id) => ClockumentError::NoSuchDocument(id),
            PersistenceError::Other(error) => ClockumentError::Persist(error),
        })?;

        let persisted_heads = doc.get_heads();
        let res = {
            let mut tx = doc
                .transaction_at(
                    PatchLog::inactive(),
                    doc_ref.heads().clone().into_iter().as_slice(),
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
        self.insert(doc_ref.id(), doc).await?;

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
        let doc_ref = DocumentRef::new(self.clockument.id, heads.clone());
        select! {
            _ = worker.token.cancelled() => {
                return Err(ClockumentError::TargetRemoved);
            }
            has = worker.target.has(&doc_ref) => {
                if has.map_err(|e|ClockumentError::Persist(Box::new(e)))? {
                    return Ok(());
                }
            }
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
                                .has(&DocumentRef::new(self.clockument.id(), heads.clone()))
                                .await
                                .map_err(|e| ClockumentError::Persist(Box::new(e)))?
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
