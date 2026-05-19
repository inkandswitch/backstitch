use std::{collections::HashMap, path::PathBuf};

use futures::future::join_all;
use tracing::Instrument;

use crate::{
    fs::file_utils::{FileContent, FileSystemEvent},
    helpers::{history_ref::HistoryRef, utils::ChangeType},
    project::{branch_db::BranchDb, fs::fs_index::FileSystemIndex},
};

#[derive(Debug)]
pub struct SyncAutomergeToFileSystem {
    branch_db: BranchDb,
    fs_index: FileSystemIndex,
}

impl SyncAutomergeToFileSystem {
    /// Create a new instance of [SyncAutomergeToFileSystem]. Does not start any process.
    /// Call checkout_ref to do something.
    pub fn new(branch_db: BranchDb, fs_index: FileSystemIndex) -> Self {
        Self {
            branch_db,
            fs_index,
        }
    }

    // TODO: We should consider running partial checkouts to the FS.
    // Currently, if we get a remote change, and a single file is unsaved in Godot, we can't call this method at all.
    // Ideally, we'd check out the synced ref, and just exclude the edited files.

    /// Check out a [HistoryRef] from the Backstitch history, changing the filesystem as necessary.
    /// Returns a vector of file changes.
    #[tracing::instrument(skip_all, level = "trace")]
    pub async fn checkout_ref(&self, goal_ref: HistoryRef) -> Vec<FileSystemEvent> {
        // Ensure that there's no way anything can grab the ref while we're trying to write it
        let r = self.branch_db.get_checked_out_ref_mut();
        let mut checked_out_ref = r.write().await;

        if checked_out_ref.as_ref().is_some_and(|r| r == &goal_ref) {
            return Vec::new();
        }

        tracing::info!(
            "Our current ref is different than the requested ref. Attempting to checkout {:?}",
            goal_ref
        );

        let Some(changes) = self
            .branch_db
            .get_changed_files_between_refs(checked_out_ref.as_ref(), &goal_ref)
            .instrument(tracing::info_span!("get_changed_file_content_between_refs"))
            .await
        else {
            tracing::error!(
                "Couldn't get changed file content between refs; canceling ref checkout of {:?}",
                goal_ref
            );
            return Vec::new();
        };

        let Some(mut contents) = self
            .branch_db
            .get_files_at_ref(&goal_ref, &changes.keys().cloned().collect())
            .await
        else {
            tracing::error!(
                "Couldn't get file content between refs; canceling ref checkout of {:?}",
                goal_ref
            );
            return Vec::new();
        };

        let joined: HashMap<String, (ChangeType, FileContent)> = changes
            .into_iter()
            .filter_map(|(path, change_type)| {
                contents
                    .remove(&path)
                    .map(|content| (path, (change_type, content)))
            })
            .collect();

        // Consider instead using a Tokio join set here...
        let futures = joined
            .into_iter()
            .map(async |(path, (change_type, content))| {
                let global_path = self.branch_db.globalize_path(&path);
                match change_type {
                    ChangeType::Created | ChangeType::Modified => {
                        self.handle_file_update(&global_path, &content).await?
                    }
                    ChangeType::Deleted => self.handle_file_delete(&global_path).await?,
                };
                Some((global_path, change_type, content))
            });

        let results: Vec<FileSystemEvent> = join_all(futures)
            .await
            .into_iter()
            .filter_map(|r| r)
            .map(|(path, change_type, content)| match change_type {
                ChangeType::Created => FileSystemEvent::FileCreated(path, content),
                ChangeType::Deleted => FileSystemEvent::FileDeleted(path),
                ChangeType::Modified => FileSystemEvent::FileModified(path, content),
            })
            .collect();

        tracing::info!("Wrote {:?} files!", results.len());

        *checked_out_ref = Some(goal_ref);
        results
    }

    async fn compare_hashes(&self, path: &PathBuf, content: &FileContent) -> bool {
        match self.fs_index.get_hash(path).await {
            Ok(existing_hash) => {
                let hash = content.to_hash();
                if hash == existing_hash {
                    tracing::warn!(
                        "Skipping creating file {:?} because it already exists, and the hash is the same.",
                        path
                    );
                    return true;
                }
                tracing::warn!(
                    "File {:?} already exists with a different hash; overwriting.",
                    path
                );
                return false;
            }
            Err(e) => {
                tracing::error!("Couldn't get existing hash for file {:?}, {e}", path);
                return false;
            }
        }
    }

    /// Update a file on disk if it exists and hasn't been ignored, and if the hash has changed.
    /// Returns true if we successfully wrote the file.
    async fn handle_file_update(&self, path: &PathBuf, content: &FileContent) -> Option<()> {
        // Skip if path matches any ignore pattern
        if self.branch_db.should_ignore(&path, false) {
            return None;
        }

        if self.compare_hashes(path, content).await {
            return None;
        }

        // Write the file content to disk
        if let Err(e) = content.write(&path).await {
            tracing::error!("Failed to write file {:?} during checkout: {}", path, e);
            return None;
        };
        tracing::info!("Successfully modified {:?}", path);
        Some(())
    }

    /// Delete a file on disk, if it exists and isn't ignored. Returns true if we successfully deleted the file.
    async fn handle_file_delete(&self, path: &PathBuf) -> Option<()> {
        // Skip if path matches any ignore pattern
        if self.branch_db.should_ignore(&path, false) {
            return None;
        }

        let Ok(canon) = path.canonicalize() else {
            tracing::error!(
                "Failed to delete file {:?} during checkout because it's already gone.",
                path
            );
            return None;
        };

        // Delete the file from disk
        match tokio::fs::remove_file(&canon).await {
            Err(e) => {
                tracing::error!("Failed to delete file {:?} during checkout: {}", path, e);
                return None;
            }
            Ok(_) => (),
        };
        tracing::info!("Successfully deleted {:?}", path);
        Some(())
    }
}
