use std::collections::HashMap;

use automerge::{Automerge, ROOT, transaction::Transactable};
use autosurgeon::{hydrate, reconcile};
use sedimentree_core::id::SedimentreeId;

use crate::{
    helpers::{
        branch::{BRANCH_DOC_VERSION, Branch, BranchesMetadataDoc, GodotProjectDoc},
        utils::{CommitMetadata, commit_with_metadata},
    },
    project::{
        branch_db::{BranchDb, DbError, HistoryRef},
        doc_db::repo::RepoError,
    },
};

// Methods related to branch and document management on a [BranchDb].
impl BranchDb {
    /// Create a new metadata document, and a new main branch, and return the handle of the metadata document.
    /// Checks out the initial commit of the main branch automatically.
    pub async fn create_metadata_doc(&self) -> Result<SedimentreeId, DbError> {
        tracing::info!("Creating new metadata doc...");
        let username = self.username.lock().await.clone();

        // Because we always change the checked out ref after creating, we need to lock this in write mode.
        let r = self.get_checked_out_ref_mut();
        let mut checked_out_ref = r.write().await;

        // Create new main branch doc
        let main_handle = self.repo.create().await?;
        let main_handle_clone = main_handle.clone();
        let username_clone = username.clone();

        let new_heads = self
            .repo
            .with_document(&main_handle_clone, async |d| {
                let mut tx = d.transaction();
                let _ = reconcile(
                    &mut tx,
                    GodotProjectDoc {
                        files: HashMap::new(),
                        state: HashMap::new(),
                    },
                );
                let _ = tx.put(ROOT, "version", BRANCH_DOC_VERSION as i64);
                commit_with_metadata(
                    tx,
                    &CommitMetadata {
                        username: username_clone,
                        branch_id: Some(main_handle_clone.clone()),
                        merge_metadata: None,
                        reverted_to: None,
                        changed_files: None,
                        is_setup: Some(true),
                    },
                );
                d.get_heads()
            })
            .await?;

        *checked_out_ref = Some(HistoryRef::new(main_handle.clone(), new_heads));

        let main_doc_id = main_handle.clone();
        let branches = HashMap::from([(
            main_doc_id.clone(),
            Branch {
                name: String::from("main"),
                id: main_handle.clone(),
                forked_from: None,
                merge_into: None,
                created_by: username.clone(),
                reverted_to: None,
            },
        )]);

        // create new branches metadata doc
        let metadata_handle = self.repo.create().await.unwrap();
        let metadata_handle_clone = metadata_handle.clone();
        tokio::task::spawn_blocking(move || {
            self.repo.with_document(|d| {
                let mut tx = d.transaction();
                let _ = reconcile(
                    &mut tx,
                    BranchesMetadataDoc {
                        main_doc_id,
                        branches,
                    },
                );
                commit_with_metadata(
                    tx,
                    &CommitMetadata {
                        username,
                        branch_id: None,
                        merge_metadata: None,
                        reverted_to: None,
                        changed_files: None,
                        is_setup: Some(true),
                    },
                );
            });
        })
        .await?;
        Ok(metadata_handle_clone)
    }

    #[tracing::instrument(skip_all, level = "trace")]
    pub(super) async fn add_branch_to_meta(&self, branch: Branch) -> Result<(), DbError> {
        let meta_handle = {
            let meta = self.metadata_state.lock().await;
            meta.as_ref().ok_or(DbError::NoMetadataState)?.0.clone()
        };

        let username = self.username.lock().await.clone();
        self.repo
            .with_document(
                &meta_handle,
                async |d: &mut Automerge| -> Result<_, DbError> {
                    let mut branches_metadata: BranchesMetadataDoc = hydrate(d)?;
                    let mut tx = d.transaction();
                    branches_metadata.branches.insert(branch.id.clone(), branch);
                    reconcile(&mut tx, branches_metadata)?;
                    commit_with_metadata(
                        tx,
                        &CommitMetadata {
                            username,
                            branch_id: None,
                            merge_metadata: None,
                            reverted_to: None,
                            changed_files: None,
                            is_setup: Some(true),
                        },
                    );
                    Ok(())
                },
            )
            .await?;
        Ok(())
    }

    async fn remove_branch_from_meta(&self, branch: SedimentreeId) -> Result<(), DbError> {
        let meta_handle = {
            let meta = self.metadata_state.lock().await;
            meta.as_ref().ok_or(DbError::NoMetadataState)?.0.clone()
        };
        let branch_clone = branch.clone();
        let username = self.username.lock().await.clone();
        tokio::task::spawn_blocking(move || {
            meta_handle.with_document(|d| {
                let mut tx = d.transaction();
                let mut branches_metadata: BranchesMetadataDoc = hydrate(&tx).unwrap();
                branches_metadata.branches.remove(&branch_clone);
                let _ = reconcile(&mut tx, branches_metadata);
                commit_with_metadata(
                    tx,
                    &CommitMetadata {
                        username,
                        branch_id: None,
                        merge_metadata: None,
                        reverted_to: None,
                        changed_files: None,
                        is_setup: Some(true),
                    },
                );
            });
        })
        .await?;
        Ok(())
    }

    // delete branch isn't fully implemented right now deletes are not propagated to the frontend
    // right now this is just useful to clean up merge preview branches
    #[tracing::instrument(skip_all, level = "trace")]
    pub async fn delete_branch(&self, branch: &SedimentreeId) -> Result<(), DbError> {
        self.remove_branch_from_meta(branch.clone()).await
    }

    async fn clone_branch(&self, branch: &SedimentreeId) -> Result<DocHandle, DbError> {
        Ok(self
            .with_shadow_document(branch, async |d| self.repo.create(d.clone()).await)
            .await??)
    }

    // TODO: This would be more versatile if we gave a HistoryRef instead of a branch.
    // That way it might work for reverts too?
    pub async fn fork_branch(
        &self,
        name: String,
        source: &SedimentreeId,
    ) -> Result<SedimentreeId, DbError> {
        tracing::info!("Forking new branch {:?} from source {:?}", name, source);

        let latest_ref = self.get_latest_ref_on_branch(source).await?;

        // At the instant which we clone, the new shadow document does NOT exist, but the
        // canonical document does.
        // We wait for document_watcher to ingest the metadata handle and start tracking the new branch.
        let new_handle = self.clone_branch(source).await?;
        let username = self.username.lock().await.clone();
        let id = new_handle.document_id();

        self.add_branch_to_meta(Branch {
            name: name.clone(),
            id: id.clone(),
            forked_from: Some(latest_ref),
            merge_into: None,
            created_by: username,
            reverted_to: None,
        })
        .await?;
        Ok(id.clone())
    }
}
