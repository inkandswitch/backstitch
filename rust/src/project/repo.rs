use std::{
    cell::{LazyCell, OnceCell},
    collections::{BTreeSet, HashMap},
    ops::DerefMut,
    path::PathBuf,
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};

use automerge::{Automerge, AutomergeError, ChangeHash, transaction::CommitOptions};
use future_form::Sendable;
use futures::Stream;
use nonempty::NonEmpty;
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
    peer::id::PeerId,
    policy::open::OpenPolicy,
    remote_heads::{RemoteHeads, RemoteHeadsObserver},
    storage::memory::MemoryStorage,
    subduction::{Subduction, builder::SubductionBuilder, error::WriteError},
    timeout::call::CallTimeout,
};
use subduction_crypto::signer::memory::MemorySigner;
use subduction_redb_storage::{RedbStorage, RedbStorageError};
use subduction_websocket::tokio::{TimeoutTokio, TokioSpawn, client::TokioWebSocketClient};
use thiserror::Error;
use tokio::{
    select,
    sync::{Mutex, broadcast, mpsc},
};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use tokio_util::sync::CancellationToken;

use crate::{
    helpers::spawn_utils::spawn_named,
    project::repo::{
        automerge_subduction_ingest::ingest_automerge,
        doc_db::{DocumentDb, DocumentDbError},
        heads::Heads,
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
pub mod heads;

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
    subduction: Weak<Subd>,
    subduction_strong: Arc<Mutex<Option<Arc<Subd>>>>,
    doc_db: DocumentDb,
    // todo (subd): implement
    token: CancellationToken,
    changes_tx: broadcast::Sender<DocumentChanged>,
}

#[derive(Clone)]
pub struct DocumentChanged {
    pub id: SedimentreeId,
    pub new_heads: Heads,
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

struct HeadsObservation {
    id: SedimentreeId,
    peer: PeerId,
    heads: RemoteHeads,
}

pub struct HeadsObserver {
    tx: mpsc::Sender<HeadsObservation>,
}

impl RemoteHeadsObserver for HeadsObserver {
    fn on_remote_heads(
        &self,
        id: SedimentreeId,
        peer: subduction_core::peer::id::PeerId,
        heads: subduction_core::remote_heads::RemoteHeads,
    ) {
        tracing::info!("REMOTE HEADS ! !! ! ! {id} {heads:?}");
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.blocking_send(HeadsObservation { id, peer, heads });
        });
    }
}

impl Repo {
    // todo: probably don't expose this at all
    pub fn subduction(&self) -> Weak<Subd> {
        self.subduction.clone()
    }

    fn subd(&self) -> Result<Arc<Subd>, RepoError> {
        self.subduction.upgrade().ok_or(RepoError::Stopped)
    }

    // TODO: don't explicitly stop; just use drops and avoid cloning the handle
    pub fn stop(&self) {
        tracing::debug!("SHUTTING DOWN REPO");
        self.token.cancel();
        let mut subd = self.subduction_strong.blocking_lock();
        let Some(s) = subd.take() else {
            return;
        };
        // is this necessary?
        s.shutdown();
    }

    async fn update_from_heads(
        &self,
        HeadsObservation { heads, id, peer }: HeadsObservation,
    ) -> Result<(), RepoError> {
        let doc_heads = self.doc_db.get_heads(id).await.ok();
        if doc_heads.is_some_and(|h| {
            Heads::from(
                heads
                    .heads
                    .into_iter()
                    .map(|c| ChangeHash(*c.as_bytes()))
                    .collect::<Vec<ChangeHash>>(),
            ) == h
        }) {
            return Ok(());
        }

        self.subd()?
            .sync_with_all_peers(id, true, CallTimeout::TimeoutMillis(3000))
            .await
            .map_err(|_| RepoError::Io)?;

        let blobs = match self
            .subd()?
            .fetch_blobs(id, CallTimeout::TimeoutMillis(3000))
            .await
        {
            Ok(Some(blobs)) => blobs.into(),
            Ok(None) => Vec::new(),
            Err(_) => return Err(RepoError::Io),
        };

        match self.doc_db.insert_blobs(id, blobs).await {
            Ok(()) => {}
            Err(e) => tracing::error!("Error while inserting blobs of {id}: {e}"),
        };

        // re-fetch heads because the heads observer hates me maybe??
        // TODO (subd): Debug this with brooke
        let heads = self.doc_db.get_heads(id).await?;

        self.notify_document_changed(id, heads).await;
        Ok(())
    }

    async fn notify_document_changed(&self, id: SedimentreeId, new_heads: Heads) {
        let _ = self.changes_tx.send(DocumentChanged { id, new_heads });
    }

