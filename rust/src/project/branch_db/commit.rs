use std::collections::HashMap;

use automerge::{Automerge, ObjId, ObjType, ROOT, ReadDoc, transaction::Transaction};
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

enum Entry {
    Binary {
        path: String,
        handle: DocHandle,
        hash: blake3::Hash,
    },
    Text {
        path: String,
        content: String,
        hash: blake3::Hash,
    },
    Scene {
        path: String,
        scene: Box<GodotScene>,
        hash: blake3::Hash,
    },
    Delete {
        path: String,
    },
}

// Methods related to committing changes to a branch in [BranchDb].
impl BranchDb {
    fn commit_entries(tx: &mut Transaction<'_>, entries: Vec<Entry>) -> Vec<ChangedFile> {
        let mut changes: Vec<ChangedFile> = Vec::new();
        let files = tx.get_obj_id(ROOT, "files").unwrap();

        for entry in entries {
            match entry {
                Entry::Binary { path, handle, hash } => {
                    // get the change flag
                    let change_type = match tx.get(&files, &path) {
                        Ok(Some(_)) => ChangeType::Modified,
                        _ => ChangeType::Created,
                    };

                    let file_entry = tx.put_object(&files, &path, ObjType::Map).unwrap();
                    let _ = tx.put(
                        &file_entry,
                        "url",
                        format!("automerge:{}", handle.document_id()),
                    );
                    let _ = tx.put(&file_entry, "hash", hash.as_bytes().to_vec());

                    changes.push(ChangedFile { path, change_type });
                }
                Entry::Text {
                    path,
                    content,
                    hash,
                } => {
                    // get existing file url or create new one
                    let (file_entry, change_type) = match tx.get(&files, &path) {
                        Ok(Some((automerge::Value::Object(ObjType::Map), file_entry))) => {
                            (file_entry, ChangeType::Modified)
                        }
                        _ => (
                            tx.put_object(&files, &path, ObjType::Map).unwrap(),
                            ChangeType::Created,
                        ),
                    };

                    changes.push(ChangedFile { path, change_type });

                    // delete url in file entry if it previously had one
                    let _ = tx.delete(&file_entry, "url");

                    // delete structured content in file entry if it previously had one
                    let _ = tx.delete(&file_entry, "structured_content");

                    // either get existing text or create new text
                    let content_key = match tx.get(&file_entry, "content") {
                        Ok(Some((automerge::Value::Object(ObjType::Text), content_key))) => {
                            content_key
                        }
                        _ => tx
                            .put_object(&file_entry, "content", ObjType::Text)
                            .expect("there was an automerge error putting the object"),
                    };
                    let _ = tx.update_text(&content_key, &content);
                    let _ = tx.put(&file_entry, "hash", hash.as_bytes().to_vec());
                }
                Entry::Scene { path, scene, hash } => {
                    // get the change flag
                    let change_type = match tx.get(&files, &path) {
                        Ok(Some(_)) => ChangeType::Modified,
                        _ => ChangeType::Created,
                    };

                    let scene_file = tx
                        .get_obj_id(&files, &path)
                        .unwrap_or_else(|| tx.put_object(&files, &path, ObjType::Map).unwrap());

                    // If this happens, it's a bug with the caller... but check, for debug purposes.
                    if let Some(old_hash) = Self::get_existing_hash(tx, &scene_file)
                        && old_hash == hash
                    {
                        tracing::error!(
                            "Scene {} hash is the same as stored! Committing anyways.",
                            path
                        );
                    }

                    autosurgeon::reconcile_prop(tx, &scene_file, "structured_content", scene)
                        .unwrap_or_else(|e| {
                            tracing::error!("error reconciling scene: {}", e);
                            panic!("error reconciling scene: {}", e);
                        });
                    let _ = tx.put(&scene_file, "hash", hash.as_bytes().to_vec());
                    changes.push(ChangedFile { path, change_type });
                }
                Entry::Delete { path } => {
                    let _ = tx.delete(&files, &path);
                    changes.push(ChangedFile {
                        path,
                        change_type: ChangeType::Deleted,
                    });
                }
            }
        }
        changes
    }

