use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use crate::{
    helpers::{
        branch::BranchesMetadataDoc, doc_utils::SimpleDocReader, spawn_utils::spawn_named,
        utils::parse_automerge_url,
    },
    project::branch_db::BranchDb,
};
use automerge::{ROOT, ReadDoc};
use autosurgeon::hydrate;
use futures::{FutureExt, StreamExt};
use samod::{DocHandle, DocumentId, Repo};
use tokio::{select, sync::Mutex};
use tokio_util::sync::CancellationToken;

/// Tracks branch and metadata documents from an Automerge repo, updating BranchDB when the state changes.
#[derive(Debug)]
pub struct DocumentWatcher {
    inner: Arc<DocumentWatcherInner>,
}

#[derive(Debug, Clone)]
struct DocumentWatcherInner {
    repo: Repo,
    branch_db: BranchDb,
    tracked_branches: Arc<Mutex<HashSet<DocumentId>>>,
    token: CancellationToken,
}

impl Drop for DocumentWatcher {
    fn drop(&mut self) {
        self.inner.token.cancel();
    }
}

impl DocumentWatcher {
    /// Spawns the [DocumentWatcher], creating parallel tasks for the metadata document tracking and subsequent tasks for any child documents.
    pub async fn new(repo: Repo, branch_db: BranchDb, metadata_handle: DocHandle) -> Self {
        let inner = Arc::new(DocumentWatcherInner {
            branch_db,
            repo,
            tracked_branches: Default::default(),
            token: CancellationToken::new(),
        });

        let inner_clone = inner.clone();

        // do the initial ingest
        inner_clone
            .ingest_metadata_document(metadata_handle.clone())
            .await;

        // track changes for future ingests
        spawn_named("Metadata tracker", async move {
            inner_clone.track_metadata_document(metadata_handle).await;
        });

        return Self { inner };
    }
}

impl DocumentWatcherInner {
    // The branch documents are a document for each branch, containing all the serialized data for all scenes and text files.
    async fn track_branch_document(&self, id: DocumentId) {
        // This find can fail, if the server doesn't have the document yet, and it's set to a nonpermissive announce policy.
        // Permissive announce policies on the server fix it because the server is allowed to check with peers before
        // find return false. But with NeverAnnounce, servers can't check with peers to see if they have a document.
        // So, we need to call find() over and over until it is available on the server... then we can ingest.
        // Alex wants to add Repo::query(), that returns an ongoing query for a document rather than a future. Then, we can
        // improve this significantly.
        let handle = loop {
            tracing::info!("Polling for branch document {id}");
            if let Some(handle) = self.repo.find(id.clone()).await.unwrap() {
                break handle;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        
        self.ingest_branch_document(handle.clone()).await;

        let mut stream = handle.changes();
        loop {
            select! {
                _ = stream.next() => {
                    // collapse the rest of the stream, in case multiple futures are ready
                    while let Some(_) = stream.next().now_or_never().flatten() {}
                    self.ingest_branch_document(handle.clone()).await;
                },
                _ = self.token.cancelled() => {
                    break;
                }
            }
        }
    }

    // The metadata document is the root document containing IDs of all branch docs.
    async fn track_metadata_document(&self, handle: DocHandle) {
        let mut stream = handle.changes();
        loop {
            select! {
                _ = stream.next() => {
                    // collapse the rest of the stream, in case multiple futures are ready
                    while let Some(_) = stream.next().now_or_never().flatten() {}
                    self.ingest_metadata_document(handle.clone()).await;
                },
                _ = self.token.cancelled() => {
                    break;
                }
            }
        }
    }

    // Binary documents are immutable, linked docs that contain binary data.
    // By tracking them, we ensure BranchDb is aware of them.
    async fn track_binary_document(&self, doc_id: DocumentId) {
        let repo = self.repo.clone();
        let branch_db = self.branch_db.clone();
        // easy early exit
        if branch_db.has_binary_doc(&doc_id).await {
            return;
        }
        tokio::task::spawn(async move {
            let handle = repo.find(doc_id.clone()).await;
            // this may trigger a reconciliation for a shadow doc
            branch_db
                .ingest_binary_doc(doc_id, handle.ok().flatten())
                .await;
        });
    }

    #[tracing::instrument(skip_all)]
    async fn ingest_branch_document(&self, handle: DocHandle) {
        let h = handle.clone();
        let (heads, linked_docs) = tokio::task::spawn_blocking(move || {
            // Collect all linked doc IDs from this branch
            h.with_document(|d| {
                let files = match d.get_obj_id(ROOT, "files") {
                    Some(files) => files,
                    None => {
                        tracing::warn!("Failed to load files for branch doc {:?}", h.document_id());
                        return (d.get_heads(), HashMap::new());
                    }
                };

                let linked_docs = d
                    .keys(&files)
                    .filter_map(|path| {
                        let file = match d.get_obj_id(&files, &path) {
                            Some(file) => file,
                            None => {
                                tracing::error!("Failed to load linked doc {:?}", path);
                                return None;
                            }
                        };

                        let url = match d.get_string(&file, "url") {
                            Some(url) => url,
                            None => {
                                return None;
                            }
                        };

                        parse_automerge_url(&url).map(|id| (path.clone(), id))
                    })
                    .collect::<HashMap<String, DocumentId>>();

                (d.get_heads(), linked_docs)
            })
        })
        .await
        .unwrap();

        for (_, doc) in &linked_docs {
            // spawn off a task to track the binary document
            self.track_binary_document(doc.clone()).await;
        }

        self.branch_db
            .update_branch_sync_state(handle, heads, linked_docs.values().cloned().collect())
            .await;
    }

    #[tracing::instrument(skip_all)]
    async fn ingest_metadata_document(&self, handle: DocHandle) {
        // TODO: Stop tracking removed branches
        // Find added branches, and begin tracking them
        let h = handle.clone();
        let meta = tokio::task::spawn_blocking(move || {
            // TODO: correct error handling on hydration failure; currently panics!
            let branches_metadata: BranchesMetadataDoc = h.with_document(|d| hydrate(d).unwrap());
            branches_metadata
        })
        .await
        .unwrap();
        self.branch_db
            .set_metadata_state(handle, meta.clone())
            .await;
        // check if there are new branches that haven't loaded yet
        let mut tracked_branches = self.tracked_branches.lock().await;
        for (branch_id, _) in meta.branches.iter() {
            if !tracked_branches.contains(branch_id) {
                tracked_branches.insert(branch_id.clone());
                let this = self.clone();
                let id = branch_id.clone();
                spawn_named(&format!("Document tracker: {:?}", branch_id), async move {
                    this.track_branch_document(id).await
                });
            }
        }
    }
}
