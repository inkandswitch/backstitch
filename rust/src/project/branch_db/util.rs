use std::path::PathBuf;

use automerge::{Automerge, ChangeMetadata};
use samod::DocumentId;
use tracing::instrument;

use crate::{
    helpers::branch::Branch,
    project::branch_db::{BranchDb, HistoryRef},
};

// Utility methods for working with [BranchDb].
impl BranchDb {
    /// Turns a filesytem path into a project-local res:// path.
    /// Local paths are represented with a [String], while global paths are represented with a [PathBuf].
    /// This is because local paths are a URL, not a filesystem path.
    pub fn localize_path(&self, path: &PathBuf) -> String {
        let path = path.to_string_lossy().replace("\\", "/");
        let project_dir = self.project_dir.to_string_lossy().replace("\\", "/");
        if path.starts_with(&project_dir) {
            // TODO: this isn't teeechnically a Path, it's a URL... PathBuf is probably the wrong choice.
            // That's why we turn it into a string when we export!
            let thing = PathBuf::from("res://".to_string())
                .join(PathBuf::from(&path[project_dir.len()..].to_string()));
            thing.to_string_lossy().to_string()
        } else {
            path.to_string()
        }
    }

    /// Convert a project URL like res:// into a local filesystem path.
    /// Local paths are represented with a [String], while global paths are represented with a [PathBuf].
    /// This is because local paths are a URL, not a filesystem path.
    pub fn globalize_path(&self, path: &String) -> PathBuf {
        // trim the project_dir from the front of the path
        if path.starts_with("res://") {
            self.project_dir.clone().join(&path["res://".len()..])
        } else {
            PathBuf::from(path)
        }
    }

    /// Get the most recent ref on a given branch (on the shadow doc).
    #[instrument(skip_all)]
    pub async fn get_latest_ref_on_branch(&self, branch: &DocumentId) -> Option<HistoryRef> {
        let Ok(heads) = self
            .with_shadow_document(branch, async |d| d.get_heads())
            .await
        else {
            return None;
        };

        Some(HistoryRef::new(branch.clone(), heads))
    }

    pub async fn get_main_branch(&self) -> Option<DocumentId> {
        let Some((_, metadata)) = self.get_metadata_state().await else {
            tracing::error!("Couldn't get main branch; no metadata doc.");
            return None;
        };
        return Some(metadata.main_doc_id);
    }

    /// Check if a path should be ignored based on the provided glob patterns
    pub fn should_ignore(&self, path: &PathBuf) -> bool {
        // TODO: We should check if it's a symlink or not, but right now it's sufficient to just check if it's outside of the watch path
        // check if it's outside of the watch path
        if path.is_symlink() {
            return true;
        }
        if !path.starts_with(&self.project_dir) {
            return true;
        }
        // TODO: upstream fix to ignore::gitignore::Gitignore to handle this case.
        // if the path is equal to the project dir minus a trailing slash, gitignore will fail to strip the root and will assert
        let is_dir = path.is_dir();
        if is_dir {
            let string_path = path.to_string_lossy();
            let project_dir = self.project_dir.to_string_lossy();
            if string_path == project_dir
                || (string_path.len() + 1 == project_dir.len()
                    && (project_dir.ends_with("/") || project_dir.ends_with("\\")))
            {
                return false;
            }
        };

        self.gitignore
            .matched_path_or_any_parents(path, is_dir)
            .is_ignore()
    }

    pub async fn get_branch_name(&self, id: &DocumentId) -> Option<String> {
        let meta = self.metadata_state.lock().await;
        Some(meta.as_ref()?.1.branches.get(id)?.name.clone())
    }

    // This is not ideal -- I'd prefer not to clone unless necessary.
    // However, we NEVER want to expose our internal BranchState mutexes.
    // That could cause deadlocks if they acquired a branch state and later tried to call any branch info method on branch_db.
    // Callers should preferentially use other getter methods.
    pub async fn get_branch_state(&self, id: &DocumentId) -> Option<Branch> {
        let meta = self.metadata_state.lock().await;
        Some(meta.as_ref()?.1.branches.get(id)?.clone())
    }

    /// Run a closure over a mutable reference to our Automerge shadow document for a branch.
    pub(super) async fn with_shadow_document<F, R>(
        &self,
        branch: &DocumentId,
        f: F,
    ) -> Result<R, ()>
    where
        F: AsyncFnOnce(&mut Automerge) -> R,
    {
        let sync_states = self.branch_sync_states.lock().await;
        let Some(state) = sync_states.get(branch).cloned() else {
            tracing::error!("Branch not found in sync states! Unable to run with_shadow_document.");
            return Err(());
        };
        // intentionally drop sync_states mutex here so that we can run nested with_shadow_document calls
        drop(sync_states);
        let mut state = state.lock().await;
        let Some(shadow_doc) = state.shadow_doc.as_mut() else {
            tracing::error!("Shadow document not initialized!");
            return Err(());
        };
        Ok(f(shadow_doc).await)
    }

    pub async fn get_branch_children(&self, id: &DocumentId) -> Vec<DocumentId> {
        let meta = self.metadata_state.lock().await;
        let mut result = Vec::new();
        let Some((_, m)) = meta.as_ref() else {
            return result;
        };

        for (bid, state) in m.branches.iter() {
            if let Some(forked_from) = &state.forked_from {
                if forked_from.branch() == id {
                    result.push(bid.clone());
                }
            }
        }
        result
    }

    /// Get ALL change metadata on the current branch shadow document, including those changes made before the document was created.
    pub async fn get_shadow_changes(&self, id: &DocumentId) -> Option<Vec<ChangeMetadata<'_>>> {
        self.with_shadow_document(id, async |d| {
            d.get_changes_meta(&[])
                .iter()
                // this may be slow? we could consider putting it in a struct with only the info we need like CommitInfo.
                .map(|i| i.clone().into_owned())
                .collect()
        })
        .await
        .ok()
    }

    /// Get ALL change metadata on the current branch canonical document, including those changes made before the document was created.
    pub async fn get_canonical_changes(&self, id: &DocumentId) -> Option<Vec<ChangeMetadata<'_>>> {
        let sync_states = self.branch_sync_states.lock().await;
        let Some(state) = sync_states.get(id).cloned() else {
            tracing::error!("Branch not found in sync states! Unable to run get_canonical_changes.");
            return None;
        };
        let handle = state.lock().await.canonical_doc.clone();
        tokio::task::spawn_blocking(move || {
            handle.with_document(|d| {
                d.get_changes_meta(&[])
                    .iter()
                    // this may be slow? we could consider putting it in a struct with only the info we need like CommitInfo.
                    .map(|i| i.clone().into_owned())
                    .collect()
            })
        }).await.ok()
    }

    /// Dumps a branch document to disk, at ./.backstitch/DUMP_{id}.bin
    pub async fn dump_branch_doc(&self, id: &DocumentId) {
        let path = self
            .get_project_dir()
            .join("./.backstitch/")
            .join(format!("DUMP_{id}.bin"));
        let handle = {
            let states = self.branch_sync_states.lock().await;
            let Some(state) = states.get(id) else {
                return;
            };
            let state = state.lock().await;
            state.canonical_doc.clone()
        };
        let bytes = tokio::task::spawn_blocking(move || handle.with_document(|d| d.save()))
            .await
            .unwrap();

        if let Err(e) = tokio::fs::write(path.clone(), bytes).await {
            tracing::error!("Error dumping branch {id} to {:?}: {:?}", path, e);
            return;
        }

        tracing::info!("Dumped branch {id} to {:?}", path);
    }
}
