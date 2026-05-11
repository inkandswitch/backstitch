use std::collections::{HashMap, HashSet};

use automerge::{ObjId, ObjType, ROOT, ReadDoc};
use samod::DocumentId;

use crate::{
    fs::{file_utils::FileContent, file_utils::FileSystemEvent},
    helpers::{branch::BRANCH_DOC_VERSION, doc_utils::SimpleDocReader, utils::get_changed_files},
    project::branch_db::{BranchDb, HistoryRef},
};

#[derive(Debug)]
enum ReadPathHashMapError {
    VersionTooOld,
    FilesNotFound,
    ShadowDocumentError,
}

/// Methods related to getting file changes and file contents out of documents.
impl BranchDb {
    // Utility to check for shared history between refs
    async fn shares_history(&self, earlier_ref: HistoryRef, later_ref: HistoryRef) -> bool {
        let Ok(res) = self
            .with_shadow_document(later_ref.branch(), async |d| {
                d.get_obj_id_at(ROOT, "files", earlier_ref.heads()).is_some()
                    && d.get_obj_id_at(ROOT, "files", later_ref.heads()).is_some()
            })
            .await
        else {
            return false;
        };
        return res;
    }

    /// Given two refs, checks to see if one is a direct descendant of another.
    /// If it is, it returns the more up-to-date ref.
    /// If not, it returns None.
    async fn get_descendent_ref(
        &self,
        ref_a: &HistoryRef,
        ref_b: &HistoryRef,
    ) -> Option<HistoryRef> {
        // If we can't compare them, they can't share a history
        if !ref_a.is_valid() || !ref_b.is_valid() {
            return None;
        }
        if self.shares_history(ref_a.clone(), ref_b.clone()).await {
            return Some(ref_b.clone());
        }
        if self.shares_history(ref_b.clone(), ref_a.clone()).await {
            return Some(ref_a.clone());
        }
        None
    }
    /// Read the branch-doc schema version and the `path -> hash` map visible at `ref_`.
    /// A `None` hash indicates a file entry that pre-dates the versioned hash field.
    async fn read_path_hash_map(
        &self,
        ref_: &HistoryRef,
    ) -> Result<HashMap<String, Option<Vec<u8>>>, ReadPathHashMapError> {
        let ref_clone = ref_.clone();
        self.with_shadow_document(ref_.branch(), async move |d| {
            let heads = ref_clone.heads();
            let version = d.get_int_at(ROOT, "version", heads).unwrap_or(0) as u32;
            if version < BRANCH_DOC_VERSION {
                return Err(ReadPathHashMapError::VersionTooOld);
            }
            let Some(files_id) = d.get_obj_id_at(ROOT, "files", heads) else {
                return Err(ReadPathHashMapError::FilesNotFound);
            };
            let mut out = HashMap::new();
            for path in d.keys_at(&files_id, heads) {
                let Some(entry_id) = d.get_obj_id_at(&files_id, &path, heads) else {
                    continue;
                };
                let hash = d.get_bytes_at(&entry_id, "hash", heads);
                out.insert(path, hash);
            }
            Ok(out)
        })
        .await
        .unwrap_or(Err(ReadPathHashMapError::ShadowDocumentError))
    }

