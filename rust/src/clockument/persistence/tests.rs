use std::{
    collections::{HashMap, HashSet},
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::clockument::persistence::{
    ClockumentCoordinator, NegligenceDecision, PersistenceError, PersistenceId,
};
use async_trait::async_trait;
use automerge::{
    Automerge, ChangeHash, PatchLog, ReadDoc,
    transaction::{Transactable, Transaction},
};
use autosurgeon::{Hydrate, Reconcile};
use futures::{StreamExt, stream::BoxStream};
use rand::Rng;
use rstest::{fixture, rstest};
use sedimentree_core::id::SedimentreeId;
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, broadcast, watch};
use tokio_stream::wrappers::BroadcastStream;

use crate::{
    clockument::persistence::{Clockument, DependencyResolver, DocumentRef, PersistenceTarget},
    project::repo::heads::Heads,
};

fn generate_sedimentree_id() -> SedimentreeId {
    let mut id = [0u8; 32];
    rand::rng().fill_bytes(id.as_mut_slice());
    let id = SedimentreeId::from_bytes(id);
    id
}

#[derive(Clone)]
struct TransientTarget {
    ping: Duration,
    data: Arc<RwLock<HashMap<SedimentreeId, Automerge>>>,
    stuck: Arc<Mutex<HashSet<SedimentreeId>>>,

    heads_tx: broadcast::Sender<DocumentRef>,

    op_count: Arc<AtomicUsize>,

    stuck_tx: watch::Sender<()>,
}

impl TransientTarget {
    fn new(ping: Duration) -> Arc<Self> {
        let (heads_tx, _) = broadcast::channel(2048);
        let (stuck_tx, _) = watch::channel(());
        Arc::new(Self {
            ping,
            data: Default::default(),
            stuck: Default::default(),
            heads_tx,
            op_count: Default::default(),
            stuck_tx,
        })
    }
    /// Stop the [SedimentreeId] from being propagated.
    /// This is useful when tests need to ensure sync doesn't occur
    /// before we can check conditions.
    async fn stick(&self, id: SedimentreeId) {
        let mut st = self.stuck.lock().await;
        if st.insert(id) {
            self.stuck_tx.send_replace(());
        }
    }

    /// Un-stop the [SedimentreeId] from being propagated.
    async fn unstick(&self, id: SedimentreeId) {
        let mut st = self.stuck.lock().await;
        if st.remove(&id) {
            self.stuck_tx.send_replace(());
        }
    }

    async fn stuck_guard(&self, id: SedimentreeId) {
        let mut rx = self.stuck_tx.subscribe();
        while {
            let st = self.stuck.lock().await;
            st.contains(&id)
        } {
            let _ = rx.changed().await;
        }
    }

    fn heads_exist(doc: &Automerge, heads: &Heads) -> bool {
        // Every supplied hash must be a change in the document.
        if heads
            .iter()
            .any(|h| doc.get_change_meta_by_hash(h).is_none())
        {
            return false;
        }

        // TODO: ensure no shared ancestry?

        true
    }

    async fn get_fast(&self, id: SedimentreeId) -> Result<Automerge, PersistenceError> {
        self.op_count.fetch_add(1, Ordering::Relaxed);
        self.stuck_guard(id).await;
        let data = self.data.read().await;
        let d = data.get(&id).ok_or(PersistenceError::NotFound(id))?;
        Ok(d.clone())
    }
}

// TODO: Design a set of tests intended for testing PersistenceTarget methods for expected properties.
#[async_trait]
impl PersistenceTarget for TransientTarget {
    async fn put(&self, id: SedimentreeId, doc: Automerge) -> Result<(), PersistenceError> {
        self.op_count.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(self.ping).await;
        self.stuck_guard(id).await;

        let mut data = self.data.write().await;
        let entry = data.entry(id);
        let mut doc = doc;
        let mut modified_heads = None;
        entry
            .and_modify(|e| {
                let heads_before = Heads::from(e.get_heads());
                let res = e.merge(&mut doc);
                // todo: handle error
                let heads_after = Heads::from(res.unwrap());
                if heads_before != heads_after {
                    modified_heads = Some(heads_after);
                }
            })
            .or_insert_with(|| {
                modified_heads = Some(Heads::from(doc.get_heads()));
                doc.clone()
            });

        // TODO: Are there cases where it persists, and is available via get,
        // but this method hasn't returned yet where the sync can break? I don't think so...

        if let Some(heads) = modified_heads {
            let _ = self.heads_tx.send(DocumentRef::new(id, heads));
        }
        Ok(())
    }

