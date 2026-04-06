use std::collections::{HashMap, HashSet};

use automerge::{ObjId, ObjType, ROOT, ReadDoc};
use samod::DocumentId;

use crate::{
    fs::{file_utils::FileContent, file_utils::FileSystemEvent},
    helpers::{doc_utils::SimpleDocReader, utils::get_changed_files},
    project::branch_db::{BranchDb, HistoryRef},
};

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
    // TODO (Lilith): During profiling, look at this method. It seems quite improvable.
    // Here's my idea:
    // In each branch doc, store an md5 hash of the file contents.
    // Then, we can get a vector of changed files and their operations by comparing the two tracked_files
    // using a single with_document call. With that, we can construct a filter to make the hydration lighter.
    // That would significantly improve the slow diff. I'm not sure if that would be faster than the fast diff.
    // (But if they're equivalent, not dealing with patches significantly simplifies code.)

    // Afterwards, another possible improvement here:
    // Instead of fetching the file content, we could just get the changed files.
    // Then, later code could fetch the file content asynchronously.

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
            // neither document is the descendent of the other, we can't do a fast diff,
            // we need to do it the slow way; get the files from both docs
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
        let modified_files: HashSet<_> = get_changed_files(&patches)
            .into_iter()
            .filter(|f| !deleted_files.contains(f))
            .filter(|f| !added_files.contains(f))
            .collect();
        let all_files: HashSet<_> = deleted_files
            .iter()
            .chain(added_files.iter())
            .chain(modified_files.iter())
            .cloned()
            .collect();

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