    async fn compare_file_hashes(&self, old_ref: &HistoryRef, new_ref: &HistoryRef) -> Result<Vec<FileSystemEvent>, ReadPathHashMapError> {
            let old_map = self.read_path_hash_map(old_ref).await?;
            let new_map = self.read_path_hash_map(new_ref).await?;


            let mut deleted: HashSet<String> = HashSet::new();
            let mut added: HashSet<String> = HashSet::new();
            let mut modified: HashSet<String> = HashSet::new();

            for (path, old_hash) in old_map.iter() {
                match new_map.get(path) {
                    None => {
                        deleted.insert(path.clone());
                    }
                    Some(new_hash) => {
                        // Treat a missing hash on either side as "changed" to be conservative.
                        if old_hash.is_none() || new_hash.is_none() || old_hash != new_hash {
                            modified.insert(path.clone());
                        }
                    }
                }
            }
            for path in new_map.keys() {
                if !old_map.contains_key(path) {
                    added.insert(path.clone());
                }
            }

            let to_hydrate: HashSet<String> =
                added.iter().chain(modified.iter()).cloned().collect();

            let mut events: Vec<FileSystemEvent> = Vec::new();
            for path in &deleted {
                events.push(FileSystemEvent::FileDeleted(self.globalize_path(path)));
            }

            if !to_hydrate.is_empty() {
                let Some(hydrated) = self.get_files_at_ref(new_ref, &to_hydrate).await else {
                    return Err(ReadPathHashMapError::FilesNotFound);
                };
                for (path, content) in hydrated {
                    match content {
                        FileContent::Deleted => {
                            events.push(FileSystemEvent::FileDeleted(
                                self.globalize_path(&path),
                            ));
                        }
                        _ if added.contains(&path) => {
                            events.push(FileSystemEvent::FileCreated(
                                self.globalize_path(&path),
                                content,
                            ));
                        }
                        _ => {
                            events.push(FileSystemEvent::FileModified(
                                self.globalize_path(&path),
                                content,
                            ));
                        }
                    }
                }
            }

            return Ok(events);

    }

