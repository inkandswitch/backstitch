use std::{path::PathBuf, sync::Arc};

use futures::StreamExt;
use tokio::{select, sync::Mutex, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use crate::{
    fs::file_utils::{FileContent, FileSystemEvent}, helpers::spawn_utils::spawn_named, project::{branch_db::BranchDb, fs::fs_watcher::FileSystemWatcher}
};

/// Tracks changes using [FileSystemWatcher], handles the changes, and tracks them as pending.
/// Call `commit` to commit them.
#[derive(Debug)]
pub struct SyncFileSystemToAutomerge {
    // Collects a list of pending changes from the filesystem.
    // In process, we commit these. We do this to make sure we don't make a separate commit for every file change.
    // Or maybe that's OK?
    // TODO (Lilith) Maybe do stream instead? This works for now though
    // Stream is good though because I ***think*** we can poll with now_or_never
    pending_changes: Arc<Mutex<Vec<(String, FileContent)>>>,
    branch_db: BranchDb,
    token: CancellationToken,
}

impl Drop for SyncFileSystemToAutomerge {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

impl SyncFileSystemToAutomerge {
    pub fn new(branch_db: BranchDb) -> Self {
        let pending_changes = Arc::new(Mutex::new(Vec::new()));
        let token = CancellationToken::new();

        let pending_changes_clone = pending_changes.clone();
        let branch_db_clone = branch_db.clone();
        let token_clone = token.clone();

        // TODO (Lilith): stick this on a method on an Inner struct like the rest
        spawn_named("Sync FS to Automerge", async move {
            let changes = FileSystemWatcher::start_watching(
                branch_db_clone.get_project_dir().clone(),
                branch_db_clone.clone(),
            )
            .await;
            tokio::pin!(changes);

            loop {
                select! {
                    event = changes.next() => {
                        let Some(event) = event else { continue; };
                        let (path, content) = match event {
                            FileSystemEvent::FileCreated(path, content) => (path, content),
                            FileSystemEvent::FileModified(path, content) => (path, content),
                            FileSystemEvent::FileDeleted(path) => (path, FileContent::Deleted),
                        };
                        pending_changes_clone
                            .lock()
                            .await
                            .push((branch_db_clone.localize_path(&path), content));
                    },
                    _ = token_clone.cancelled() => { break; }
                }
            }
        });

        Self {
            pending_changes,
            token,
            branch_db,
        }
    }

    /// Make a commit of all watched, pending changes from the filesystem to automerge.
    /// Returns true on success.
    #[instrument(skip_all)]
    pub async fn commit(&self) -> bool {
        // Because we always change the checked out ref after committing, we need to lock this in write mode.
        let r = self.branch_db.get_checked_out_ref_mut();
        let mut checked_out_ref = r.write().await;

        let mut pending_changes = self.pending_changes.lock().await;

        if pending_changes.is_empty() {
            return false;
        }

        tracing::info!(
            "There are {:?} pending changes, attempting to commit...",
            pending_changes.len()
        );

        // If the checked-out ref is invalid, we can't commit to the current branch.
        if checked_out_ref.as_ref().is_none_or(|r| !r.is_valid()) {
            tracing::warn!(
                "Can't commit to the current ref {:?}, because it isn't valid.",
                checked_out_ref
            );
            return false;
        }

        let new_ref = self
            .branch_db
            .commit_fs_changes(
                pending_changes.clone(),
                &checked_out_ref.as_ref().unwrap(),
                None,
                false,
            )
            .await;
        if let Some(new_ref) = new_ref {
            tracing::info!("Successfully made a commit! {:?}", new_ref);
            pending_changes.clear();
            *checked_out_ref = Some(new_ref);
            return true;
        } else {
            tracing::info!("Did not commit pending files!");
            pending_changes.clear();
            return false;
        }
    }

    /// Make an initial commit of ALL files from the filesystem to automerge.
    /// Makes the commit on the currently checked-out branch, and checks out the new heads.
    pub async fn checkin(&self) {
        // Because we always change the checked out ref after committing, we need to lock this in write mode.
        let r = self.branch_db.get_checked_out_ref_mut();
        let mut checked_out_ref = r.write().await;

        if checked_out_ref.is_none() {
            tracing::error!("Could not check in files; we don't have a branch checked out!");
        }
        else {
            tracing::info!("Checking in files at ref {:?}", checked_out_ref);
        }

        let files = self.get_all_files().await;

        let new_ref = self
            .branch_db
            .commit_fs_changes(
                files.clone(),
                &checked_out_ref.as_ref().unwrap(),
                None,
                true,
            )
            .await;

        if let Some(new_ref) = new_ref {
            *checked_out_ref = Some(new_ref);
        } else {
            tracing::error!("Could not check in files! Making no changes.");
        }
    }

    fn get_all_files_recur(
        branch_db: BranchDb,
        path: PathBuf,
        content: Arc<Mutex<Option<Vec<(PathBuf, FileContent)>>>>,
    ) -> impl Future<Output = Option<()>> + Send {
        async move {
            let mut dir = tokio::fs::read_dir(path).await.ok()?;
            let mut set = JoinSet::new();
            while let Some(entry) = dir.next_entry().await.ok()? {
                let path = entry.path();

                // Skip if path matches any ignore pattern
                if branch_db.should_ignore(&path) {
                    continue;
                }

                if path.is_file() {
                    let path_clone = path.clone();
                    let content = content.clone();
                    set.spawn(async move {
                        let data = tokio::fs::read(path).await;
                        match data {
                            Ok(data) => content
                                .lock()
                                .await
                                .as_mut()
                                .unwrap()
                                .push((path_clone, FileContent::from_buf(data))),
                            Err(e) => tracing::error!("Error while trying to read file: {}", e),
                        }
                        None
                    });
                } else if path.is_dir() {
                    let path = path.clone();
                    let branch_db = branch_db.clone();
                    let content = content.clone();
                    set.spawn(
                        async move { Self::get_all_files_recur(branch_db, path, content).await },
                    );
                }
            }
            while let Some(_) = set.join_next().await {}
            None
        }
    }

    async fn get_all_files(&self) -> Vec<(String, FileContent)> {
        let content = Arc::new(Mutex::new(Some(Vec::new())));
        Self::get_all_files_recur(
            self.branch_db.clone(),
            self.branch_db.get_project_dir(),
            content.clone(),
        )
        .await;
        // steal the content from the mutex
        content
            .lock()
            .await
            .take()
            .unwrap()
            .into_iter()
            .map(|(path, content)| (self.branch_db.localize_path(&path), content))
            .collect()
    }
}
