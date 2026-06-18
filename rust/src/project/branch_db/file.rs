use std::collections::{HashMap, HashSet};

use automerge::{ObjId, ObjType, ROOT, ReadDoc, ValueRef};
use autosurgeon::Doc;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use samod::DocumentId;

use crate::{
    fs::file_utils::FileContent,
    helpers::{
        doc_utils::SimpleDocReader,
        utils::{ChangeType, CommitMetadata, commit_with_metadata},
    },
    project::{
        branch_db::{BranchDb, HistoryRef},
        fs::fs_traversal::FileSystemTraversal,
    },
};

/// Methods related to getting file changes and file contents out of documents.
impl BranchDb {
    /// Get the hash index of the file system at a given ref.
    /// This gets stored hashes from the doc. In most cases, slow hash retreival is unnecessary.
    /// The following circumstances will cause a slow hash retrieval:
    /// 1. The document predates hash insertion
    /// 2. There was a merge conflict, causing multiple hashes (all incorrect) to be available
    /// 3. The hash is otherwise unavailable or missing from the document
    // TODO: It would be smart, here, to re-insert computed hashes if they were missing or invalid.
    // We're not doing that right now because I don't know the side effects; they could be bad.
    #[tracing::instrument(skip_all, level = "trace")]
    pub async fn get_hash_index(&self, ref_: &HistoryRef) -> Option<HashMap<String, blake3::Hash>> {
        // TODO (Lilith): Should we not use the shadow document here? The canonical might be stable,
        // but I haven't thought through the consequences of using either here.

        enum PendingHash {
            Hash(blake3::Hash),
            Linked(DocumentId),
        }

        let ref_clone = ref_.clone();
        let hashes = self
            .with_shadow_document(ref_.branch(), async move |d| {
                let heads = ref_clone.heads();
                let Some(files_id) = d.get_obj_id_at(ROOT, "files", heads) else {
                    tracing::error!("files not found at ref {ref_clone}!");
                    return None;
                };

                let entries: Vec<(String, ObjId)> = d
                    .map_range_at(&files_id, .., heads)
                    .filter_map(|item| {
                        let id = item.id();
                        // if it's a scalar for some reason, ignore this entry
                        match item.value {
                            ValueRef::Object(_) => Some((item.key.into_owned(), id)),
                            _ => None,
                        }
                    })
                    .collect();

                // Using rayon for this, to parallelize the key retrieval from the document (not just hashing!!)
                // Maps to a PendingHash() with either a hash value or a request to look for a linked doc for slow hashing.
                // Also maps to a bool indicating whether to re-insert the computed hash after slow hashing (useful for old or broken docs).
                Some(
                    entries
                        .into_par_iter()
                        .filter_map(|(path, entry_id)| {
                            // First, try and access the quick hash from the doc.
                            let should_reinsert;
                            if let Ok(hashes) = d.get_all_at(&entry_id, "hash", heads) {
                                // If there are multiple hashes here, it means there are conflicts!
                                // i.e. the hash might be totally invalid; we need to calculate it manually.
                                if hashes.len() == 1 {
                                    let hash = &hashes.first().unwrap().0;
                                    if let Some(bytes) = hash.to_bytes() {
                                        if let Ok(hash) = blake3::Hash::from_slice(bytes) {
                                            return Some((path, (PendingHash::Hash(hash), false)));
                                        } else {
                                            tracing::error!("Couldn't read hash for {path}");
                                            should_reinsert = true;
                                        }
                                    } else {
                                        tracing::error!("Couldn't convert hash for {path}");
                                        should_reinsert = true;
                                    }
                                } else if hashes.len() > 1 {
                                    tracing::debug!("Multiple heads found {path}");
                                    should_reinsert = false; // don't do this, I think it's unsafe because multiple clients would thrash on it
                                } else {
                                    tracing::debug!("No stored hash for {path}");
                                    should_reinsert = true;
                                }
                            } else {
                                tracing::debug!("Couldn't get hashes for {path}");
                                should_reinsert = true; // I don't think this happens but might as well
                            }

                            // If there were any issues, fallback to the slow hash.
                            tracing::debug!("Using slow hash for {path}");
                            match FileContent::hydrate_content_at(entry_id, d, &path, heads) {
                                Ok(content) => {
                                    tracing::debug!("Done logging {path}");
                                    Some((
                                        path,
                                        (PendingHash::Hash(content.to_hash()), should_reinsert),
                                    ))
                                }
                                Err(res) => match res {
                                    Ok(id) => {
                                        tracing::debug!("Done logging {path}");
                                        Some((path, (PendingHash::Linked(id), should_reinsert)))
                                    }
                                    Err(error_msg) => {
                                        tracing::error!("error: {:?}", error_msg);
                                        None
                                    }
                                },
                            }
                        })
                        .collect::<HashMap<String, (PendingHash, bool)>>(),
                )
            })
            .await
            .ok()??;

        // Resolve binary files
        let mut new_hashes = HashMap::new();
        let mut reinserting = false;
        for (path, (pending_hash, should_reinsert)) in hashes {
            if self.should_ignore(&self.globalize_path(&path), false) {
                continue;
            }
            if should_reinsert {
                reinserting = true;
            }
            let hash = match pending_hash {
                PendingHash::Hash(hash) => hash,
                PendingHash::Linked(document_id) => {
                    tracing::info!("Hashing linked file {document_id}");
                    // It may be wise to do this in parallel too... but this shouldn't be a frequent case.
                    let Some(content) = self.get_linked_file(&document_id).await else {
                        tracing::error!("Could not get linked file for hashing {path}");
                        continue;
                    };
                    content.to_hash()
                }
            };
            new_hashes.insert(path, (hash, should_reinsert));
        }

        if reinserting {
            tracing::info!("Found broken hashes; reinserting");
            let ref_clone = ref_.clone();
            let username = self.username.lock().await.clone();
            let new_hashes = new_hashes.clone();
            self.with_shadow_document(ref_.branch(), async move |d| {
                let Some(files_id) = d.get_obj_id_at(ROOT, "files", ref_clone.heads()) else {
                    tracing::error!("files not found at ref {ref_clone}!");
                    return;
                };

                let entries: Vec<(String, ObjId)> = d
                    .map_range_at(&files_id, .., ref_clone.heads())
                    .filter_map(|item| {
                        let id = item.id();
                        // if it's a scalar for some reason, ignore this entry
                        match item.value {
                            ValueRef::Object(_) => Some((item.key.into_owned(), id)),
                            _ => None,
                        }
                    })
                    .collect();
                let mut tx = d.transaction();
                for (path, id) in entries {
                    let Some((hash, should_reinsert)) = new_hashes.get(&path) else {
                        continue;
                    };
                    if !should_reinsert {
                        continue;
                    }

                    let _ = tx.put(id, "hash", hash.as_bytes().to_vec());
                }

                commit_with_metadata(
                    tx,
                    &CommitMetadata {
                        username,
                        branch_id: Some(ref_clone.branch().clone()),
                        merge_metadata: None,
                        reverted_to: None,
                        changed_files: None,
                        is_setup: Some(false),
                    },
                );
            })
            .await
            .unwrap();
        }

        Some(
            new_hashes
                .into_iter()
                .map(|(path, (hash, _))| (path, hash))
                .collect(),
        )
    }

