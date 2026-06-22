use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, SystemTime},
};

use automerge::ChangeHash;
use futures::StreamExt;
use tokio::{
    select,
    sync::{Mutex, Notify, watch},
};
use tokio_stream::wrappers::WatchStream;
use tokio_util::sync::CancellationToken;

use crate::{
    helpers::{
        spawn_utils::spawn_named,
        utils::{CommitInfo, CommitMetadata, summarize_changes},
    },
    project::{branch_db::BranchDb, peer_watcher::PeerWatcher, project_api::ChangeViewModel},
};

#[derive(Debug)]
pub struct ChangeIngester {
    inner: Arc<ChangeIngesterInner>,
    token: CancellationToken,
}

#[derive(Debug)]
struct ChangeIngesterInner {
    changes_tx: watch::Sender<Vec<CommitInfo>>,
    ingestion_request: Notify,
    peer_watcher: Arc<PeerWatcher>,
    token: CancellationToken,
    last_ingest: Mutex<(SystemTime, i32)>,
    branch_db: BranchDb,
}

impl Drop for ChangeIngester {
    fn drop(&mut self) {
        self.token.cancel()
    }
}

impl ChangeIngester {
    pub fn new(peer_watcher: Arc<PeerWatcher>, branch_db: BranchDb) -> Self {
        let (changes_tx, _) = watch::channel(Vec::new());
        let token = CancellationToken::new();
        let ingestion_request = Notify::new();
        let inner = Arc::new(ChangeIngesterInner {
            changes_tx,
            peer_watcher,
            ingestion_request,
            token: token.clone(),
            last_ingest: Mutex::new((SystemTime::UNIX_EPOCH, 0)),
            branch_db,
        });

        let inner_clone = inner.clone();
        spawn_named("Change ingester", async move {
            let peer_stream = WatchStream::new(inner_clone.peer_watcher.subscribe());
            tokio::pin!(peer_stream);

            let reconcile_stream = inner_clone.branch_db.subscribe_doc_changes();
            tokio::pin!(reconcile_stream);

            loop {
                select! {
                    _ = inner_clone.token.cancelled() => { break; }
                    _ = peer_stream.next() => {
                        inner_clone.ingestion_request.notify_one();
                    }
                    _ = reconcile_stream.next() => {
                        inner_clone.ingestion_request.notify_one();
                    }
                    _ = inner_clone.ingestion_request.notified() => {
                        inner_clone.ingest_changes().await;
                    }
                }
            }
        });

        Self { token, inner }
    }

    pub fn request_ingestion(&self) {
        self.inner.ingestion_request.notify_one();
    }

    // I don't like exposing this, but it's the simplest solution for now.
    pub fn get_changes_rx(&self) -> watch::Receiver<Vec<CommitInfo>> {
        self.inner.changes_tx.subscribe()
    }
}

impl ChangeIngesterInner {
    async fn ingest_changes(&self) {
        let mut last_ingest = self.last_ingest.lock().await;
        let now = SystemTime::now();
        let last_diff = now
            .duration_since(last_ingest.0)
            .unwrap_or(Duration::from_secs(0));

        // Impose an arbitrary cap on requests within a time period.
        // This is so that immediate syncs -- such as those from a local server -- don't have to wait before getting synced.
        // But it also prevents spam of like a hundred slowing down the thread.
        let max_requests_before_debounce = 3;
        let debounce = 100;
        if last_diff.as_millis() < debounce {
            if last_ingest.1 >= max_requests_before_debounce {
                tokio::time::sleep(Duration::from_millis(
                    (debounce - last_diff.as_millis()) as u64,
                ))
                .await;
            }
        } else {
            // since we're past the duration with no other requests, the counter resets.
            *last_ingest = (now, 0);
        }
        let new_changes = self.get_changes().await;
        self.changes_tx.send_if_modified(|old_changes| {
            if *old_changes == new_changes {
                return false;
            }
            *old_changes = new_changes;
            true
        });
        last_ingest.1 += 1;
    }