    async fn has(&self, doc_ref: &DocumentRef) -> Result<bool, PersistenceError> {
        // println!("HAS: {doc_ref:?}");
        self.op_count.fetch_add(1, Ordering::Relaxed);
        // tokio::time::sleep(self.ping).await;

        self.stuck_guard(doc_ref.id()).await;

        let data = self.data.read().await;

        if let Some(doc) = data.get(&doc_ref.id()) {
            // println!("HAS DOC {}, heads {:?}", doc_ref.id, doc.get_heads());
            if Self::heads_exist(doc, doc_ref.heads()) {
                // println!("HAS HEADS {}", doc_ref.id);
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn get(&self, id: SedimentreeId) -> Result<Automerge, PersistenceError> {
        self.op_count.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(self.ping).await;
        self.stuck_guard(id).await;
        let data = self.data.read().await;
        let d = data.get(&id).ok_or(PersistenceError::NotFound(id))?;
        Ok(d.clone())
    }

    fn new_heads(&self) -> BoxStream<'_, DocumentRef> {
        self.op_count.fetch_add(1, Ordering::Relaxed);
        let str = BroadcastStream::new(self.heads_tx.subscribe());
        let str = str.filter_map(async |r| r.ok());
        str.boxed()
    }
}

/// The test is run with a variety of different setups.
enum TargetSetup {
    DiskOnly,
    DiskAndServer,
    DiskAndTwoPeers,
}

enum ClockumentConfig {
    RootOnly,
    ShallowNested {
        dependencies: usize,
    },
    DeeplyNested {
        levels: usize,
        branching_factor: usize,
    },
}

#[derive(Hydrate, Reconcile)]
struct DocumentData {
    deps: Vec<DocumentRef>,
}

struct ClockumentDatabase {
    docs: HashMap<SedimentreeId, Automerge>,
}

impl DependencyResolver for ClockumentDatabase {
    fn get_dependencies(&self, tx: &Transaction) -> HashSet<DocumentRef> {
        let data: DocumentData = autosurgeon::hydrate(tx).unwrap();
        data.deps.into_iter().collect()
    }

    fn negligence(&self, clockument_id: SedimentreeId) -> NegligenceDecision {
        NegligenceDecision::Propagate
    }
}

impl ClockumentDatabase {
    fn new() -> Self {
        Self {
            docs: Default::default(),
        }
    }
    fn make(&mut self, dependencies: Vec<DocumentRef>) -> DocumentRef {
        let mut doc = Automerge::new();
        let id = generate_sedimentree_id();
        let mut tx = doc.transaction();
        let _ = autosurgeon::reconcile(&mut tx, DocumentData { deps: dependencies });
        let _ = tx.commit();
        let r = DocumentRef::new(id, doc.get_heads().into());
        self.docs.insert(id, doc);
        r
    }
}

// TODO: do an initial persist
async fn setup_clockument(config: ClockumentConfig) -> (Clockument, Arc<ClockumentDatabase>) {
    let mut db = ClockumentDatabase::new();
    let root = match config {
        ClockumentConfig::RootOnly => db.make(Vec::new()).id(),
        ClockumentConfig::ShallowNested { dependencies } => {
            let mut deps = Vec::new();
            for _ in 0..dependencies {
                deps.push(db.make(Vec::new()));
            }

            db.make(deps).id()
        }
        ClockumentConfig::DeeplyNested {
            levels,
            branching_factor,
        } => {
            fn populate_node(
                level: usize,
                levels: usize,
                branching_factor: usize,
                db: &mut ClockumentDatabase,
            ) -> DocumentRef {
                let mut deps = Vec::new();
                if level < levels {
                    for _ in 0..branching_factor {
                        deps.push(populate_node(level + 1, levels, branching_factor, db));
                    }
                }
                db.make(deps)
            }

            populate_node(0, levels, branching_factor, &mut db).id()
        }
    };

    let db = Arc::new(db);
    (Clockument::new(root, db.clone(), None), db)
}

type TargetSet = Vec<Arc<TransientTarget>>;

fn make_target_set(setup: TargetSetup) -> TargetSet {
    match setup {
        TargetSetup::DiskOnly => vec![TransientTarget::new(Duration::ZERO)],
        TargetSetup::DiskAndServer => vec![
            TransientTarget::new(Duration::ZERO),
            TransientTarget::new(Duration::from_millis(20)),
        ],
        TargetSetup::DiskAndTwoPeers => vec![
            TransientTarget::new(Duration::ZERO),
            TransientTarget::new(Duration::from_millis(20)),
            TransientTarget::new(Duration::from_millis(20)),
        ],
    }
}

async fn add_targets(
    coordinator: &ClockumentCoordinator,
    targets: &TargetSet,
) -> Vec<PersistenceId> {
    let mut out = Vec::new();
    for target in targets {
        out.push(
            coordinator
                .add_persistence(target.clone())
                .await
                .expect("target add failed"),
        );
    }
    out
}

async fn check_synced(targets: &TargetSet, docs: &ClockumentDatabase) {
    for (id, doc) in &docs.docs {
        let doc_ref = DocumentRef::new(*id, doc.get_heads().into());
        for target in targets {
            assert!(
                target.has(&doc_ref).await.expect("has shouldn't fail"),
                "target should have the document"
            );
        }
    }
}

#[derive(Error, Debug)]
enum TestError {
    #[error("workers did not reach quiessence in the time provided")]
    DidNotQuiesce,
}

async fn quiessence_reached(target_set: &TargetSet) -> Result<(), TestError> {
    const POLL_TIME: u64 = 200;
    const TIMEOUT: u64 = 20000;

    let mut time = TIMEOUT;
    loop {
        let ops_before: Vec<usize> = target_set
            .iter()
            .map(|t| t.op_count.load(Ordering::Relaxed))
            .collect();
        tokio::time::sleep(Duration::from_millis(POLL_TIME)).await;
        time -= POLL_TIME;
        let ops_after: Vec<usize> = target_set
            .iter()
            .map(|t| t.op_count.load(Ordering::Relaxed))
            .collect();
        if ops_before == ops_after {
            return Ok(());
        }
        if time <= 0 {
            return Err(TestError::DidNotQuiesce);
        }
    }
}

#[fixture]
fn tracing() {
    use tracing_subscriber::fmt::format::FmtSpan;

    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_span_events(FmtSpan::CLOSE)
        .with_max_level(::tracing::Level::INFO)
        .try_init();
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(ClockumentConfig::RootOnly, TargetSetup::DiskOnly)]
#[case(ClockumentConfig::RootOnly, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::RootOnly, TargetSetup::DiskAndTwoPeers)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskOnly)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndTwoPeers)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskOnly)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndTwoPeers)]
async fn root_clockument_waits_to_persist(
    _tracing: (),
    #[case] config: ClockumentConfig,
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let set = make_target_set(target_setup);
    let (clockument, data) = setup_clockument(config).await;

    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    // Start by inserting the root document
    let cloc = data.docs.get(&clockument.id).unwrap().clone();
    let cloc_ref = DocumentRef::new(clockument.id(), cloc.get_heads().into());
    coordinator.insert(clockument.id, cloc).await?;

    quiessence_reached(&set).await?;

    // No target should have the clockument
    for target in &set {
        if data.docs.len() == 1 {
            assert!(
                target.has(&cloc_ref).await?,
                "the root clockument should be persisted"
            );
        } else {
            assert!(
                !target.has(&cloc_ref).await?,
                "the root clockument should NOT be persisted"
            );
        }
    }

    for (id, doc) in &data.docs {
        coordinator.insert(*id, doc.clone()).await?;
    }

    quiessence_reached(&set).await?;

    check_synced(&set, &data).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(ClockumentConfig::RootOnly, TargetSetup::DiskOnly)]
#[case(ClockumentConfig::RootOnly, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::RootOnly, TargetSetup::DiskAndTwoPeers)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskOnly)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndTwoPeers)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskOnly)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndTwoPeers)]
async fn loads_from_existing_persistence(
    _tracing: (),
    #[case] config: ClockumentConfig,
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let set = make_target_set(target_setup);
    let (clockument, data) = setup_clockument(config).await;

    let target = set[0].clone();

    // seed before creating coordinator
    for (id, doc) in &data.docs {
        target.put(*id, doc.clone()).await?;
    }

    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let ids = add_targets(&coordinator, &set).await;

    // we should be ready at the first persistence already
    let root = data.docs.get(&clockument.id()).unwrap().clone();
    coordinator
        .heads_ready(root.get_heads().into(), ids[0])
        .await?;

    quiessence_reached(&set).await?;

    check_synced(&set, &data).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(ClockumentConfig::RootOnly, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::RootOnly, TargetSetup::DiskAndTwoPeers)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndTwoPeers)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndTwoPeers)]