    /// Get a list of file operations between two points in Backstitch history.
    /// If one ref exists in the history of another, we can do a fast automerge diff.
    /// If they have diverged, we must do a slow file-wise diff.
    #[tracing::instrument(skip_all)]
    pub async fn get_changed_file_content_between_refs(
        &self,
        old_ref: Option<&HistoryRef>,
        new_ref: &HistoryRef,
        force_slow_diff: bool,
    ) -> Option<Vec<FileSystemEvent>> {
        tracing::info!("Getting changes between {:?} and {:?}", new_ref, old_ref);
        if !new_ref.is_valid() {
            tracing::warn!("new ref is empty, can't get changed files");
            return None;
        }

        if old_ref.is_none() || !old_ref.unwrap().is_valid() {
            tracing::info!("old heads empty, getting ALL files on branch");

            // NOTE: This returns local res:// paths, we must globalize them before exporting them to FileSystemEvents
            let files = self.get_files_at_ref(&new_ref, &HashSet::new()).await?;

            return Some(
                files
                    .into_iter()
                    .map(|(path, content)| match content {
                        FileContent::Deleted => {
                            FileSystemEvent::FileDeleted(self.globalize_path(&path))
                        }
                        _ => FileSystemEvent::FileCreated(self.globalize_path(&path), content),
                    })
                    .collect(),
            );
        }

        let old_ref = old_ref.unwrap();

        let descendent_ref = self.get_descendent_ref(old_ref, new_ref).await;

        if descendent_ref.is_none() || force_slow_diff {
            // Neither document is the descendent of the other, we can't do a fast diff.
            // If both refs are on a branch-doc version with per-file hashes, we can use the cheap hash-based slow diff
            // Otherwise, fall back to the legacy full-hydrate comparison.
            match self.compare_file_hashes(old_ref, new_ref).await {
                Ok(events) => {
                    return Some(events);
                }
                Err(err) => {
                    match err {
                        ReadPathHashMapError::VersionTooOld => {
                            tracing::warn!("Document is too old, can't do a fast diff");
                        }
                        ReadPathHashMapError::FilesNotFound => {
                            tracing::warn!("Files not found, can't do a fast diff");
                        }
                        ReadPathHashMapError::ShadowDocumentError => {
                            tracing::error!("Shadow document error, can't do a fast diff");
                        }
                    }
                }   
            }
            // Legacy fallback: one or both refs pre-date the hash schema. Hydrate everything at both refs and compare FileContent directly.
            let old_files = self.get_files_at_ref(old_ref, &HashSet::new()).await?;
            let new_files = self.get_files_at_ref(new_ref, &HashSet::new()).await?;

            let mut events = Vec::new();
            for (path, _) in old_files.iter() {
                if !new_files.contains_key(path) {
                    events.push(FileSystemEvent::FileDeleted(self.globalize_path(path)));
                }
            }
            for (path, content) in new_files {
                match content {
                    FileContent::Deleted => {
                        events.push(FileSystemEvent::FileDeleted(self.globalize_path(&path)));
                        continue;
                    }
                    _ => {}
                }
                if !old_files.contains_key(&path) {
                    events.push(FileSystemEvent::FileCreated(
                        self.globalize_path(&path),
                        content,
                    ));
                } else if &content != old_files.get(&path).unwrap() {
                    events.push(FileSystemEvent::FileModified(
                        self.globalize_path(&path),
                        content,
                    ));
                }
            }
            return Some(events);
        }

        let descendent_ref = descendent_ref.unwrap();

        // Get the patches from the later (descendant) ref
        let old_heads = old_ref.heads().clone();
        let new_heads = new_ref.heads().clone();
        let (patches, old_file_set, curr_file_set) = self
            .with_shadow_document(descendent_ref.branch(), async |d| {
                let old_files_id: Option<ObjId> = d.get_obj_id_at(ROOT, "files", &old_heads);
                let curr_files_id = d.get_obj_id_at(ROOT, "files", &new_heads);
                let old_file_set = if old_files_id.is_none() {
                    HashSet::<String>::new()
                } else {
                    d.keys_at(&old_files_id.unwrap(), &old_heads)
                        .into_iter()
                        .collect::<HashSet<String>>()
                };
                let curr_file_set = if curr_files_id.is_none() {
                    HashSet::<String>::new()
                } else {
                    d.keys_at(&curr_files_id.unwrap(), &new_heads)
                        .into_iter()
                        .collect::<HashSet<String>>()
                };
                let patches = d.diff(&old_heads, &new_heads);
                (patches, old_file_set, curr_file_set)
            })
            .await
            .ok()?;

        // Gather the information of what files changed from the patches.
        let deleted_files: HashSet<_> = old_file_set.difference(&curr_file_set).cloned().collect();
        let added_files: HashSet<_> = curr_file_set.difference(&old_file_set).cloned().collect();
        let modified_files: HashSet<_> = if patches.len() == 0 {
            HashSet::new()
        } else {
            get_changed_files(&patches)
            .into_iter()
            .filter(|f| !deleted_files.contains(f))
            .filter(|f| !added_files.contains(f))
            .collect()
        };
        let all_files: HashSet<_> = deleted_files
            .iter()
            .chain(added_files.iter())
            .chain(modified_files.iter())
            .cloned()
            .collect();

        // Valid diff, just no changes
        if all_files.len() == 0 {
            return Some(Vec::new());
        }
        // Get the files, then convert them into events using the information we gathered.
        Some(
            self.get_files_at_ref(new_ref, &all_files)
                .await?
                .into_iter()
                .map(|(path, content)| match content {
                    FileContent::Deleted => {
                        FileSystemEvent::FileDeleted(self.globalize_path(&path))
                    }
                    _ if added_files.contains(&path) => {
                        FileSystemEvent::FileCreated(self.globalize_path(&path), content)
                    }
                    _ if deleted_files.contains(&path) => {
                        FileSystemEvent::FileDeleted(self.globalize_path(&path))
                    }
                    _ => FileSystemEvent::FileModified(self.globalize_path(&path), content),
                })
                .chain(
                    deleted_files
                        .iter()
                        .map(|path| FileSystemEvent::FileDeleted(self.globalize_path(&path))),
                )
                .collect(),
        )
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

    #[tracing::instrument(skip_all)]
    pub async fn get_files_at_ref(
        &self,
        desired_ref: &HistoryRef,
        filters: &HashSet<String>,
    ) -> Option<HashMap<String, FileContent>> {
        tracing::info!("Getting files at ref {:?}", desired_ref);
        let mut files = HashMap::new();
        let mut linked_doc_ids = Vec::new();

        let filters = filters.clone();
        let desired_ref = desired_ref.clone();
        let (mut files, linked_doc_ids) = self
            .with_shadow_document(desired_ref.branch(), async |doc| {
                let files_obj_id: ObjId = doc.get_at(ROOT, "files", desired_ref.heads()).ok()??.1;
                for path in doc.keys_at(&files_obj_id, desired_ref.heads()) {
                    if !filters.is_empty() && !filters.contains(&path) {
                        continue;
                    }
                    let file_entry =
                        match doc.get_at(&files_obj_id, &path, desired_ref.heads()) {
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
                        &doc,
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
