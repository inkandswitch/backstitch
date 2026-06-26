use std::collections::HashSet;

use automerge::{Automerge, ROOT, transaction::Transactable};
use samod::DocumentId;

use crate::{
    fs::file_utils::FileContent,
    helpers::{
        branch::Branch,
        doc_utils::SimpleDocReader,
        history_ref::HistoryRef,
        utils::{ChangeType, CommitMetadata, MergeMetadata, commit_with_metadata},
    },
    project::branch_db::{BranchDb, DbError},
};

impl BranchDb {
    #[tracing::instrument(skip_all, level = "trace")]
    pub async fn create_merge_preview_branch(
        &self,
        source: &DocumentId,
        target: &DocumentId,
    ) -> Result<DocumentId, DbError> {
        // Not getting the branch state so we don't gotta clone, honestly that was probably simpler though
        let source_name = self.get_branch_name(source).await?;
        let target_name = self.get_branch_name(target).await?;

        let source_ref = self.get_latest_ref_on_branch(source).await?;
        let target_ref = self.get_latest_ref_on_branch(target).await?;

        let handle = self.repo.create(Automerge::new()).await.unwrap();
        let handle_clone = handle.clone();

        self.with_shadow_document(source, async |d| {
            handle_clone.with_document(|preview_doc| {
                let _ = preview_doc.merge(d);
            });
        })
        .await?;

        self.with_shadow_document(target, async |d| {
            handle_clone.with_document(|preview_doc| {
                let _ = preview_doc.merge(d);
            });
        })
        .await?;

        let username = self.username.lock().await.clone();
        self.add_branch_to_meta(Branch {
            name: format!("{} <- {}", target_name, source_name),
            id: handle.document_id().clone(),
            forked_from: Some(source_ref),
            merge_into: Some(target_ref),
            created_by: username.clone(),
            reverted_to: None,
        })
        .await?;
        Ok(handle.document_id().clone())
    }

    pub async fn merge_branch(
        &self,
        source: &DocumentId,
        target: &DocumentId,
    ) -> Result<(), DbError> {
        let source_state = self.get_branch_state(source).await?;

        if source == target {
            tracing::error!("cannot merge branch into itself!");
            return Ok(());
        }

        self.with_shadow_document(source, async |source_doc| {
            self.with_shadow_document(target, async |target_doc| {
                let _ = target_doc.merge(source_doc);
            })
            .await
        })
        .await??;

        // if the branch has some merge_into we know that it's a merge preview branch
        // forked_from is the original branch of the preview branch
        let forked_from = source_state.forked_from.unwrap().branch().clone();
        let merge_metadata = if source_state.merge_into.is_some() {
            match self.get_branch_state(&forked_from).await {
                Ok(original_state) => Some(MergeMetadata {
                    merged_branch_id: forked_from,
                    forked_at_heads: original_state.forked_from.unwrap().heads().clone(),
                }),
                _ => None,
            }
        } else {
            // todo: implement this case
            None
        };

        let username = self.username.lock().await.clone();
        if let Some(merge_metadata) = merge_metadata {
            let target = target.clone();
            self.with_shadow_document(&target, async |d| {
                let mut tx = d.transaction();

                // do a dummy change that we can attach some metadata to
                let changed = tx.get_int(&ROOT, "_changed").unwrap_or(0);
                let _ = tx.put(ROOT, "_changed", changed + 1);

                commit_with_metadata(
                    tx,
                    &CommitMetadata {
                        username: username.clone(),
                        branch_id: Some(target.clone()),
                        merge_metadata: Some(merge_metadata),
                        reverted_to: None,
                        changed_files: None,
                        is_setup: Some(false),
                    },
                );
            })
            .await?;
        }

        // reconcile the dummy merge commit
        let states = self.branch_sync_states.lock().await;
        let state = states
            .get(target)
            .ok_or_else(|| DbError::NoBranch(Box::new(target.clone())))?;
        self.try_reconcile_branch(state.clone()).await?;
        Ok(())
    }