async fn newly_added_target_catches_up(
    _tracing: (),
    #[case] config: ClockumentConfig,
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(config).await;

    let mut set = make_target_set(target_setup);
    let new_target = set.pop().unwrap();

    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    // Populate the coordinator through the first persistence.
    for (id, doc) in &data.docs {
        coordinator.insert(*id, doc.clone()).await?;
    }

    quiessence_reached(&set).await?;

    for target in &set {
        for (id, doc) in &data.docs {
            let doc_ref = DocumentRef::new(*id, doc.get_heads().into());
            assert!(target.has(&doc_ref).await?);
        }
    }

    set.push(new_target.clone());
    coordinator.add_persistence(new_target.clone()).await?;

    quiessence_reached(&set).await?;

    check_synced(&set, &data).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskOnly)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndTwoPeers)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskOnly)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndTwoPeers)]
async fn heads_ready_waits_for_delayed_persistence(
    _tracing: (),
    #[case] config: ClockumentConfig,
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(config).await;
    let set = make_target_set(target_setup);
    let coordinator = Arc::new(ClockumentCoordinator::new(clockument.clone()));
    let ids = add_targets(&coordinator, &set).await;

    let root = data.docs.get(&clockument.id()).unwrap().clone();
    let root_heads = root.get_heads();

    let join = {
        let coordinator = coordinator.clone();
        let root_heads = root_heads.clone();
        let id = ids[0];
        tokio::task::spawn(async move {
            coordinator
                .heads_ready(root_heads.into(), id)
                .await
                .expect("heads_ready shouldn't have failed");
        })
    };

    tokio::time::sleep(Duration::from_millis(100)).await;
    quiessence_reached(&set).await?;
    assert!(!join.is_finished(), "heads_ready shouldn't have worked");

    coordinator.insert(clockument.id(), root).await?;

    quiessence_reached(&set).await?;
    assert!(!join.is_finished(), "heads_ready shouldn't have worked");

    for (id, doc) in &data.docs {
        coordinator.insert(*id, doc.clone()).await?;
    }

    // try a new one
    coordinator
        .heads_ready(root_heads.clone().into(), ids[0])
        .await?;

    // make sure the old one worked
    join.await.expect("join should've worked");

    check_synced(&set, &data).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(true)]
