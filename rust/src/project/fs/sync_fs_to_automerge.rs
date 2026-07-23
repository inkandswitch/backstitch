use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use futures::{StreamExt, stream};
use tokio::{select, sync::Mutex};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::{
    fs::file_utils::FileContent,
    helpers::{history_ref::HistoryRef, spawn_utils::spawn_named},
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

#[derive(Clone)]
pub struct CommitStatus {
    pub hash_before_commit: Option<blake3::Hash>,
    pub hash_after_commit: Option<blake3::Hash>,
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

    /// Make a commit of all changes from the filesystem to the given automerge ref.
    /// Returns a map of committed files, with their hashes before and after.
    /// skip_files will skip the commit for files that have the same hash.
    #[tracing::instrument(skip_all, level = "trace")]
    pub async fn commit(
        &self,
        ref_: &HistoryRef,
        force: bool,
        skip_files: &HashMap<PathBuf, blake3::Hash>,
    ) -> Option<(HistoryRef, HashMap<PathBuf, CommitStatus>)> {
        let mut pending_changes = self.pending_changes.lock().await;

        if !force && pending_changes.is_empty() {
            return None;
        }

        tracing::info!(
            "There are {:?} watched changes, attempting to commit...",
            pending_changes.len()
        );

        let db_clone = self.branch_db.clone();
        let current_files = FileSystemTraversal::get_all_files(
            self.branch_db.get_project_dir(),
            &self.fs_index,
            move |path, is_dir| db_clone.should_ignore(&path.to_path_buf(), is_dir),
        )
        .instrument(tracing::debug_span!("get_all_files"))
        .await;

        let Ok(old_files) = self
            .branch_db
            .get_hash_index(ref_)
            .instrument(tracing::debug_span!("get_hash_index"))
            .await
            .inspect_err(|e| {
                tracing::error!("Failed to get current files! Canceling commit. Reason: {e}")
            })
        else {
            return None;
        };

        let old_files = old_files
            .into_iter()
            .map(|(k, v)| (self.branch_db.globalize_path(&k), v))
            .collect();
        let diff: HashMap<_, _> = FileSystemTraversal::get_file_changes(&old_files, &current_files)
            .into_iter()
            .filter(|(path, _)| {
                let Some(skip_hash) = skip_files.get(path) else {
                    return true;
                };
                let Some(current_hash) = current_files.get(path) else {
                    return true;
                };
                skip_hash != current_hash
            })
            .collect();

        if diff.is_empty() {
            tracing::info!("Did not commit anything because there's no diff.");
            pending_changes.clear();
            return None;
        }

        tracing::debug!("Current changes: {:?}", diff);
        let keys: HashSet<PathBuf> = diff.into_keys().collect();
        let contents = self.get_file_contents(keys.clone()).await;

        pending_changes.clear();

        let new_ref = self
            .branch_db
            .commit_fs_changes(contents, ref_, None, false)
            .instrument(tracing::debug_span!("commit_fs_changes"))
            .await;
        if let Some((new_ref, hashes)) = new_ref {
            tracing::info!("Successfully made a commit! {:?}", new_ref);
            let ret = hashes
                .into_iter()
                .map(|(path, v)| {
                    let path = self.branch_db.globalize_path(&path);
                    let hash_before = current_files.get(&path).cloned();
                    (
                        path,
                        CommitStatus {
                            hash_after_commit: v,
                            hash_before_commit: hash_before,
                        },
                    )
                })
                .collect();
            return Some((new_ref, ret));
        } else {
            tracing::info!("Did not commit pending files!");
            return None;
        }
    }

    /// Make an initial commit of ALL files from the filesystem to automerge.
    /// Makes the commit on the currently checked-out branch, and checks out the new heads.
    // Awkward: This doesn't support re-writing upgraded scenes back to the FS... But that'll happen on future commits ig.
    pub async fn checkin(&self) {
        // Because we always change the checked out ref after committing, we need to lock this in write mode.
        let r = self.branch_db.get_checked_out_ref_mut();
        let mut checked_out_ref = r.write().await;

        if checked_out_ref.is_none() {
            tracing::error!("Could not check in files; we don't have a branch checked out!");
        } else {
            tracing::info!("Checking in files at ref {:?}", checked_out_ref);
        }

        let db_clone = self.branch_db.clone();
        let current_files = FileSystemTraversal::get_all_files(
            self.branch_db.get_project_dir(),
            &self.fs_index,
            move |path, is_dir| db_clone.should_ignore(&path.to_path_buf(), is_dir),
        )
        .await;

        tracing::info!("Successfully hashed {:?} files", current_files.len());

        let contents = self
            .get_file_contents(current_files.into_keys().collect())
            .await;

        let new_ref = self
            .branch_db
            .commit_fs_changes(contents, checked_out_ref.as_ref().unwrap(), None, true)
            .await;

        if let Some((new_ref, _)) = new_ref {
            *checked_out_ref = Some(new_ref);
        } else {
            tracing::error!("Could not check in files! Making no changes.");
        }
    }

    async fn get_file_contents(
        &self,
        files: HashSet<PathBuf>,
    ) -> Vec<(String, Option<FileContent>)> {
        stream::iter(files)
            .map(|path| async move {
                let exists = tokio::fs::try_exists(&path).await?;
                // If it doesn't exist, the file is removed.
                if !exists {
                    return Ok((self.branch_db.localize_path(&path), None));
                }
                tokio::fs::read(&path).await.map(|data| {
                    (
                        self.branch_db.localize_path(&path),
                        Some(FileContent::from_buf(data, path.to_str().unwrap_or(""))),
                    )
                })
            })
            .buffer_unordered(64)
            .filter_map(|x| async { x.ok() })
            .collect()
            .await
    }
}
