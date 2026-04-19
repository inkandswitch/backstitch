use std::collections::{HashMap, HashSet};

use automerge::{Automerge, ObjType, ROOT, ReadDoc};
use autosurgeon::Doc;
use samod::DocHandle;

use crate::{
    fs::file_utils::FileContent,
    helpers::{
        doc_utils::SimpleDocReader,
        utils::{ChangeType, ChangedFile, CommitMetadata, commit_with_metadata},
    },
    parser::godot_parser::GodotScene,
    project::branch_db::{BranchDb, HistoryRef},
};

// Methods related to committing changes to a branch in [BranchDb].
impl BranchDb {
    /// Commit a list of files from the filesystem, while ensuring they've actually been changed before including them.
    /// Returns a HistoryRef referring to the new heads. We may or may not have reconciled to the canonical doc at this point.
    pub async fn commit_fs_changes(
        &self,
        files: Vec<(String, FileContent)>,
        ref_: &HistoryRef,
        revert: Option<&HistoryRef>,
        is_checking_in: bool,
    ) -> Option<HistoryRef> {
        tracing::info!("Attempting to commit changes...");
        // Only commit files that have actually changed
        // TODO: We may be able to use notify's compare file hash system instead? Or in addition to this?
        let files = self.filter_changed_files(ref_, files).await;
        let count = files.len();
        let username = self.username.lock().await.clone();

        if count == 0 {
            tracing::info!("No actual changes found; not committing.");
            return None;
        }

        // TODO: we should have `FileSystemEvent` objects as parameters, and they should store a precomputed hash; then we can just pass them in here.
        let mut binary_entries: Vec<(String, DocHandle, blake3::Hash)> = Vec::new();
        let mut text_entries: Vec<(String, String, blake3::Hash)> = Vec::new();
        let mut scene_entries: Vec<(String, GodotScene, blake3::Hash)> = Vec::new();
        let mut deleted_entries: Vec<String> = Vec::new();

        for (path, content) in files {
            let hash = content.to_hash();
            match content {
                FileContent::Binary(content) => {
                    let handle = self.create_new_binary_doc(content).await;
                    binary_entries.push((path, handle, hash));
                }
                FileContent::String(content) => {
                    text_entries.push((path, content, hash));
                }
                FileContent::Scene(godot_scene) => {
                    scene_entries.push((path, godot_scene, hash));
                }
                FileContent::Deleted => {
                    deleted_entries.push(path);
                }
            }
        }

        let sync_states = self.branch_sync_states.lock().await;
        let Some(state_arc) = sync_states.get(ref_.branch()) else {
            tracing::error!("Sync state doesn't exist for branch; can't commit changes.");
            return None;
        };

        let mut state = state_arc.lock().await;

        // We always commit to the shadow doc, and later attempt reconciliation.
        let Some(d) = state.shadow_doc.as_mut() else {
            tracing::error!("Shadow doc not initialized for branch; can't commit changes.");
            return None;
        };

        let mut tx = d.transaction();

        let mut changes: Vec<ChangedFile> = Vec::new();
        let files = tx.get_obj_id(ROOT, "files").unwrap();

        // write text entries to doc
        for (path, content, hash) in text_entries {
            // get existing file url or create new one
            let (file_entry, change_type) = match tx.get(&files, &path) {
                Ok(Some((automerge::Value::Object(ObjType::Map), file_entry))) => {
                    (file_entry, ChangeType::Modified)
                }
                _ => (
                    tx.put_object(&files, &path, ObjType::Map).unwrap(),
                    ChangeType::Added,
                ),
            };

            changes.push(ChangedFile { path, change_type });

            // delete url in file entry if it previously had one
            if let Ok(Some((_, _))) = tx.get(&file_entry, "url") {
                let _ = tx.delete(&file_entry, "url");
            }

            // delete structured content in file entry if it previously had one
            if let Ok(Some((_, _))) = tx.get(&file_entry, "structured_content") {
                let _ = tx.delete(&file_entry, "structured_content");
            }

            // either get existing text or create new text
            let content_key = match tx.get(&file_entry, "content") {
                Ok(Some((automerge::Value::Object(ObjType::Text), content))) => content,
                _ => tx
                    .put_object(&file_entry, "content", ObjType::Text)
                    .unwrap(),
            };
            let _ = tx.update_text(&content_key, &content);
            let _ = tx.put(&file_entry, "hash", hash.as_bytes().to_vec());
        }

        // write scene entries to doc
        for (path, godot_scene, hash) in scene_entries {
            // get the change flag
            let change_type = match tx.get(&files, &path) {
                Ok(Some(_)) => ChangeType::Modified,
                _ => ChangeType::Added,
            };

            let scene_file = tx
                .get_obj_id(&files, &path)
                .unwrap_or_else(|| tx.put_object(&files, &path, ObjType::Map).unwrap());
            autosurgeon::reconcile_prop(&mut tx, &scene_file, "structured_content", godot_scene)
                .unwrap_or_else(|e| {
                    tracing::error!("error reconciling scene: {}", e);
                    panic!("error reconciling scene: {}", e);
                });
            let _ = tx.put(&scene_file, "hash", hash.as_bytes().to_vec());
            changes.push(ChangedFile { path, change_type });
        }

        // write binary entries to doc
        for (path, binary_doc_handle, hash) in binary_entries {
            // get the change flag
            let change_type = match tx.get(&files, &path) {
                Ok(Some(_)) => ChangeType::Modified,
                _ => ChangeType::Added,
            };

            let file_entry = tx.put_object(&files, &path, ObjType::Map).unwrap();
            let _ = tx.put(
                &file_entry,
                "url",
                format!("automerge:{}", &binary_doc_handle.document_id()),
            );
            let _ = tx.put(&file_entry, "hash", hash.as_bytes().to_vec());

            changes.push(ChangedFile { path, change_type });
        }

        for path in deleted_entries {
            let _ = tx.delete(&files, &path);
            changes.push(ChangedFile {
                path,
                change_type: ChangeType::Removed,
            });
        }

        let res = commit_with_metadata(
            tx,
            &CommitMetadata {
                username: username.clone(),
                // if we're reverting, fake the branch so the change will show up once merged
                branch_id: match revert {
                    Some(revert) => Some(revert.branch().clone()),
                    None => Some(ref_.branch().clone()),
                },
                merge_metadata: None,
                reverted_to: revert.map(|r| r.heads().clone()),
                changed_files: Some(changes),
                is_setup: Some(is_checking_in),
            },
        );

        let new_heads = d.get_heads();

        if new_heads.get(0) != res.as_ref() {
            tracing::error!("Document heads {:?} different from commit result {:?}!", new_heads, res);
        }

        // Unlock state, then attempt a reconcile.
        // The reconcile may fail if we are currently syncing binary docs.
        // That's OK; once the binary doc sync finishes, it will trigger a reconcile to canonical.
        // In the mean time, we can continue committing to the shadow doc.
        drop(state);
        self.try_reconcile_branch(state_arc.clone()).await;

        tracing::info!("Committed {} files.", count);

        if new_heads == *ref_.heads() {
            tracing::error!("Document heads {:?} didn't change after committing!", new_heads);
        }

        return Some(HistoryRef::new(ref_.branch().clone(), new_heads));
    }