    pub fn new(storage_directory: PathBuf) -> Result<Self, RepoError> {
        let doc_db = DocumentDb::new();
        let sub: Arc<std::sync::Mutex<Option<Arc<Subd>>>> = Default::default();

        let (heads_tx, mut heads_rx) = mpsc::channel(256);
        let heads_observer = HeadsObserver { tx: heads_tx };

        let (changes_tx, _) = broadcast::channel(256);

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

        let sub_strong = Arc::new(Mutex::new(Some(subduction.clone())));
        let this = Self {
            subduction_strong: sub_strong,
            subduction: Arc::<Subd>::downgrade(&subduction),
            doc_db,
            token: token.clone(),
            changes_tx,
        };

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

        let tok = token.clone();
        let this_clone = this.clone();
        spawn_named("heads observer", async move {
            loop {
                select! {
                    _ = tok.cancelled() => {break;}
                    o = heads_rx.recv() => {
                        let Some(o) = o else {
                            break;
                        };
                        let _ = this_clone.update_from_heads(o).await.inspect_err(|e| tracing::error!("error updating from heads{e}"));
                    }
                }
            }
        });

        Ok(this)
    }

    fn ensure_running(&self) -> Result<(), RepoError> {
        if self.token.is_cancelled() {
            return Err(RepoError::Stopped);
        };
        Ok(())
    }

    pub async fn find(&self, id: SedimentreeId, timeout: Duration) -> Result<(), RepoError> {
        self.ensure_running()?;
        if self.doc_db.has(id).await {
            return Ok(());
        }

        tracing::debug!("Does not have {id}, searching with timeout {timeout:?}");

        self.subd()?
            .sync_with_all_peers(
                id,
                true,
                CallTimeout::TimeoutMillis(timeout.as_millis() as u64),
            )
            .await
            .map_err(|_| RepoError::Io)?;

        let blobs: Result<Option<NonEmpty<Blob>>, _> = self
            .subd()?
            .fetch_blobs(
                id.clone(),
                CallTimeout::TimeoutMillis(timeout.as_millis() as u64),
            )
            .await;

        tracing::debug!("Blob result: {blobs:?}");

        let blobs = match blobs {
            Ok(v) => v,
            Err(e) => return Err(RepoError::Io),
        };

        let blobs = blobs.ok_or(RepoError::NoSuchDocument(id.clone()))?;

        self.doc_db.insert_blobs(id, blobs.into()).await?;

        // todo: should I do this?
        self.notify_document_changed(id, self.doc_db.get_heads(id).await?)
            .await;

        Ok(())
    }

    // TODO (subd): implement
    pub async fn changes(
        &self,
        id: SedimentreeId,
    ) -> Result<impl Stream<Item = DocumentChanged> + 'static, RepoError> {
        self.ensure_running()?;
        let rx = self.changes_tx.subscribe();
        let stream =
            BroadcastStream::new(rx).filter_map(move |item| item.ok().filter(|item| item.id == id));
        Ok(stream)
    }

    pub async fn create(&self, initial: &Automerge) -> Result<SedimentreeId, RepoError> {
        let initial = if initial.is_empty() {
            empty_automerge()
        } else {
            initial
        };
        tracing::debug!("CREATED");

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
            .subd()?
            .add_sedimentree(
                id,
                result.sedimentree,
                result.blobs,
                subduction_core::timeout::call::CallTimeout::TimeoutMillis(5000),
            )
            .await?;

        match self.subd()?.get_blobs(id).await? {
            Some(blobs) => {
                doc_db.insert_blobs(id, blobs.into()).await?;
            }
            None => {
                tracing::error!("no blobs returned for inserted ID {id}");
                return Err(RepoError::NoSuchDocument(id));
            }
        }

        self.notify_document_changed(id, self.doc_db.get_heads(id).await?)
            .await;

        Ok(id)
    }

    pub async fn with_document<F, R>(&self, id: SedimentreeId, f: F) -> Result<R, RepoError>
    where
        F: AsyncFnOnce(&mut Automerge) -> R,
    {
        self.ensure_running()?;
        let heads_before = self.doc_db.get_heads(id).await?;
        let result = self.doc_db.with_document(id, f).await?;
        let heads_after = self.doc_db.get_heads(id).await?;

        if heads_before == heads_after {
            return Ok(result);
        }

        tracing::debug!("MADE CHANGE");

        let frags = self.doc_db.get_fragments(id).await?;

        for (frag, blob) in frags {
            let mut boundary = BTreeSet::new();
            for bound in frag.boundary {
                boundary.insert(CommitId::new(bound.0));
            }

            // TODO: Don't drive sync here; use store_fragment and sync elsewhere
            self.subd()?
                .add_fragment(
                    id,
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

        self.notify_document_changed(id, heads_after).await;

        Ok(result)
    }
}

impl Drop for Repo {
    fn drop(&mut self) {}
}
