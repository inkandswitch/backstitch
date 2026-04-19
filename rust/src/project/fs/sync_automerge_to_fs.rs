use std::path::PathBuf;

use futures::future::join_all;
use tracing::instrument;

use crate::{
    fs::file_utils::{FileContent, FileSystemEvent},
    helpers::history_ref::HistoryRef,
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
        Self { branch_db, fs_index }
    }

    // TODO: We should consider running partial checkouts to the FS.
    // Currently, if we get a remote change, and a single file is unsaved in Godot, we can't call this method at all.
    // Ideally, we'd check out the synced ref, and just exclude the edited files.

    /// Check out a [HistoryRef] from the Backstitch history, changing the filesystem as necessary.
    /// Returns a vector of file changes.
    #[instrument(skip_all)]
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
            .get_changed_file_content_between_refs(checked_out_ref.as_ref(), &goal_ref, false)
            .await
        else {
            tracing::error!(
                "Couldn't get changed file content between refs; canceling ref checkout of {:?}",
                goal_ref
            );
            return Vec::new();
        };

        // Consider instead using a Tokio join set here...
        let futures = changes.into_iter().map(async |change| {
            let written = match &change {
                FileSystemEvent::FileCreated(path, content) => {
                    self.handle_file_create(path, content).await
                }
                FileSystemEvent::FileModified(path, content) => {
                    self.handle_file_update(path, content).await
                }
                FileSystemEvent::FileDeleted(path) => self.handle_file_delete(path).await,
            };
            (change, written)
        });

        let results: Vec<FileSystemEvent> = join_all(futures)
            .await
            .into_iter()
            .filter_map(|(event, written)| written.then_some(event))
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
            },
        }
    }

    async fn handle_file_create(&self, path: &PathBuf, content: &FileContent) -> bool {
        // Skip if path matches any ignore pattern
        if self.branch_db.should_ignore(&path) {
            return false;
        }

        if self.compare_hashes(path, content).await {
            return false;
        }

        // Write the file content to disk
        if let Err(e) = content.write(&path).await {
            tracing::error!("Failed to write file {:?} during checkout: {}", path, e);
            return false;
        };
        tracing::info!("Successfully wrote {:?}", path);
        true
    }

    /// Update a file on disk if it exists and hasn't been ignored, and if the hash has changed.
    /// Returns true if we successfully wrote the file.
    async fn handle_file_update(&self, path: &PathBuf, content: &FileContent) -> bool {
        // Skip if path matches any ignore pattern
        if self.branch_db.should_ignore(&path) {
            return false;
        }

        if self.compare_hashes(path, content).await {
            return false;
        }
        
        // Write the file content to disk
        if let Err(e) = content.write(&path).await {
            tracing::error!("Failed to write file {:?} during checkout: {}", path, e);
            return false;
        };
        tracing::info!("Successfully modified {:?}", path);
        true
    }

    /// Delete a file on disk, if it exists and isn't ignored. Returns true if we successfully deleted the file.
    async fn handle_file_delete(&self, path: &PathBuf) -> bool {
        // Skip if path matches any ignore pattern
        if self.branch_db.should_ignore(&path) {
            return false;
        }

        let Ok(canon) = path.canonicalize() else {
            tracing::error!(
                "Failed to delete file {:?} during checkout because it's already gone.",
                path
            );
            return false;
        };

        // Delete the file from disk
        match tokio::fs::remove_file(&canon).await {
            Err(e) => {
                tracing::error!("Failed to delete file {:?} during checkout: {}", path, e);
                return false;
            }
            Ok(_) => (),
        };
        tracing::info!("Successfully deleted {:?}", path);
        return true;
    }
}
