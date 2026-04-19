use std::{collections::HashSet, path::PathBuf, sync::Arc};

use futures::{StreamExt, stream};
use tokio::{select, sync::Mutex};
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use crate::{
    fs::file_utils::FileContent,
    helpers::spawn_utils::spawn_named,
    project::{
        branch_db::BranchDb,
        fs::{
            fs_index::FileSystemIndex,
            fs_traversal::FileSystemTraversal,
            fs_watcher::{FileSystemWatcher, WatcherEvent},
        },
    },
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
    pending_changes: Arc<Mutex<Vec<String>>>,
    branch_db: BranchDb,
    fs_index: FileSystemIndex,
    token: CancellationToken,
}

impl Drop for SyncFileSystemToAutomerge {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

impl SyncFileSystemToAutomerge {
    pub fn new(branch_db: BranchDb, fs_index: FileSystemIndex) -> Self {
        let pending_changes = Arc::new(Mutex::new(Vec::new()));
        let token = CancellationToken::new();

        let pending_changes_clone = pending_changes.clone();
        let branch_db_clone = branch_db.clone();
        let token_clone = token.clone();

        // TODO (Lilith): Now that we have hash-based indexing, we don't need to respond to watcher changes directly.
        // I'm keeping this code for now for speed of implementation, but it's legacy.
        // Soon, I want to just to do a naive filesystem diff when we detect ANY watched, unignored change.

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
                        let Some(WatcherEvent::FileTouched(path)) = event else { continue; };
                        pending_changes_clone
                            .lock()
                            .await
                            .push(branch_db_clone.localize_path(&path));
                    },
                    _ = token_clone.cancelled() => { break; }
                }
            }
        });

        Self {
            pending_changes,
            fs_index,
            token,
            branch_db,
        }
    }

    /// Make a commit of all changes from the filesystem to automerge.
    /// Returns true on success.
    #[instrument(skip_all)]
    pub async fn commit(&self, force: bool) -> bool {
        // Because we always change the checked out ref after committing, we need to lock this in write mode.
        let r = self.branch_db.get_checked_out_ref_mut();
        let mut checked_out_ref = r.write().await;

        let mut pending_changes = self.pending_changes.lock().await;

        if !force && pending_changes.is_empty() {
            return false;
        }

        tracing::info!(
            "There are {:?} watched changes, attempting to commit...",
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

        // we can probably do better for larger trees? this traversal could be slow
        let current_files = FileSystemTraversal::get_all_files(
            self.branch_db.get_project_dir(),
            &self.fs_index,
            |path| self.branch_db.should_ignore(&path.to_path_buf()),
        )
        .await;
        let Some(old_files) = self
            .branch_db
            .get_hash_index(&checked_out_ref.as_ref().unwrap())
            .await
        else {
            tracing::error!("Failed to get current files!");
            return false;
        };

        let old_files = old_files
            .into_iter()
            .map(|(k, v)| (self.branch_db.globalize_path(&k), v))
            .collect();
        let diff = FileSystemTraversal::get_file_changes(old_files, current_files);

        if diff.is_empty() {
            tracing::info!("Did not commit anything because there's no diff.");
            return false;
        }

        tracing::debug!("Current changes: {:?}", diff);
        let contents = self.get_file_contents(&diff.into_keys().collect()).await;

        pending_changes.clear();

        let new_ref = self
            .branch_db
            .commit_fs_changes(contents, &checked_out_ref.as_ref().unwrap(), None, false)
            .await;
        if let Some(new_ref) = new_ref {
            tracing::info!("Successfully made a commit! {:?}", new_ref);
            *checked_out_ref = Some(new_ref);
            return true;
        } else {
            tracing::info!("Did not commit pending files!");
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
        } else {
            tracing::info!("Checking in files at ref {:?}", checked_out_ref);
        }

        let current_files = FileSystemTraversal::get_all_files(
            self.branch_db.get_project_dir(),
            &self.fs_index,
            |path| self.branch_db.should_ignore(&path.to_path_buf()),
        )
        .await;

        let contents = self
            .get_file_contents(&current_files.into_keys().collect())
            .await;

        let new_ref = self
            .branch_db
            .commit_fs_changes(contents, &checked_out_ref.as_ref().unwrap(), None, true)
            .await;

        if let Some(new_ref) = new_ref {
            *checked_out_ref = Some(new_ref);
        } else {
            tracing::error!("Could not check in files! Making no changes.");
        }
    }

    async fn get_file_contents(&self, files: &HashSet<PathBuf>) -> Vec<(String, FileContent)> {
        stream::iter(files)
            .then(|path| async move {
                tokio::fs::read(path).await.map(|data| {
                    (
                        self.branch_db.localize_path(path),
                        FileContent::from_buf(data),
                    )
                })
            })
            .filter_map(|x| async { x.ok() })
            .collect()
            .await
    }
}