    // Filter a list of files to those changed compared to a given ref.
    async fn filter_changed_files(
        &self,
        ref_: &HistoryRef,
        files: Vec<(String, FileContent)>,
    ) -> Vec<(String, FileContent)> {
        // Only load files matching those we've provided
        let filter = files
            .iter()
            .map(|(path, _)| path.to_string())
            .collect::<HashSet<String>>();

        // Check our stored files
        let stored_files = self
            .get_files_at_ref(&ref_, &filter)
            .await
            .unwrap_or(HashMap::new());

        // Filter out files that haven't actually changed
        files
            .into_iter()
            .filter_map(|(path, content)| {
                let path = path.to_string();
                let stored_content = stored_files.get(&path);
                if let Some(stored_content) = stored_content {
                    if stored_content == &content {
                        return None;
                    }
                }
                Some((path, content))
            })
            .collect()
    }

    pub async fn create_new_binary_doc(&self, content: Vec<u8>) -> DocHandle {
        tracing::info!("Creating new binary doc...");
        let handle = self.repo.create(Automerge::new()).await.unwrap();

        let username = self.username.lock().await.clone();

        // we're allowed to transact in the background: nobody needs this to exist yet.
        let h = handle.clone();
        tokio::task::spawn_blocking(move || {
            h.with_document(|d| {
                let mut tx = d.transaction();
                let _ = tx.put(ROOT, "content", content);
                commit_with_metadata(
                    tx,
                    &CommitMetadata {
                        username: username,
                        branch_id: None,
                        merge_metadata: None,
                        reverted_to: None,
                        changed_files: None,
                        is_setup: Some(false),
                    },
                );
            });
        });

        // TODO: actually store the handle
        return handle;
    }
}
