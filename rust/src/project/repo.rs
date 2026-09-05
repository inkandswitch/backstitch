use std::{
    cell::{LazyCell, OnceCell},
    collections::{BTreeSet, HashMap},
    ops::DerefMut,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

use automerge::{Automerge, AutomergeError, ChangeHash, transaction::CommitOptions};
use future_form::Sendable;
use futures::Stream;
use rand::{Rng, RngExt};
use sedimentree_core::{
    blob::{Blob, BlobMeta},
    depth::CountLeadingZeroBytes,
    fragment::Fragment,
    id::SedimentreeId,
    loose_commit::{LooseCommit, id::CommitId},
    sedimentree::Sedimentree,
};
use subduction_core::{
    connection::message::SyncMessage,
    handler::sync::SyncHandler,
    policy::open::OpenPolicy,
    remote_heads::RemoteHeadsObserver,
    storage::memory::MemoryStorage,
    subduction::{Subduction, builder::SubductionBuilder, error::WriteError},
    timeout::call::CallTimeout,
};
use subduction_crypto::signer::memory::MemorySigner;
use subduction_redb_storage::{RedbStorage, RedbStorageError};
use subduction_websocket::tokio::{TimeoutTokio, TokioSpawn, client::TokioWebSocketClient};
use thiserror::Error;
use tokio::{select, sync::Mutex};
use tokio_util::sync::CancellationToken;

use crate::{
    helpers::spawn_utils::spawn_named,
    project::repo::{
        automerge_subduction_ingest::ingest_automerge,
        doc_db::{DocumentDb, DocumentDbError},
    },
};

type Subd = Subduction<
    'static,
    Sendable,
    RedbStorage,
    TokioWebSocketClient<MemorySigner>,
    SyncHandler<
        Sendable,
        RedbStorage,
        TokioWebSocketClient<MemorySigner>,
        OpenPolicy,
        CountLeadingZeroBytes,
        TokioSpawn,
        256,
        HeadsObserver,
    >,
    OpenPolicy,
    MemorySigner,
    TimeoutTokio,
    TokioSpawn,
    CountLeadingZeroBytes,
    256,
>;

mod automerge_subduction_ingest;
mod doc_db;

static EMPTY_AUTOMERGE: OnceLock<Automerge> = OnceLock::new();

fn empty_automerge() -> &'static Automerge {
    EMPTY_AUTOMERGE.get_or_init(|| {
        let mut doc = Automerge::new();
        doc.empty_commit(CommitOptions {
            message: Some("Initial commit".to_string()),
            time: None,
        });
        doc
    })
}

#[derive(Debug, Clone)]
pub struct Repo {
    subduction: Arc<Subd>,
    doc_db: DocumentDb,
    // todo (subd): implement
    token: CancellationToken,
}

pub struct DocumentChanged {
    pub new_heads: Vec<ChangeHash>,
}

