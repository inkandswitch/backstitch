use std::{
    collections::{HashMap, HashSet},
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::clockument::persistence::{
    ClockumentCoordinator, ClockumentError, DependencyTree, NegligenceDecision, PersistenceError,
    PersistenceId,
};
use async_trait::async_trait;
use automerge::{
    ActorId, Automerge, ChangeHash, PatchLog, ReadDoc,
    transaction::{Transactable, Transaction},
};
use autosurgeon::{Hydrate, Reconcile};
use futures::{FutureExt, StreamExt, stream::BoxStream};
use indextree::{Arena, NodeEdge};
use rand::Rng;
use rstest::{fixture, rstest};
use sedimentree_core::id::SedimentreeId;
use thiserror::Error;
use tokio::sync::{Barrier, Mutex, RwLock, broadcast, watch};
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

#[derive(Default, Clone, Copy)]
enum FailureMode {
    #[default]
    None,
    Hang,
    Fail,
}

#[derive(Clone)]
struct TransientTarget {
    ping: Duration,
    data: Arc<RwLock<HashMap<SedimentreeId, Automerge>>>,
    stuck: Arc<Mutex<HashSet<SedimentreeId>>>,

    heads_tx: broadcast::Sender<DocumentRef>,

    op_count: Arc<AtomicUsize>,

    stuck_tx: watch::Sender<()>,

    fail_put: Arc<std::sync::Mutex<FailureMode>>,
    fail_get: Arc<std::sync::Mutex<FailureMode>>,
    fail_has: Arc<std::sync::Mutex<FailureMode>>,
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
            fail_put: Default::default(),
            fail_get: Default::default(),
            fail_has: Default::default(),
        })
    }

    fn set_fail_put(&self, mode: FailureMode) {
        let mut v = self.fail_put.lock().unwrap();
        *v = mode;
    }

    fn set_fail_get(&self, mode: FailureMode) {
        let mut v = self.fail_get.lock().unwrap();
        *v = mode;
    }

    fn set_fail_has(&self, mode: FailureMode) {
        let mut v = self.fail_has.lock().unwrap();
        *v = mode;
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

    async fn fail(mode: FailureMode) -> Result<(), PersistenceError> {
        match mode {
            FailureMode::None => {}
            FailureMode::Hang => std::future::pending::<()>().await,
            FailureMode::Fail => return Err(PersistenceError::Other("simulated error".into())),
        }
        Ok(())
    }
}

// TODO: Design a set of tests intended for testing PersistenceTarget methods for expected properties.
#[async_trait]
impl PersistenceTarget for TransientTarget {
    async fn put(&self, id: SedimentreeId, doc: Automerge) -> Result<(), PersistenceError> {
        let mode = self.fail_put.lock().unwrap().clone();
        Self::fail(mode).await?;

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
        let mode = self.fail_has.lock().unwrap().clone();
        Self::fail(mode).await?;

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
        let mode = self.fail_get.lock().unwrap().clone();
        Self::fail(mode).await?;

        self.op_count.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(self.ping).await;
        self.stuck_guard(id).await;
        let data = self.data.read().await;
        let d = data.get(&id).ok_or(PersistenceError::NotFound(id))?;
        Ok(d.clone().with_actor(ActorId::random()))
    }

    fn new_heads(&self) -> BoxStream<'static, DocumentRef> {
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
    data: u32,
}

impl DocumentData {
    fn touch(tx: &mut Transaction) {
        let mut this: Self = autosurgeon::hydrate(tx).expect("hydrate failure");
        this.data += 1;
        autosurgeon::reconcile(tx, this).expect("reconcile failure");
    }
}

struct ClockumentDatabase {
    docs: HashMap<SedimentreeId, Automerge>,
    decision: NegligenceDecision,
}

impl DependencyResolver for ClockumentDatabase {
    fn get_dependencies(&self, tx: &Transaction) -> HashSet<DocumentRef> {
        let data: DocumentData = autosurgeon::hydrate(tx).unwrap();
        data.deps.into_iter().collect()
    }

    fn negligence(&self, clockument_id: SedimentreeId) -> NegligenceDecision {
        self.decision
    }
}

impl ClockumentDatabase {
    fn new(decision: NegligenceDecision) -> Self {
        Self {
            docs: Default::default(),
            decision,
        }
    }
    fn make(&mut self, dependencies: Vec<DocumentRef>) -> DocumentRef {
        let mut doc = Automerge::new();
        let id = generate_sedimentree_id();
        let mut tx = doc.transaction();
        let _ = autosurgeon::reconcile(
            &mut tx,
            DocumentData {
                data: 0,
                deps: dependencies,
            },
        );
        let _ = tx.commit();
        let r = DocumentRef::new(id, doc.get_heads().into());
        self.docs.insert(id, doc);
        r
    }
}