    async fn get_change_summary(&self, change: &CommitInfo) -> Option<String> {
        let meta = change.metadata.as_ref();
        let author = meta?.username.clone().unwrap_or("Anonymous".to_string());

        // merge commit
        if let Some(merge_info) = &meta?.merge_metadata {
            let merged_branch = self
                .branch_db
                .get_branch_name(&merge_info.merged_branch_id.clone())
                .await
                .unwrap_or(merge_info.merged_branch_id.to_string());
            return Some(format!("↪ {author} merged {merged_branch}"));
        }

        // revert commit
        if let Some(revert_info) = &meta?.reverted_to {
            let heads = revert_info
                .iter()
                .map(|s| s.to_string()[..7].to_string())
                .collect::<Vec<String>>()
                .join(", ");
            return Some(format!("↩ {author} reverted to {heads}"));
        }

        // initial commit
        if change.is_setup() {
            return Some("Initialized repository".to_string());
        }

        Some(summarize_changes(&author, meta?.changed_files.as_ref()?))
    }

    /// Gets the changes from the current branch and returns it.
    #[tracing::instrument(skip_all, level = "trace")]
    async fn get_changes(&self) -> Vec<CommitInfo> {
        let checked_out = self.branch_db.get_checked_out_ref_mut();
        let checked_out = checked_out.read().await;
        let Some(checked_out) = checked_out.as_ref() else {
            tracing::info!("Can't get changes; nothing checked out!");
            return Vec::new();
        };

        tracing::trace!("ingesting on ref: {:?}", checked_out);

        // This is the last heads the server has seen. Refers to the canonical
        // document, NOT the shadow document.
        let last_acked_heads = self
            .peer_watcher
            .get_server_info()
            .as_ref()
            .and_then(|info| info.docs.get(checked_out.branch()))
            .and_then(|state| state.last_acked_heads.clone());

        // When we have pending commits, there are several things we need to check.
        // - Unsynced and shadow-only: The commit is present in the shadow doc but not the canonical doc.
        // - Unsynced: The commit is present in the canonical doc but the server hasn't acked them
        // - Canonical-only: The commit is present in the canonical doc but not the shadow doc.
        // - Unsynced and canonical-only: ...
        // For now, any of these trigger an unsynced status. But we need to check everything, because
        // we want to be able to respond to both local commits and incoming commits that haven't reconciled.

        let mut changes = self
            .branch_db
            .get_shadow_changes(checked_out.branch())
            .await
            .unwrap_or(Vec::new());
        let canonical_changes = self
            .branch_db
            .get_canonical_changes(checked_out.branch())
            .await
            .unwrap_or(Vec::new());

        // First step: Merge shadow and canonical.
        let mut shadow_hashes = HashSet::new();
        let mut canon_hashes = HashSet::new();
        for change in &changes {
            shadow_hashes.insert(change.hash);
        }
        for change in &canonical_changes {
            canon_hashes.insert(change.hash);
        }

        // We need this to know whether the changes are actually synced.
        let intersecting_hashes = shadow_hashes
            .symmetric_difference(&canon_hashes)
            .copied()
            .collect::<HashSet<ChangeHash>>();

        // Do the merge
        // This just puts all canonical-only changes after local-only changes... probably fine for now.
        for change in canonical_changes {
            if !shadow_hashes.contains(&change.hash) {
                changes.push(change);
            }
        }

        let changes = changes
            .into_iter()
            .map(|c| {
                CommitInfo {
                    hash: c.hash,
                    timestamp: c.timestamp,
                    metadata: c
                        .message
                        .as_ref()
                        .and_then(|m| serde_json::from_str::<CommitMetadata>(m).ok()),
                    synced: false,           // set later
                    summary: "".to_string(), // set later
                }
            })
            .collect::<Vec<CommitInfo>>();

        // Check to see what the most recent synced commit is.
        // This relies on canonical_changes being all *after* local changes in the list
        let mut synced_until_index = -1;
        for (i, change) in changes.iter().enumerate() {
            if last_acked_heads
                .as_ref()
                .is_some_and(|f| f.contains(&change.hash))
            {
                synced_until_index = i as i32;
            }
        }

        let mut commit_infos = Vec::new();

        for (i, change) in changes.into_iter().enumerate() {
            // only consider changes on the current branch with valid metadata
            let Some(metadata) = &change.metadata else {
                continue;
            };
            let Some(branch_id) = &metadata.branch_id else {
                continue;
            };
            if branch_id != checked_out.branch() {
                continue;
            }

            let summary = self
                .get_change_summary(&change)
                .await
                .unwrap_or("Invalid data".to_string());
            let commit_info = CommitInfo {
                synced: (i as i32) <= synced_until_index
                    && !intersecting_hashes.contains(&change.hash),
                summary,
                ..change
            };
            commit_infos.push(commit_info);
        }
        commit_infos
    }
}