#[derive(Error, Debug)]
pub enum RepoError {
    #[error("No such document {0}")]
    NoSuchDocument(SedimentreeId),
    #[error(transparent)]
    Storage(#[from] RedbStorageError),
    #[error(transparent)]
    Automerge(#[from] AutomergeError),
    #[error(transparent)]
    Write(
        #[from] WriteError<Sendable, RedbStorage, TokioWebSocketClient<MemorySigner>, SyncMessage>,
    ),
    #[error("the repo has been stopped")]
    Stopped,
    // TODO (subd): Forward the error
    #[error("there was an IO error")]
    Io,
}

impl From<DocumentDbError> for RepoError {
    fn from(value: DocumentDbError) -> Self {
        match value {
            DocumentDbError::Automerge(automerge_error) => RepoError::Automerge(automerge_error),
            DocumentDbError::NoSuchDocument(sedimentree_id) => {
                RepoError::NoSuchDocument(sedimentree_id)
            }
        }
    }
}

pub struct HeadsObserver {
    subduction: Arc<std::sync::Mutex<Option<Arc<Subd>>>>,
    doc_db: DocumentDb,
}

impl RemoteHeadsObserver for HeadsObserver {
    fn on_remote_heads(
        &self,
        id: SedimentreeId,
        peer: subduction_core::peer::id::PeerId,
        heads: subduction_core::remote_heads::RemoteHeads,
    ) {
        let subd = self.subduction.lock().expect("AAA");
        if subd.is_none() {
            return;
        }
        let sub = subd.clone().unwrap().clone();
        let doc_db = self.doc_db.clone();
        tokio::task::spawn_blocking(async move || {
            let blobs = match sub.get_blobs(id).await {
                Ok(Some(blobs)) => blobs.into(),
                Ok(None) => Vec::new(),
                Err(e) => {
                    tracing::error!("Error while fetching blobs of {id} from storage: {e}");
                    return;
                }
            };

            match doc_db.insert_blobs(id, blobs).await {
                Ok(()) => {}
                Err(e) => tracing::error!("Error while inserting blobs of {id}: {e}"),
            };
        });
    }
}

impl Repo {
    pub fn subduction(&self) -> Arc<Subd> {
        self.subduction.clone()
    }

    pub fn stop(&self) {
        self.token.cancel();
        self.subduction.shutdown();
    }

    pub fn new(storage_directory: PathBuf) -> Result<Self, RepoError> {
        let doc_db = DocumentDb::new();
        let sub: Arc<std::sync::Mutex<Option<Arc<Subd>>>> = Default::default();
        let heads_observer = HeadsObserver {
            subduction: sub.clone(),
            doc_db: doc_db.clone(),
        };

        let storage = RedbStorage::new(storage_directory)?;
        let (subduction, sync_handler, listener, connection_manager) =
            SubductionBuilder::<_, _, _, _, _, _, 256>::default()
                .storage(storage, Arc::new(OpenPolicy))
                .spawner(TokioSpawn)
                // TODO (keyhive): Don't generate a key; use a stable one.
                .signer(MemorySigner::generate())
                .timer(TimeoutTokio)
                .heads_observer(heads_observer)
                .build();

        let mut guard = sub.lock().expect("ajajajaja");
        *guard = Some(subduction.clone());
        drop(guard);

        let token = CancellationToken::new();
        let tok = token.clone();
        spawn_named("connection manager", async move {
            select! {
                _ = tok.cancelled() => {}
                _ = connection_manager => {}
            }
        });

        let tok = token.clone();
        spawn_named("listener", async move {
            select! {
                _ = tok.cancelled() => {}
                _ = listener => {}
            }
        });

        let this = Self {
            subduction,
            doc_db,
            token,
        };

        Ok(this)
    }

    fn ensure_running(&self) -> Result<(), RepoError> {
        if self.token.is_cancelled() {
            return Err(RepoError::Stopped);
        };
        Ok(())
    }

    pub async fn find(&self, id: &SedimentreeId, timeout: Duration) -> Result<(), RepoError> {
        self.ensure_running()?;
        if self.doc_db.has(id).await {
            return Ok(());
        }

        let blobs = self
            .subduction()
            .fetch_blobs(
                id.clone(),
                CallTimeout::TimeoutMillis(timeout.as_millis() as u64),
            )
            .await;

        let blobs = match blobs {
            Ok(v) => v,
            Err(e) => return Err(RepoError::Io),
        };

        let blobs = blobs.ok_or(RepoError::NoSuchDocument(id.clone()))?;

        self.doc_db.insert_blobs(id.clone(), blobs.into()).await?;

        Ok(())
    }

    // TODO (subd): implement
    pub async fn changes(
        &self,
        id: &SedimentreeId,
    ) -> Result<impl Stream<Item = DocumentChanged> + 'static, RepoError> {
        self.ensure_running()?;
        Ok(futures::stream::pending())
    }

    pub async fn create(&self, initial: &Automerge) -> Result<SedimentreeId, RepoError> {
        let initial = if initial.is_empty() {
            empty_automerge()
        } else {
            initial
        };

        self.ensure_running()?;
        let doc_db = self.doc_db.clone();
        let mut id = [0u8; 32];
        rand::rng().fill_bytes(id.as_mut_slice());
        let id = SedimentreeId::from_bytes(id);

        let result = ingest_automerge(initial, id);

        // should never happen, but just in case (until add_sedimentree takes a NonEmpty)
        if result.blobs.is_empty() {
            panic!("Can't insert an empty automerge document!");
        }

        // maybe do something with this peer result?
        // TODO (subd): this is horrible; don't drive sync here (use store_sedimentree? or wait to put inside subduction?)
        let res = self
            .subduction()
            .add_sedimentree(
                id,
                result.sedimentree,
                result.blobs,
                subduction_core::timeout::call::CallTimeout::TimeoutMillis(5000),
            )
            .await?;

        match self.subduction().get_blobs(id).await? {
            Some(blobs) => {
                doc_db.insert_blobs(id, blobs.into()).await?;
            }
            None => {
                tracing::error!("no blobs returned for inserted ID {id}");
                return Err(RepoError::NoSuchDocument(id));
            }
        }

        Ok(id)
    }

    pub async fn with_document<F, R>(&self, id: &SedimentreeId, f: F) -> Result<R, RepoError>
    where
        F: AsyncFnOnce(&mut Automerge) -> R,
    {
        self.ensure_running()?;
        let result = self.doc_db.with_document(id, f).await?;
        // TODO: actually check if document changed
        let frags = self.doc_db.get_fragments(id).await?;

        for (frag, blob) in frags {
            let mut boundary = BTreeSet::new();
            for bound in frag.boundary {
                boundary.insert(CommitId::new(bound.0));
            }

            // TODO: Don't drive sync here; use store_fragment and sync elsewhere
            self.subduction()
                .add_fragment(
                    *id,
                    CommitId::new(frag.head.0),
                    boundary,
                    frag.checkpoints
                        .into_iter()
                        .map(|c| CommitId::new(c.0))
                        .collect::<Vec<_>>()
                        .as_slice(),
                    Blob::new(blob),
                )
                .await?;
        }

        Ok(result)
    }
}