async fn setup_clockument(config: ClockumentConfig) -> (Clockument, Arc<ClockumentDatabase>) {
    let mut db = ClockumentDatabase::new(NegligenceDecision::Propagate);
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

async fn all_have(targets: &TargetSet, doc_ref: &DocumentRef) -> bool {
    for t in targets {
        if !t.has(doc_ref).await.expect("has shouldn't fail") {
            return false;
        }
    }
    true
}

async fn none_have(targets: &TargetSet, doc_ref: &DocumentRef) -> bool {
    for t in targets {
        if t.has(doc_ref).await.expect("has shouldn't fail") {
            return false;
        }
    }
    true
}

fn ref_of(doc: &Automerge, id: SedimentreeId) -> DocumentRef {
    DocumentRef::new(id, doc.get_heads().into())
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

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(false)]
#[case(true)]
async fn transact_persists_changes(
    _tracing: (),
    #[case] use_transact_at: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(ClockumentConfig::RootOnly).await;
    let target = TransientTarget::new(Duration::ZERO);
    let set: TargetSet = vec![target.clone()];
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let ids = add_targets(&coordinator, &set).await;

    let root = data.docs.get(&clockument.id()).unwrap().clone();
    let before = root.get_heads();
    coordinator.insert(clockument.id(), root).await?;
    coordinator
        .heads_ready(before.clone().into(), ids[0])
        .await?;

    if use_transact_at {
        let doc_ref = DocumentRef::new(clockument.id(), before.clone().into());
        coordinator
            .transact_at(&doc_ref, ids[0], async |mut tx: Transaction| {
                DocumentData::touch(&mut tx);
                tx.commit();
            })
            .await?;
    } else {
        coordinator
            .transact(ids[0], async |mut tx: Transaction| {
                DocumentData::touch(&mut tx);
                tx.commit();
            })
            .await?;
    }

    quiessence_reached(&set).await?;

    let after = target.get(clockument.id()).await?.get_heads();
    assert_ne!(before, after, "transaction changes weren't persisted");

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
async fn multidoc_transact_propagates(
    _tracing: (),
    #[case] config: ClockumentConfig,
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let set = make_target_set(target_setup);
    let (clockument, data) = setup_clockument(config).await;

    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let ids = add_targets(&coordinator, &set).await;

    // Start by inserting the root document
    let cloc = data.docs.get(&clockument.id).unwrap().clone();
    coordinator.insert(clockument.id, cloc).await?;

    // Then, all of its dependencies
    for (id, doc) in &data.docs {
        coordinator.insert(*id, doc.clone()).await?;
    }

    quiessence_reached(&set).await?;

    check_synced(&set, &data).await;

    // Build a tree of dependencies based on the inserted stuff
    let mut tree = Arena::new();
    let mut new_heads: HashMap<SedimentreeId, Heads> = HashMap::new();
    let base_heads = coordinator
        .transact(ids[0], async |tx: Transaction| tx.get_heads())
        .await
        .expect("transaction failed");

    let root = tree.new_node(DocumentRef::new(clockument.id(), base_heads.into()));
    let mut frontier = Vec::new();
    frontier.push(root);
    while let Some(node) = frontier.pop() {
        let dref = tree.get_data(node).unwrap();
        let refs = coordinator
            .transact_at(dref, ids[0], async |tx: Transaction| {
                let data: DocumentData = autosurgeon::hydrate(&tx).unwrap();
                data.deps
            })
            .await
            .expect("transaction failed");
        for r in refs {
            frontier.push(node.append_value(r, &mut tree));
        }
    }
    let mut edge = Some(NodeEdge::Start(root));
    while let Some(current) = edge {
        edge = current.next_traverse(&tree);

        let NodeEdge::End(node) = current else {
            continue;
        };

        let dref = tree.get_data_mut(node).unwrap();
        let new_head = coordinator
            .transact_at(&dref, ids[0], async |mut tx: Transaction| {
                let mut data: DocumentData = autosurgeon::hydrate(&tx).unwrap();
                data.data += 1;
                for dep in &mut data.deps {
                    *dep = DocumentRef::new(
                        dep.id(),
                        new_heads
                            .get(&dep.id())
                            .expect("should be in new_heads")
                            .clone(),
                    );
                }
                autosurgeon::reconcile(&mut tx, data).expect("reconcile failed");
                vec![tx.commit().0.expect("there'd better be new heads")]
            })
            .await
            .expect("transaction failed");
        new_heads.insert(dref.id(), Heads::from(new_head.clone()));
        let new_ref = DocumentRef::new(dref.id(), new_head.into());
        *dref = new_ref;

        // this is pretty slow...
        quiessence_reached(&set).await?;

        for (id, doc) in &data.docs {
            let (heads, expected_data) = if let Some(h) = new_heads.get(id) {
                (h.clone(), 1)
            } else {
                (doc.get_heads().into(), 0)
            };

            for target in &set {
                let doc = target.get_fast(*id).await.expect("get failed");
                assert!(heads == Heads::from(doc.get_heads()), "unexpected heads");
                let data: DocumentData = autosurgeon::hydrate(&doc).unwrap();
                assert!(data.data == expected_data, "unexpected data");
            }
        }
    }

    Ok(())
}

fn make_doc() -> Automerge {
    let mut doc = Automerge::new();
    let mut tx = doc.transaction();
    tx.put(automerge::ROOT, "k", 1_i64).expect("put failed");
    let _ = tx.commit();
    doc
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
async fn transact_at_unknown_heads_returns_error(
    _tracing: (),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(ClockumentConfig::RootOnly).await;
    let target = TransientTarget::new(Duration::ZERO);
    let set: TargetSet = vec![target.clone()];
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let ids = add_targets(&coordinator, &set).await;

    let root = data.docs.get(&clockument.id()).unwrap().clone();
    let heads = root.get_heads();
    coordinator.insert(clockument.id(), root).await?;
    coordinator.heads_ready(heads.into(), ids[0]).await?;

    let bogus = DocumentRef::new(clockument.id(), Heads::from(vec![ChangeHash([0x69; 32])]));
    let res = coordinator
        .transact_at(&bogus, ids[0], async |_tx: Transaction| {})
        .await;

    assert!(matches!(res, Err(ClockumentError::NoSuchHeads(_))));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
async fn root_waits_for_dependency_at_exact_heads(
    _tracing: (),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut db = ClockumentDatabase::new(NegligenceDecision::Propagate);
    let dep_1 = db.make(Vec::new());
    let dep_doc_1 = db.docs.get(&dep_1.id()).unwrap().clone();

    // do some more-recent changes
    let mut dep_doc_2 = dep_doc_1.clone();
    let mut tx = dep_doc_2.transaction();
    DocumentData::touch(&mut tx);
    tx.commit();

    let dep_2 = DocumentRef::new(dep_1.id(), dep_doc_2.get_heads().into());

    let root = db.make(vec![dep_2.clone()]);
    let root_doc = db.docs.get(&root.id()).unwrap().clone();

    // target knows about dep_1, but not the updated dep_2
    let target = TransientTarget::new(Duration::ZERO);
    target.put(dep_1.id(), dep_doc_1).await?;

    let set: TargetSet = vec![target.clone()];
    let clockument = Clockument::new(root.id(), Arc::new(db), None);
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    // root knows about dep_2
    coordinator.insert(root.id(), root_doc).await?;
    quiessence_reached(&set).await?;

    assert!(
        !target.has(&root).await?,
        "root persisted even though the heads were too new"
    );

    // now we insert the updated heads
    coordinator.insert(dep_1.id(), dep_doc_2).await?;
    quiessence_reached(&set).await?;

    assert!(target.has(&dep_2).await?);
    assert!(target.has(&root).await?, "root didn't persist");

    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
enum Op {
    Put,
    Get,
    Has,
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(FailureMode::Fail, vec![Op::Put])]
#[case(FailureMode::Hang, vec![Op::Put])]
#[case(FailureMode::Fail, vec![Op::Get])]
#[case(FailureMode::Hang, vec![Op::Get])]
#[case(FailureMode::Fail, vec![Op::Has])]
#[case(FailureMode::Hang, vec![Op::Has])]
#[case(FailureMode::Fail, vec![Op::Put, Op::Get, Op::Has])]
#[case(FailureMode::Hang, vec![Op::Put, Op::Get, Op::Has])]
async fn remove_target_is_not_blocked(
    _tracing: (),
    #[case] mode: FailureMode,
    #[case] ops: Vec<Op>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, _data) = setup_clockument(ClockumentConfig::RootOnly).await;
    let target = TransientTarget::new(Duration::ZERO);

    let coordinator = Arc::new(ClockumentCoordinator::new(clockument.clone()));
    let persistence = coordinator.add_persistence(target.clone()).await?;

    tokio::time::sleep(Duration::from_millis(100)).await;
    if ops.contains(&Op::Get) {
        target.set_fail_get(mode);
    }
    if ops.contains(&Op::Put) {
        target.set_fail_put(mode);
    }
    if ops.contains(&Op::Has) {
        target.set_fail_has(mode);
    }

    {
        let coordinator = coordinator.clone();
        tokio::spawn(async move {
            let _ = coordinator
                .insert(generate_sedimentree_id(), make_doc())
                .await;
        });
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    let res = tokio::time::timeout(
        Duration::from_secs(2),
        coordinator.remove_persistence(persistence),
    )
    .await;
    assert!(res.is_ok(), "remove_persistence hung");
    assert!(res.unwrap().is_ok(), "remove_persistence failed");

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
async fn heads_ready_fails_on_bad_put(
    _tracing: (),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(ClockumentConfig::RootOnly).await;
    let target = TransientTarget::new(Duration::ZERO);

    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let persistence = coordinator.add_persistence(target.clone()).await?;

    target.set_fail_put(FailureMode::Fail);

    let root = data.docs.get(&clockument.id()).unwrap().clone();
    let heads = root.get_heads();
    coordinator.insert(clockument.id(), root).await?;

    let res = tokio::time::timeout(
        Duration::from_secs(3),
        coordinator.heads_ready(heads.into(), persistence),
    )
    .await;

    match res {
        Err(_) => panic!("heads_ready hung forever"),
        Ok(res) => assert!(res.is_err(), "heads_ready shouldn't succeed"),
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
async fn heads_ready_fails_on_target_removed(
    _tracing: (),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(ClockumentConfig::RootOnly).await;
    let target = TransientTarget::new(Duration::ZERO);

    let coordinator = Arc::new(ClockumentCoordinator::new(clockument.clone()));
    let persistence = coordinator.add_persistence(target.clone()).await?;

    target.set_fail_put(FailureMode::Hang);

    let root = data.docs.get(&clockument.id()).unwrap().clone();
    let heads = root.get_heads();
    coordinator.insert(clockument.id(), root).await?;

    let res = {
        let coordinator = coordinator.clone();
        tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(3),
                coordinator.heads_ready(heads.into(), persistence),
            )
            .await
        })
    };

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(!res.is_finished(), "heads_ready should still be hanging");

    coordinator.remove_persistence(persistence).await?;
    let res = res.await;
    assert!(matches!(res??, Err(ClockumentError::TargetRemoved)));
    Ok(())
}

// TODO: Test multiple simultaneous transact calls without calling heads_ready should create diverging heads
// TODO: Test other forms of negligence recovery
// TODO: Test that a PendingInsertion is canceled when a target is removed, or manually on a direct insert

/// Heads of every known document on every target, so tests can assert "nothing changed".
async fn head_snapshot(
    targets: &TargetSet,
    data: &ClockumentDatabase,
) -> HashMap<(usize, SedimentreeId), Heads> {
    let mut out = HashMap::new();
    for (i, target) in targets.iter().enumerate() {
        for id in data.docs.keys() {
            let doc = target.get_fast(*id).await.expect("get failed");
            out.insert((i, *id), Heads::from(doc.get_heads()));
        }
    }
    out
}

/// Transacts on the root, but only commits once `barrier` is released, so that two of these
/// are guaranteed to have read the same persisted heads before either writes.
async fn barrier_touch(
    coordinator: &ClockumentCoordinator,
    persistence: PersistenceId,
    barrier: Arc<Barrier>,
) -> ChangeHash {
    coordinator
        .transact(persistence, async move |mut tx: Transaction| {
            barrier.wait().await;
            DocumentData::touch(&mut tx);
            tx.commit().0.expect("there'd better be new heads")
        })
        .await
        .expect("transact failed")
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(TargetSetup::DiskOnly)]
#[case(TargetSetup::DiskAndServer)]
#[case(TargetSetup::DiskAndTwoPeers)]
async fn standalone_document_persists_without_root(
    _tracing: (),
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let set = make_target_set(target_setup);
    let (clockument, data) =
        setup_clockument(ClockumentConfig::ShallowNested { dependencies: 5 }).await;
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    let id = generate_sedimentree_id();
    let doc = make_doc();
    let doc_ref = DocumentRef::new(id, doc.get_heads().into());
    coordinator.insert(id, doc).await?;

    quiessence_reached(&set).await?;

    assert!(
        all_have(&set, &doc_ref).await,
        "standalone doc should reach every target"
    );
    let root_ref = ref_of(data.docs.get(&clockument.id()).unwrap(), clockument.id());
    assert!(
        none_have(&set, &root_ref).await,
        "the root was never inserted, so it must not appear"
    );
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
async fn dependencies_first_then_root_persists(
    _tracing: (),
    #[case] config: ClockumentConfig,
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let set = make_target_set(target_setup);
    let (clockument, data) = setup_clockument(config).await;
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    for (id, doc) in &data.docs {
        if *id == clockument.id() {
            continue;
        }
        coordinator.insert(*id, doc.clone()).await?;
    }
    quiessence_reached(&set).await?;

    let root = data.docs.get(&clockument.id()).unwrap().clone();
    let root_ref = ref_of(&root, clockument.id());
    assert!(none_have(&set, &root_ref).await);

    coordinator.insert(clockument.id(), root).await?;
    quiessence_reached(&set).await?;

    check_synced(&set, &data).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(ClockumentConfig::RootOnly, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndServer)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, TargetSetup::DiskAndTwoPeers)]
#[case(ClockumentConfig::DeeplyNested { levels: 5, branching_factor: 2 }, TargetSetup::DiskAndTwoPeers)]
async fn reinserting_same_data_is_idempotent(
    _tracing: (),
    #[case] config: ClockumentConfig,
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let set = make_target_set(target_setup);
    let (clockument, data) = setup_clockument(config).await;
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    for (id, doc) in &data.docs {
        coordinator.insert(*id, doc.clone()).await?;
    }
    quiessence_reached(&set).await?;
    check_synced(&set, &data).await;
    let before = head_snapshot(&set, &data).await;

    for _ in 0..2 {
        for (id, doc) in &data.docs {
            coordinator.insert(*id, doc.clone()).await?;
        }
    }
    quiessence_reached(&set).await?;

    check_synced(&set, &data).await;
    assert_eq!(
        before,
        head_snapshot(&set, &data).await,
        "re-inserting identical data changed heads"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, true)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, false)]
#[case(ClockumentConfig::DeeplyNested { levels: 4, branching_factor: 2 }, true)]
#[case(ClockumentConfig::DeeplyNested { levels: 4, branching_factor: 2 }, false)]
async fn external_puts_propagate_to_other_targets(
    _tracing: (),
    #[case] config: ClockumentConfig,
    #[case] root_first: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let set = make_target_set(TargetSetup::DiskAndTwoPeers);
    let (clockument, data) = setup_clockument(config).await;
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    let source = set[0].clone();
    let root = data.docs.get(&clockument.id()).unwrap().clone();

    if root_first {
        source.put(clockument.id(), root.clone()).await?;
    }
    for (id, doc) in &data.docs {
        if *id == clockument.id() {
            continue;
        }
        source.put(*id, doc.clone()).await?;
    }
    if !root_first {
        source.put(clockument.id(), root).await?;
    }

    quiessence_reached(&set).await?;
    check_synced(&set, &data).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
async fn cancelled_pending_insertion_is_dropped(
    _tracing: (),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) =
        setup_clockument(ClockumentConfig::ShallowNested { dependencies: 5 }).await;
    let target = TransientTarget::new(Duration::ZERO);
    let set: TargetSet = vec![target.clone()];
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    let root = data.docs.get(&clockument.id()).unwrap().clone();
    let root_ref = ref_of(&root, clockument.id());

    let token = coordinator.insert(clockument.id(), root.clone()).await?;
    quiessence_reached(&set).await?;
    assert!(!target.has(&root_ref).await?, "root should be pending");

    token.cancel();

    // Persist a single dependency; 4 remain missing, so the pending root can't complete,
    // sees its token cancelled, and is discarded.
    let deps: Vec<_> = data
        .docs
        .iter()
        .filter(|(id, _)| **id != clockument.id())
        .collect();
    coordinator.insert(*deps[0].0, deps[0].1.clone()).await?;
    quiessence_reached(&set).await?;

    for (id, doc) in &deps[1..] {
        coordinator.insert(**id, (*doc).clone()).await?;
    }
    quiessence_reached(&set).await?;

    for (id, doc) in &deps {
        assert!(target.has(&ref_of(doc, **id)).await?, "deps should persist");
    }
    assert!(
        !target.has(&root_ref).await?,
        "a cancelled pending root must not persist when its dependencies later arrive"
    );

    // A fresh insertion of the same root still works.
    coordinator.insert(clockument.id(), root).await?;
    quiessence_reached(&set).await?;
    assert!(target.has(&root_ref).await?);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
async fn multiple_pending_root_versions_resolve_independently(
    _tracing: (),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut db = ClockumentDatabase::new(NegligenceDecision::Propagate);
    let dep_a = db.make(Vec::new());
    let dep_b = db.make(Vec::new());
    let dep_a_doc = db.docs.get(&dep_a.id()).unwrap().clone();
    let dep_b_doc = db.docs.get(&dep_b.id()).unwrap().clone();

    // v1 depends on A. v2 (a descendant of v1) depends on A and B.
    let root_1 = db.make(vec![dep_a.clone()]);
    let root_doc_1 = db.docs.get(&root_1.id()).unwrap().clone();
    let mut root_doc_2 = root_doc_1.clone();
    {
        let mut tx = root_doc_2.transaction();
        let mut d: DocumentData = autosurgeon::hydrate(&tx).unwrap();
        d.deps.push(dep_b.clone());
        autosurgeon::reconcile(&mut tx, d).unwrap();
        tx.commit();
    }
    let root_ref_1 = DocumentRef::new(root_1.id(), root_doc_1.get_heads().into());
    let root_ref_2 = DocumentRef::new(root_1.id(), root_doc_2.get_heads().into());

    let target = TransientTarget::new(Duration::ZERO);
    let set: TargetSet = vec![target.clone()];
    let clockument = Clockument::new(root_1.id(), Arc::new(db), None);
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    // newer version first
    coordinator.insert(root_1.id(), root_doc_2).await?;
    coordinator.insert(root_1.id(), root_doc_1).await?;
    quiessence_reached(&set).await?;
    assert!(!target.has(&root_ref_1).await?);
    assert!(!target.has(&root_ref_2).await?);

    coordinator.insert(dep_a.id(), dep_a_doc).await?;
    quiessence_reached(&set).await?;
    assert!(target.has(&root_ref_1).await?, "v1 only needed dep A");
    assert!(
        !target.has(&root_ref_2).await?,
        "v2 still needs dep B and must keep waiting"
    );

    coordinator.insert(dep_b.id(), dep_b_doc).await?;
    quiessence_reached(&set).await?;
    assert!(target.has(&root_ref_2).await?, "v2 should now persist");
    assert!(target.has(&root_ref_1).await?);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
async fn root_accepts_dependency_at_ancestor_heads(
    _tracing: (),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut db = ClockumentDatabase::new(NegligenceDecision::Propagate);
    let dep_old = db.make(Vec::new());
    let dep_doc_old = db.docs.get(&dep_old.id()).unwrap().clone();

    let mut dep_doc_new = dep_doc_old.clone();
    {
        let mut tx = dep_doc_new.transaction();
        DocumentData::touch(&mut tx);
        tx.commit();
    }
    assert_ne!(dep_doc_old.get_heads(), dep_doc_new.get_heads());

    // The root pins the OLD heads.
    let root = db.make(vec![dep_old.clone()]);
    let root_doc = db.docs.get(&root.id()).unwrap().clone();

    // The target only knows the newer version of the dependency.
    let target = TransientTarget::new(Duration::ZERO);
    target.put(dep_old.id(), dep_doc_new).await?;

    let set: TargetSet = vec![target.clone()];
    let clockument = Clockument::new(root.id(), Arc::new(db), None);
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    coordinator.insert(root.id(), root_doc).await?;
    quiessence_reached(&set).await?;

    assert!(target.has(&dep_old).await?);
    assert!(
        target.has(&root).await?,
        "root pinned to ancestor heads should persist against a newer dependency"
    );
    Ok(())
}

/// Diamond-shaped graphs are weird and unsupported; current behavior is to just resolve one branch and never shape the diamond.
/// This tests the basic "still block sync pls" behavior.
#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(TargetSetup::DiskOnly)]
#[case(TargetSetup::DiskAndServer)]
async fn shared_dependency_blocks_root_until_present(
    _tracing: (),
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut db = ClockumentDatabase::new(NegligenceDecision::Propagate);
    let shared = db.make(Vec::new());
    let left = db.make(vec![shared.clone()]);
    let right = db.make(vec![shared.clone()]);
    let root = db.make(vec![left.clone(), right.clone()]);
    let db = Arc::new(db);
    let clockument = Clockument::new(root.id(), db.clone(), None);

    let set = make_target_set(target_setup);
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    for r in [&root, &left, &right] {
        coordinator
            .insert(r.id(), db.docs.get(&r.id()).unwrap().clone())
            .await?;
    }
    quiessence_reached(&set).await?;
    assert!(
        none_have(&set, &root).await,
        "root must wait for the shared dependency"
    );

    coordinator
        .insert(shared.id(), db.docs.get(&shared.id()).unwrap().clone())
        .await?;
    quiessence_reached(&set).await?;

    check_synced(&set, &db).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(None, false)]
#[case(Some(0), true)]
#[case(Some(1), false)]
async fn search_depth_limits_dependency_expansion(
    _tracing: (),
    #[case] depth: Option<usize>,
    #[case] root_persists_without_grandchild: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut db = ClockumentDatabase::new(NegligenceDecision::Propagate);
    let grandchild = db.make(Vec::new());
    let child = db.make(vec![grandchild.clone()]);
    let root = db.make(vec![child.clone()]);
    let root_doc = db.docs.get(&root.id()).unwrap().clone();
    let child_doc = db.docs.get(&child.id()).unwrap().clone();
    let grandchild_doc = db.docs.get(&grandchild.id()).unwrap().clone();

    let target = TransientTarget::new(Duration::ZERO);
    target.put(child.id(), child_doc).await?;

    let set: TargetSet = vec![target.clone()];
    let clockument = Clockument::new(root.id(), Arc::new(db), depth);
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    coordinator.insert(root.id(), root_doc).await?;
    quiessence_reached(&set).await?;
    assert_eq!(
        target.has(&root).await?,
        root_persists_without_grandchild,
        "unexpected root persistence with depth {depth:?}"
    );

    // Whatever the depth, supplying the grandchild must leave everything persisted.
    coordinator.insert(grandchild.id(), grandchild_doc).await?;
    quiessence_reached(&set).await?;
    assert!(target.has(&grandchild).await?);
    assert!(target.has(&child).await?);
    assert!(target.has(&root).await?);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, true)]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 }, false)]
#[case(ClockumentConfig::DeeplyNested { levels: 4, branching_factor: 2 }, true)]
#[case(ClockumentConfig::DeeplyNested { levels: 4, branching_factor: 2 }, false)]
async fn do_not_propagate_blocks_negligent_source(
    _tracing: (),
    #[case] config: ClockumentConfig,
    #[case] seed_before_coordinator: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(config).await;
    let clockument = Clockument::new(
        clockument.id(),
        Arc::new(ClockumentDatabase::new(NegligenceDecision::DoNotPropagate)),
        None,
    );
    let set = make_target_set(TargetSetup::DiskAndServer);
    let source = set[0].clone();
    let other = set[1].clone();
    let root = data.docs.get(&clockument.id()).unwrap().clone();

    let coordinator = ClockumentCoordinator::new(clockument.clone());
    if seed_before_coordinator {
        source.put(clockument.id(), root.clone()).await?;
        let _ = add_targets(&coordinator, &set).await;
    } else {
        let _ = add_targets(&coordinator, &set).await;
        source.put(clockument.id(), root.clone()).await?;
    }

    quiessence_reached(&set).await?;

    for (id, doc) in &data.docs {
        assert!(
            !other.has(&ref_of(doc, *id)).await?,
            "negligent source leaked a document to another target"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 })]
#[case(ClockumentConfig::DeeplyNested { levels: 4, branching_factor: 2 })]
async fn do_not_propagate_allows_healthy_source(
    _tracing: (),
    #[case] config: ClockumentConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(config).await;
    let clockument = Clockument::new(
        clockument.id(),
        Arc::new(ClockumentDatabase::new(NegligenceDecision::DoNotPropagate)),
        None,
    );
    let set = make_target_set(TargetSetup::DiskAndTwoPeers);

    for (id, doc) in &data.docs {
        set[0].put(*id, doc.clone()).await?;
    }
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    quiessence_reached(&set).await?;
    check_synced(&set, &data).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(ClockumentConfig::ShallowNested { dependencies: 5 })]
#[case(ClockumentConfig::DeeplyNested { levels: 4, branching_factor: 2 })]
async fn failing_put_does_not_block_other_targets_and_recovers(
    _tracing: (),
    #[case] config: ClockumentConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let set = make_target_set(TargetSetup::DiskAndServer);
    let healthy = set[0].clone();
    let flaky = set[1].clone();
    let (clockument, data) = setup_clockument(config).await;
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let _ = add_targets(&coordinator, &set).await;

    flaky.set_fail_put(FailureMode::Fail);
    for (id, doc) in &data.docs {
        coordinator.insert(*id, doc.clone()).await?;
    }
    quiessence_reached(&set).await?;

    check_synced(&vec![healthy.clone()], &data).await;
    let root_ref = ref_of(data.docs.get(&clockument.id()).unwrap(), clockument.id());
    assert!(
        !flaky.has(&root_ref).await?,
        "root can't be on a target that couldn't store its dependencies"
    );

    flaky.set_fail_put(FailureMode::None);
    for (id, doc) in &data.docs {
        coordinator.insert(*id, doc.clone()).await?;
    }
    quiessence_reached(&set).await?;

    check_synced(&set, &data).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
async fn removed_target_receives_no_further_writes(
    _tracing: (),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let set = make_target_set(TargetSetup::DiskAndServer);
    let (clockument, data) =
        setup_clockument(ClockumentConfig::ShallowNested { dependencies: 5 }).await;
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let ids = add_targets(&coordinator, &set).await;

    coordinator.remove_persistence(ids[1]).await?;

    for (id, doc) in &data.docs {
        coordinator.insert(*id, doc.clone()).await?;
    }
    quiessence_reached(&set).await?;

    check_synced(&vec![set[0].clone()], &data).await;
    for (id, doc) in &data.docs {
        assert!(
            !set[1].has(&ref_of(doc, *id)).await?,
            "removed target was still written to"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
async fn unknown_persistence_id_is_rejected(
    _tracing: (),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(ClockumentConfig::RootOnly).await;
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let bogus = PersistenceId(9999);

    let root = data.docs.get(&clockument.id()).unwrap().clone();
    let root_ref = ref_of(&root, clockument.id());

    let res = coordinator
        .transact(bogus, async |_tx: Transaction| {})
        .await;
    assert!(matches!(res, Err(ClockumentError::NoSuchTarget(_))));

    let res = coordinator
        .transact_at(&root_ref, bogus, async |_tx: Transaction| {})
        .await;
    assert!(matches!(res, Err(ClockumentError::NoSuchTarget(_))));

    let res = coordinator
        .heads_ready(root.get_heads().into(), bogus)
        .await;
    assert!(matches!(res, Err(ClockumentError::NoSuchTarget(_))));

    let res = coordinator.remove_persistence(bogus).await;
    assert!(matches!(res, Err(ClockumentError::NoSuchTarget(_))));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
async fn removing_a_target_twice_fails_the_second_time(
    _tracing: (),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, _data) = setup_clockument(ClockumentConfig::RootOnly).await;
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let id = coordinator
        .add_persistence(TransientTarget::new(Duration::ZERO))
        .await?;

    coordinator.remove_persistence(id).await?;
    let res = coordinator.remove_persistence(id).await;
    assert!(matches!(res, Err(ClockumentError::NoSuchTarget(_))));
    Ok(())
}

/// Transacting before the root exists anywhere on the target is `NoSuchDocument`, not a panic.
#[tokio::test(flavor = "multi_thread")]
#[rstest]
async fn transact_before_root_exists_returns_no_such_document(
    _tracing: (),
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(ClockumentConfig::RootOnly).await;
    let target = TransientTarget::new(Duration::ZERO);
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let id = coordinator.add_persistence(target.clone()).await?;

    let res = coordinator.transact(id, async |_tx: Transaction| {}).await;
    assert!(matches!(res, Err(ClockumentError::NoSuchDocument(_))));

    let root_ref = ref_of(data.docs.get(&clockument.id()).unwrap(), clockument.id());
    let res = coordinator
        .transact_at(&root_ref, id, async |_tx: Transaction| {})
        .await;
    assert!(matches!(res, Err(ClockumentError::NoSuchDocument(_))));

    let missing = DocumentRef::new(generate_sedimentree_id(), root_ref.heads().clone());
    let res = coordinator
        .transact_at(&missing, id, async |_tx: Transaction| {})
        .await;
    assert!(matches!(res, Err(ClockumentError::NoSuchDocument(_))));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(false)]
#[case(true)]
async fn transact_without_changes_persists_nothing(
    _tracing: (),
    #[case] use_transact_at: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (clockument, data) = setup_clockument(ClockumentConfig::RootOnly).await;
    let target = TransientTarget::new(Duration::ZERO);
    let set: TargetSet = vec![target.clone()];
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let ids = add_targets(&coordinator, &set).await;

    let root = data.docs.get(&clockument.id()).unwrap().clone();
    let before = root.get_heads();
    coordinator.insert(clockument.id(), root).await?;
    coordinator
        .heads_ready(before.clone().into(), ids[0])
        .await?;

    if use_transact_at {
        let doc_ref = DocumentRef::new(clockument.id(), before.clone().into());
        coordinator
            .transact_at(&doc_ref, ids[0], async |tx: Transaction| {
                let _ = tx.get_heads();
            })
            .await?;
        coordinator
            .transact_at(&doc_ref, ids[0], async |mut tx: Transaction| {
                DocumentData::touch(&mut tx);
                tx.rollback();
            })
            .await?;
    } else {
        coordinator
            .transact(ids[0], async |tx: Transaction| {
                let _ = tx.get_heads();
            })
            .await?;
        coordinator
            .transact(ids[0], async |mut tx: Transaction| {
                DocumentData::touch(&mut tx);
                tx.rollback();
            })
            .await?;
    }

    quiessence_reached(&set).await?;

    let after = target.get(clockument.id()).await?.get_heads();
    assert_eq!(before, after, "a no-op transaction changed the document");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case(TargetSetup::DiskOnly)]
#[case(TargetSetup::DiskAndServer)]
async fn concurrent_transacts_diverge_then_merge(
    _tracing: (),
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::sync::Barrier;

    let set = make_target_set(target_setup);
    let (clockument, data) = setup_clockument(ClockumentConfig::RootOnly).await;
    let coordinator = ClockumentCoordinator::new(clockument.clone());
    let ids = add_targets(&coordinator, &set).await;

    let root = data.docs.get(&clockument.id()).unwrap().clone();
    let base = root.get_heads();
    coordinator.insert(clockument.id(), root).await?;
    coordinator.heads_ready(base.into(), ids[0]).await?;

    let barrier = Arc::new(Barrier::new(2));
    let (h1, h2) = tokio::join!(
        barrier_touch(&coordinator, ids[0], barrier.clone()),
        barrier_touch(&coordinator, ids[0], barrier.clone()),
    );
    assert_ne!(h1, h2, "concurrent changes should be distinct");

    quiessence_reached(&set).await?;

    for target in &set {
        let doc = target.get_fast(clockument.id()).await?;
        assert_eq!(
            doc.get_heads().len(),
            2,
            "both concurrent branches should be present as heads"
        );
        for h in [h1, h2] {
            let r = DocumentRef::new(clockument.id(), Heads::from(vec![h]));
            assert!(target.has(&r).await?, "branch {h:?} was lost in the merge");
        }
    }

    // Each branch's own heads must be reported ready.
    for h in [h1, h2] {
        coordinator
            .heads_ready(Heads::from(vec![h]), ids[0])
            .await?;
    }
    Ok(())
}
