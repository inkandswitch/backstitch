use std::{
    cell::{LazyCell, OnceCell},
    collections::{BTreeSet, HashMap},
    ops::DerefMut,
    path::PathBuf,
    sync::Arc,
};

use automerge::Automerge;
use future_form::Sendable;
use sedimentree_core::{
    blob::{Blob, BlobMeta},
    depth::CountLeadingZeroBytes,
    fragment::Fragment,
    id::SedimentreeId,
    loose_commit::id::CommitId,
};
use subduction_core::{
    handler::sync::SyncHandler,
    policy::open::OpenPolicy,
    remote_heads::RemoteHeadsObserver,
    storage::memory::MemoryStorage,
    subduction::{Subduction, builder::SubductionBuilder},
};
use subduction_crypto::signer::memory::MemorySigner;
use subduction_redb_storage::{RedbStorage, RedbStorageError};
use subduction_websocket::tokio::{TimeoutTokio, TokioSpawn, client::TokioWebSocketClient};
use thiserror::Error;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{helpers::spawn_utils::spawn_named, project::doc_db::DocumentDb};

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
    >,
    OpenPolicy,
    MemorySigner,
    TimeoutTokio,
    TokioSpawn,
>;

#[derive(Clone)]
pub struct Repo {
    subduction: Arc<Subd>,
    doc_db: DocumentDb,
    // TODO (Subduction): implement
    token: CancellationToken,
}

#[derive(Error, Debug)]
pub enum RepoError {
    #[error("No such document {0}")]
    NoSuchDocument(SedimentreeId),
    #[error(transparent)]
    Storage(#[from] RedbStorageError),
}

struct HeadsObserver {
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
        let mut doc_db = self.doc_db.clone();
        tokio::task::spawn_blocking(async move || {
            let blobs = match sub.get_blobs(id).await {
                Ok(Some(blobs)) => blobs.into(),
                Ok(None) => Vec::new(),
                Err(e) => {
                    tracing::error!("Error while fetching blobs of {id} from storage: {e}");
                    return;
                }
            };

            doc_db.insert_blobs(id, blobs);
        });
    }
}

impl Repo {
    pub fn subduction(&self) -> Arc<Subd> {
        self.subduction.clone()
    }

    pub fn new(storage_directory: PathBuf) -> Result<Self, RepoError> {
        let doc_db = DocumentDb::new();
        let sub: Arc<std::sync::Mutex<Option<Arc<Subd>>>> = Default::default();
        let heads_observer = HeadsObserver {
            subduction: sub.clone(),
            doc_db: doc_db.clone(),
        };
        let storage = RedbStorage::new(storage_directory)?;
        let (subduction, sync_handler, listener, connection_manager) = SubductionBuilder::default()
            .storage(storage, Arc::new(OpenPolicy))
            .spawner(subduction_websocket::tokio::TokioSpawn)
            .signer(MemorySigner::from_bytes(&[0; 32]))
            .timer(TimeoutTokio)
            .heads_observer(heads_observer)
            .build();

        let mut guard = sub.lock().expect("ajajajaja");
        *guard = Some(subduction.clone());
        drop(guard);

        spawn_named("connection manager", async move {
            let _ = connection_manager.await;
        });

        spawn_named("listener", async move {
            let _ = listener.await;
        });

        let this = Self {
            subduction,
            doc_db,
            token: Default::default(),
        };

        Ok(this)
    }

    pub async fn with_document<F, R>(&self, id: &SedimentreeId, f: F) -> Result<R, RepoError>
    where
        F: AsyncFnOnce(&mut Automerge) -> R,
    {
        let result = self.doc_db.with_document(id, f).await?;
        // TODO: actually check if document changed
        let frags = self.doc_db.get_fragments(id).await?;

        for (frag, blob) in frags {
            let mut boundary = BTreeSet::new();
            for bound in frag.boundary {
                boundary.insert(CommitId::new(bound.0));
            }

            self.subduction().add_fragment(
                *id,
                CommitId::new(frag.head.0),
                boundary,
                frag.checkpoints
                    .into_iter()
                    .map(|c| CommitId::new(c.0))
                    .collect::<Vec<_>>()
                    .as_slice(),
                Blob::new(blob),
            );
        }

        Ok(result)
    }
}