#[case(false)]
async fn removed_target_causes_heads_ready_to_fail(
    _tracing: (),
    #[case] pend_insertion: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(ClockumentConfig::RootOnly).await;

    let target = TransientTarget::new(Duration::ZERO);
    let coordinator = Arc::new(ClockumentCoordinator::new(clockument.clone()));
    let persistence = coordinator.add_persistence(target.clone()).await?;

    let root = data.docs.get(&clockument.id()).unwrap().clone();
    let heads = root.get_heads();

    if pend_insertion {
        // stop insertion from completing
        target.stick(clockument.id()).await;
        coordinator.insert(clockument.id(), root).await?;
    }

    // execute this immediately
    let waiter_task = {
        let coordinator = coordinator.clone();
        let heads = heads.clone();
        tokio::task::spawn(async move { coordinator.heads_ready(heads.into(), persistence).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;

    coordinator
        .remove_persistence(persistence)
        .await
        .expect("remove failed");

    let res = waiter_task.await.expect("join error");

    assert!(matches!(
        res,
        Err(crate::clockument::persistence::ClockumentError::TargetRemoved)
    ));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(ClockumentConfig::RootOnly, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::RootOnly, TargetSetup::DiskAndTwoPeers)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndTwoPeers)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndTwoPeers)]
async fn negligence_propagates_missing_dependency(
    _tracing: (),
    #[case] config: ClockumentConfig,
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(config).await;
    let set = make_target_set(target_setup);

    let source = set[0].clone();

    // create a negligent source by seeding only the root...
    let root = data.docs.get(&clockument.id()).unwrap().clone();
    source.put(clockument.id(), root.clone()).await?;

    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    quiessence_reached(&set).await?;

    let root_ref = DocumentRef::new(clockument.id(), root.get_heads().into());

    // make sure we propagated the root, even with all the negligent dependencies
    for target in &set {
        assert!(target.has(&root_ref).await?);
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndTwoPeers)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndTwoPeers)]
async fn negligence_can_recover(
    _tracing: (),
    #[case] config: ClockumentConfig,
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(config).await;
    let set = make_target_set(target_setup);

    let source = set[0].clone();

    // create a negligent source by seeding only the root...
    let root = data.docs.get(&clockument.id()).unwrap().clone();
    source.put(clockument.id(), root.clone()).await?;

    // make a source that can recover by seeding all BUT the root
    let source = set[1].clone();
    for (id, doc) in &data.docs {
        if *id == clockument.id {
            continue;
        }
        source.put(*id, doc.clone()).await?;
    }

    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    quiessence_reached(&set).await?;
    tokio::time::sleep(Duration::from_millis(1000)).await;

    check_synced(&set, &data).await;

    Ok(())
}

// TODO: Test that transact for roots and transact_at for dependencies propagates correctly
// TODO: Test that multiple simultaneous transact calls without calling heads_ready
// TODO: Test that our NegligenceDecision works (propagates or does not propagate accordingly)
// and can fully recover if the deps become non-negligent
