use std::{collections::HashMap, path::PathBuf};

use crate::{
    fs::file_utils::FileContent,
    helpers::{history_ref::HistoryRef, utils::ChangeType},
    project::{
        branch_db::BranchDb,
        fs::{fs_index::FileSystemIndex, fs_traversal::FileSystemTraversal},
    },
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

    /// Check out a [HistoryRef] from the Backstitch history. Does NOT modify the filesystem yet.
    /// Returns a vector of file changes that should be applied.
    #[tracing::instrument(skip_all, level = "trace")]
    pub async fn checkout_ref(
        &self,
        current_ref: Option<&HistoryRef>,
        goal_ref: &HistoryRef,
    ) -> Option<HashMap<PathBuf, (ChangeType, Option<FileContent>)>> {
        if current_ref.is_some_and(|r| r == goal_ref) {
            return Default::default();
        }

        tracing::info!(
            "Our current ref is different than the requested ref. Attempting to checkout {:?}",
            goal_ref
        );

        let db_clone = self.branch_db.clone();
        let current_files = FileSystemTraversal::get_all_files(
            self.branch_db.get_project_dir(),
            &self.fs_index,
            move |path, is_dir| db_clone.should_ignore(&path.to_path_buf(), is_dir),
        )
        .await
        .into_iter()
        .map(|(k, v)| (self.branch_db.localize_path(&k), v))
        .collect();

        let goal_files = self.branch_db.get_hash_index(goal_ref).await.inspect_err(|e|{
            tracing::error!(
                "Couldn't get changed file content between refs; canceling ref checkout of {goal_ref:?}. Reason: {e}"
            )}).ok()?;

        let changes = FileSystemTraversal::get_file_changes(current_files, goal_files);

        if changes.is_empty() {
            return Some(Default::default());
        }

        tracing::debug!("Changes to be applied: {:?}", changes);

        let mut contents = self
            .branch_db
            .get_files_at_ref(goal_ref, &changes.keys().cloned().collect())
            .await
            .inspect_err(|e| tracing::error!(
                "Couldn't get file content between refs; canceling ref checkout of {goal_ref:?}. Reason: {e}",
            )).ok()?;

        let joined: HashMap<PathBuf, (ChangeType, Option<FileContent>)> = changes
            .into_iter()
            .filter_map(|(path, change_type)| {
                let gpath = self.branch_db.globalize_path(&path);
                match change_type {
                    ChangeType::Created | ChangeType::Modified => {
                        Some((gpath, (change_type, Some(contents.remove(&path)?))))
                    }
                    ChangeType::Deleted => Some((gpath, (change_type, None))),
                }
            })
            .collect();

        Some(joined)
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
                false
            }
            Err(e) => {
                tracing::error!("Couldn't get existing hash for file {:?}, {e}", path);
                false
            }
        }
    }

    /// Update a file on disk if it exists and hasn't been ignored, and if the hash has changed.
    /// Returns Some(()) if we successfully wrote the file.
    pub async fn handle_file_update(&self, path: &PathBuf, content: &FileContent) -> Option<()> {
        // Skip if path matches any ignore pattern
        if self.branch_db.should_ignore(path, false) {
            return None;
        }

        if self.compare_hashes(path, content).await {
            return None;
        }

        // Write the file content to disk
        if let Err(e) = content.write(path).await {
            tracing::error!("Failed to write file {:?} during checkout: {}", path, e);
            return None;
        };
        tracing::info!("Successfully modified {:?}", path);
        Some(())
    }

    /// Delete a file on disk, if it exists and isn't ignored. Returns Some(()) if we successfully deleted the file.
    pub async fn handle_file_delete(&self, path: &PathBuf) -> Option<()> {
        // Skip if path matches any ignore pattern
        if self.branch_db.should_ignore(path, false) {
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
        if let Err(e) = tokio::fs::remove_file(&canon).await {
            tracing::error!("Failed to delete file {:?} during checkout: {}", path, e);
            return None;
        };
        tracing::info!("Successfully deleted {:?}", path);
        Some(())
    }
}