    pub async fn create_revert_preview_branch(
        &self,
        branch: &DocumentId,
        ref_: &HistoryRef,
    ) -> Result<DocumentId, DbError> {
        let current_ref = self.get_latest_ref_on_branch(branch).await?;

        let handle = self.repo.create(Automerge::new()).await?;
        let handle_clone = handle.clone();

        self.with_shadow_document(branch, async |d| {
            handle_clone.with_document(|preview_doc| {
                let _ = preview_doc.merge(d);
            });
        })
        .await?;

        let username = self.username.lock().await.clone();
        self.add_branch_to_meta(Branch {
            name: format!("{} <- {}", ref_.short_heads(), current_ref.short_heads()),
            id: handle.document_id().clone(),
            forked_from: Some(current_ref.clone()),
            merge_into: None,
            created_by: username.clone(),
            reverted_to: Some(ref_.clone()),
        })
        .await?;

        let changed_files = self
            .get_changed_files_between_refs(Some(&current_ref), ref_)
            .await?;

        tracing::debug!("Revert preview changed_files: {:?}", changed_files);

        let mut changed_contents: std::collections::HashMap<String, FileContent> = self
            .get_files_at_ref(ref_, &changed_files.keys().cloned().collect())
            .await?;
        let changed_files = changed_files
            .into_iter()
            .filter_map(|(path, change)| {
                Some(match change {
                    ChangeType::Created | ChangeType::Modified => {
                        let content = changed_contents.remove(&path)?;
                        (path, Some(content))
                    }
                    ChangeType::Deleted => (path, None),
                })
            })
            .collect::<Vec<(String, Option<FileContent>)>>();

        tracing::debug!(
            "Revert preview changed_files: {:?}",
            changed_files.iter().map(|(f, _)| f).collect::<Vec<_>>()
        );
        // This is a weird hack -- we need the branch sync state to exist NOW to commit our revert...
        // ... not whenever document_watcher decides it's time.
        // We pretend there's 0 linked docs, because we're forking off a shadow doc for the preview, which had BETTER
        // not be waiting on any binary docs!!!!!
        self.update_branch_sync_state(handle.clone(), current_ref.heads().clone(), HashSet::new())
            .await?;

        self.commit_fs_changes(
            changed_files,
            &HistoryRef::new(handle.document_id().clone(), current_ref.heads().clone()),
            Some(ref_),
            false,
        )
        .await;

        Ok(handle.document_id().clone())
    }

    pub async fn confirm_revert_preview_branch(
        &self,
        preview_branch: &DocumentId,
    ) -> Result<(), DbError> {
        let preview_state = self.get_branch_state(preview_branch).await?;

        if preview_state.reverted_to.is_none() {
            return Err(DbError::BadBranchState(
                Box::new(preview_branch.clone()),
                format!("not a revert preview branch"),
            ));
        }

        let Some(target) = preview_state.forked_from else {
            return Err(DbError::BadBranchState(
                Box::new(preview_branch.clone()),
                format!("doesn't have forked_from"),
            ));
        };

        self.with_shadow_document(preview_branch, async |source_doc| {
            self.with_shadow_document(target.branch(), async |target_doc| {
                tracing::debug!("HEADS BEFORE MERGE: {:?}", target_doc.get_heads());
                tracing::debug!("PREVIEW HEADS BEFORE MERGE: {:?}", source_doc.get_heads());
                let res = target_doc.merge(source_doc).unwrap();
                tracing::debug!("NEW HEADS AFTER MERGE: {:?}", res);
                tracing::debug!("NEW HEADS AFTER MERGE2: {:?}", target_doc.get_heads());
            })
            .await
            .unwrap();
        })
        .await
        .unwrap();

        // Unlike merging, we don't need to make a dummy commit, because the revert already had a commit of the changed files.
        // Reconcile the merge anyways though.
        let states = self.branch_sync_states.lock().await;
        let state = states
            .get(target.branch())
            .ok_or_else(|| DbError::NoBranch(Box::new(target.branch().clone())))?;
        self.try_reconcile_branch(state.clone()).await?;
        Ok(())
    }
}