    /// Commit a list of files from the filesystem, while ensuring they've actually been changed before including them.
    /// Returns a HistoryRef referring to the new heads. We may or may not have reconciled to the canonical doc at this point.
    /// Also returns a map of the committed files to the hash.
    pub async fn commit_fs_changes(
        &self,
        files: Vec<(String, Option<FileContent>)>,
        ref_: &HistoryRef,
        revert: Option<&HistoryRef>,
        is_checking_in: bool,
    ) -> Option<(HistoryRef, HashMap<String, Option<blake3::Hash>>)> {
        tracing::info!("Attempting to commit {} changes...", files.len());

        let count = files.len();
        let username = self.resolve_username().await;

        // TODO: we should have `FileSystemEvent` objects as parameters, and they should store a precomputed hash; then we can just pass them in here.
        let mut entries: Vec<Entry> = Vec::new();
        let mut ret = HashMap::new();

        for (path, content) in files {
            // This is fairly cheap from my estimation.
            // If this becomes a bottleneck, we can reuse the previously computed hash by having callers provide file hashes.
            // If we do this, though, we still have to compute scene hashes when putting things into Automerge.
            let hash = content.as_ref().map(|c| c.to_hash());
            ret.insert(path.clone(), hash);
            match content {
                Some(FileContent::Binary(content)) => {
                    let handle = self.create_new_binary_doc(content).await;
                    entries.push(Entry::Binary {
                        path,
                        handle,
                        hash: hash.unwrap(),
                    });
                }
                Some(FileContent::String(content)) => {
                    entries.push(Entry::Text {
                        path,
                        content,
                        hash: hash.unwrap(),
                    });
                }
                Some(FileContent::Scene(godot_scene)) => {
                    entries.push(Entry::Scene {
                        path,
                        scene: Box::new(*godot_scene),
                        hash: hash.unwrap(),
                    });
                }
                None => {
                    entries.push(Entry::Delete { path });
                }
            }
        }

        let sync_states = self.branch_sync_states.lock().await;
        let Some(state_arc) = sync_states.get(ref_.branch()) else {
            tracing::error!("Sync state doesn't exist for branch; can't commit changes.");
            return None;
        };

        let mut state = state_arc.clone().lock_owned().await;

        let ref_clone = ref_.clone();
        let revert = revert.cloned();
        // We always commit to the shadow doc, and later attempt reconciliation.
        let Some(d) = state.shadow_doc.as_mut() else {
            tracing::error!("Shadow doc not initialized for branch; can't commit changes.");
            return None;
        };

        if d.get_heads() != *ref_clone.heads() {
            tracing::warn!(
                "Expected heads are different than current heads. The diffing will probably be wrong!"
            );
        }

        // This work is quite expensive
        let new_heads = tokio::task::spawn_blocking(move || {
            // We always commit to the shadow doc, and later attempt reconciliation.
            let Some(d) = state.shadow_doc.as_mut() else {
                tracing::error!("Shadow doc not initialized for branch; can't commit changes.");
                return None;
            };

            if d.get_heads() != *ref_clone.heads() {
                tracing::warn!(
                    "Expected heads are different than current heads. The diffing will probably be wrong!"
                );
            }
            let mut tx = d.transaction();

            let changes = Self::commit_entries(&mut tx, entries);

            let res = commit_with_metadata(
                tx,
                &CommitMetadata {
                    username: username.clone(),
                    // if we're reverting, fake the branch so the change will show up once merged
                    branch_id: match &revert {
                        Some(revert) => Some(revert.branch().clone()),
                        None => Some(ref_clone.branch().clone()),
                    },
                    merge_metadata: None,
                    reverted_to: revert.map(|r| r.heads().clone()),
                    changed_files: Some(changes),
                    is_setup: Some(is_checking_in),
                },
            );

            let new_heads = d.get_heads();

            if new_heads.first() != res.as_ref() {
                tracing::error!(
                    "Document heads {:?} different from commit result {:?}!",
                    new_heads,
                    res
                );
            }
            Some(new_heads)
        })
        .await
        .unwrap();

        let new_heads = new_heads?;

        // Unlock state, then attempt a reconcile.
        // The reconcile may fail if we are currently syncing binary docs.
        // That's OK; once the binary doc sync finishes, it will trigger a reconcile to canonical.
        // In the mean time, we can continue committing to the shadow doc.
        tracing::debug!("Attempting reconcile...");
        self.try_reconcile_branch(state_arc.clone())
            .await
            .inspect_err(|e| tracing::error!("Error reconciling after commit: {e}"))
            .ok();

        tracing::info!("Committed {} files.", count);

        if new_heads == *ref_.heads() {
            tracing::error!(
                "Document heads {:?} didn't change after committing!",
                new_heads
            );
        }

        Some((HistoryRef::new(ref_.branch().clone(), new_heads), ret))
    }

    fn get_existing_hash(tx: &Transaction<'_>, obj_id: &ObjId) -> Option<blake3::Hash> {
        if let Ok(hashes) = tx.get_all(obj_id, "hash") {
            // If there are multiple hashes here, it means we can't rely on it and have to do a contentwise comparison.
            if hashes.len() == 1 {
                let hash = &hashes.first().unwrap().0;
                if let Some(bytes) = hash.to_bytes()
                    && let Ok(hash) = blake3::Hash::from_slice(bytes)
                {
                    return Some(hash);
                }
            }
        };
        None
    }

    pub async fn create_new_binary_doc(&self, content: Vec<u8>) -> DocHandle {
        tracing::info!("Creating new binary doc...");
        let handle = self.repo.create(Automerge::new()).await.unwrap();

        let username = self.resolve_username().await;

        // we're allowed to transact in the background: nobody needs this to exist yet.
        let h = handle.clone();
        tokio::task::spawn_blocking(move || {
            h.with_document(|d| {
                let mut tx = d.transaction();
                let _ = tx.put(ROOT, "content", content);
                commit_with_metadata(
                    tx,
                    &CommitMetadata {
                        username,
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
        handle
    }
}
