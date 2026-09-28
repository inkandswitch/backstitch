use std::{
    collections::{HashMap, HashSet},
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::clockument::persistence::ClockumentCoordinator;
use async_trait::async_trait;
use automerge::{Automerge, ChangeHash, PatchLog, ReadDoc, transaction::Transaction};
use autosurgeon::{Hydrate, Reconcile};
use futures::{StreamExt, stream::BoxStream};
use rand::Rng;
use rstest::rstest;
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

#[derive(Error, Debug)]
enum TransientTargetError {
    #[error("the document was not found")]
    DocumentNotFound,
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

    async fn get_fast(&self, id: SedimentreeId) -> Result<Automerge, Box<dyn Error + Send + Sync>> {
        self.op_count.fetch_add(1, Ordering::Relaxed);
        self.stuck_guard(id).await;
        let data = self.data.read().await;
        let d = data
            .get(&id)
            .ok_or(TransientTargetError::DocumentNotFound)?;
        Ok(d.clone())
    }
}

// TODO: Design a set of tests intended for testing PersistenceTarget methods for expected properties.
#[async_trait]
impl PersistenceTarget for TransientTarget {
    async fn put(
        &self,
        id: SedimentreeId,
        doc: Automerge,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
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

    async fn has(&self, doc_ref: &DocumentRef) -> Result<bool, Box<dyn Error + Send + Sync>> {
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

    async fn get(&self, id: SedimentreeId) -> Result<Automerge, Box<dyn Error + Send + Sync>> {
        self.op_count.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(self.ping).await;
        self.stuck_guard(id).await;
        let data = self.data.read().await;
        let d = data
            .get(&id)
            .ok_or(TransientTargetError::DocumentNotFound)?;
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

#[async_trait]
impl DependencyResolver for ClockumentDatabase {
    #[tracing::instrument(skip_all)]
    async fn get_dependencies(
        &self,
        tx: &Transaction,
        target: Arc<dyn PersistenceTarget>,
    ) -> HashSet<DocumentRef> {
        let mut deps = HashSet::new();
        let mut stack = Vec::new();

        let data: DocumentData = autosurgeon::hydrate(tx).unwrap();
        stack.extend(data.deps);

        let mut elapsed_get = tokio::time::Duration::ZERO;
        let mut elapsed_transact = tokio::time::Duration::ZERO;
        let mut elapsed_hydrate = tokio::time::Duration::ZERO;

        while let Some(dep) = stack.pop() {
            // Avoid processing the same dependency more than once.
            if !deps.insert(dep.clone()) {
                continue;
            }

            // hack the test to get this quick
            let target: Arc<TransientTarget> = unsafe {
                let raw = Arc::into_raw(target.clone()) as *const TransientTarget;
                Arc::from_raw(raw)
            };
            let start = tokio::time::Instant::now();
            let Ok(mut doc) = target.get_fast(dep.id()).await else {
                continue;
            };
            elapsed_get += start.elapsed();

            let start = tokio::time::Instant::now();
            let patchlog = PatchLog::inactive();
            let Ok(tx) = doc.transaction_at(patchlog, dep.heads().iter().as_slice()) else {
                continue;
            };

            elapsed_transact += start.elapsed();

            let start = tokio::time::Instant::now();
            let data: DocumentData = autosurgeon::hydrate(&tx).unwrap();
            elapsed_hydrate += start.elapsed();
            stack.extend(data.deps);
        }

        tracing::info!(elapsed_get = ?elapsed_get);
        tracing::info!(elapsed_transact = ?elapsed_transact);
        tracing::info!(elapsed_hydrate = ?elapsed_hydrate);
        deps
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
    (Clockument::new(root, db.clone()), db)
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

#[test]
fn test() {
    let test = 1;
    assert_eq!(test, 1);
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
    #[case] config: ClockumentConfig,
    #[case] target_setup: TargetSetup,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tracing_subscriber::fmt::format::FmtSpan;

    tracing_subscriber::fmt()
        .with_test_writer()
        .with_span_events(FmtSpan::CLOSE)
        .with_max_level(tracing::Level::INFO)
        .init();
    let set = make_target_set(target_setup);
    let (clockument, data) = setup_clockument(config).await;

    let coordinator = ClockumentCoordinator::new(clockument.clone());
    for target in &set {
        coordinator.add_persistence(target.clone()).await?;
    }

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

    // Make sure all targets have all documents
    for target in &set {
        assert!(
            target.has(&cloc_ref).await?,
            "the root clockument should be persisted"
        );
        for (id, doc) in &data.docs {
            let ref_ = DocumentRef::new(id.clone(), doc.get_heads().into());
            assert!(
                target.has(&ref_).await?,
                "all dependencies should be persisted"
            );
        }
    }

    Ok(())
}

// TODO: Test loading from existing persistence (persistence already has the data when we add it)
// TODO: Test that newly added targets can be informed of existing state and caught up
// TODO: Test that transact for roots and transact_at for dependencies propagates correctly
// TODO: Test that multiple simultaneous transact calls without calling heads_ready
// TODO: Test that an error resolving function works, with various modes