    /// Get a list of file operations between two points in Backstitch history.
    /// Returns paths in local res:// format.
    #[tracing::instrument(skip_all, level = "trace")]
    pub async fn get_changed_files_between_refs(
        &self,
        old_ref: Option<&HistoryRef>,
        new_ref: &HistoryRef,
    ) -> Option<HashMap<String, ChangeType>> {
        tracing::info!("Getting changes between {:?} and {:?}", new_ref, old_ref);
        if !new_ref.is_valid() {
            tracing::warn!("new ref is empty, can't get changed files");
            return None;
        }

        let new_index = self.get_hash_index(new_ref).await?;

        // If the old heads are empty, we always return all content.
        if old_ref.is_none() || !old_ref.unwrap().is_valid() {
            tracing::info!("old heads empty, getting ALL files on branch");

            return Some(
                new_index
                    .into_keys()
                    .map(|path| (path, ChangeType::Created))
                    .collect(),
            );
        }

        let old_ref = old_ref.unwrap();
        let old_index = self.get_hash_index(old_ref).await?;

        Some(FileSystemTraversal::get_file_changes(old_index, new_index))
    }

    async fn get_linked_file(&self, doc_id: &DocumentId) -> Option<FileContent> {
        let handle = self
            .binary_states
            .lock()
            .await
            .get(doc_id)
            .cloned()
            .flatten();
        let Some(handle) = handle else {
            return None;
        };

        tokio::task::spawn_blocking(move || {
            handle.with_document(|d| match d.get(ROOT, "content") {
                Ok(Some((value, _))) if value.is_bytes() => {
                    Some(FileContent::Binary(value.into_bytes().unwrap()))
                }
                Ok(Some((value, _))) if value.is_str() => {
                    Some(FileContent::String(value.into_string().unwrap()))
                }
                _ => None,
            })
        })
        .await
        .unwrap()
    }

    #[tracing::instrument(skip_all, level = "trace")]
    pub async fn get_files_at_ref(
        &self,
        desired_ref: &HistoryRef,
        filters: &HashSet<String>,
    ) -> Option<HashMap<String, FileContent>> {
        if filters.is_empty() {
            tracing::debug!("Skipping get_files_at_ref; filters empty");
            return None;
        }
        tracing::info!("Getting {:?} files at ref {:?}", filters.len(), desired_ref);
        tracing::trace!("Filters: {:?}", filters);
        let mut files = HashMap::new();
        let mut linked_doc_ids = Vec::new();

        let filters = filters.clone();
        let desired_ref = desired_ref.clone();
        let (mut files, linked_doc_ids) = self
            .with_shadow_document(desired_ref.branch(), async |doc| {
                let files_obj_id: ObjId = doc.get_at(ROOT, "files", desired_ref.heads()).ok()??.1;
                for path in doc.keys_at(&files_obj_id, desired_ref.heads()) {
                    if !filters.contains(&path) {
                        continue;
                    }
                    let file_entry = match doc.get_at(&files_obj_id, &path, desired_ref.heads()) {
                        Ok(Some((automerge::Value::Object(ObjType::Map), file_entry))) => {
                            file_entry
                        }
                        _ => {
                            tracing::error!("failed to get file entry for {:?}", path);
                            continue;
                        }
                    };

                    match FileContent::hydrate_content_at(
                        file_entry,
                        doc,
                        &path,
                        desired_ref.heads(),
                    ) {
                        Ok(content) => {
                            files.insert(path, content);
                        }
                        Err(res) => match res {
                            Ok(id) => {
                                linked_doc_ids.push((id, path));
                            }
                            Err(error_msg) => {
                                tracing::error!("error: {:?}", error_msg);
                            }
                        },
                    };
                }
                Some((files, linked_doc_ids))
            })
            .await
            .ok()??;

        for (doc_id, path) in linked_doc_ids {
            let linked_file_content: Option<FileContent> = self.get_linked_file(&doc_id).await;
            if let Some(file_content) = linked_file_content {
                files.insert(path, file_content);
            } else {
                tracing::warn!("linked file {:?} not found", path);
            }
        }

        return Some(files);
    }
}
