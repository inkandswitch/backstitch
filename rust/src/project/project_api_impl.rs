use std::collections::{HashMap, HashSet};

use automerge::ChangeHash;
use samod::DocumentId;

use crate::{
    diff::differ::ProjectDiff,
    fs::file_utils::FileContent,
    helpers::{
        history_ref::HistoryRef,
        utils::{
            BranchWrapper, CommitInfo, DiffWrapper, exact_human_readable_timestamp,
            human_readable_timestamp,
        },
    },
    interop::godot_accessors::BackstitchConfigAccessor,
    project::{
        project::Project,
        project_api::{
            BranchViewModel, ChangeViewModel, DiffViewModel, ProjectViewModel, SyncStatus,
        },
    },
};

// TODO (Lilith): Figure out if there's a reasonable way to reduce blocking in this file.
// In general I kind of hate this, but I guess a sync/async divide is never going to look pretty.
impl ProjectViewModel for Project {
    fn has_project(&self) -> bool {
        self.driver.blocking_lock().is_some()
    }

    fn get_project_id(&self) -> Option<DocumentId> {
        self.with_driver_blocking("Get project ID", |driver| async move {
            driver.as_ref()?.get_metadata_doc().await
        })
    }

    fn new_project(&mut self) {
        if self.has_project() {
            return;
        }
        self.start();
    }

    fn load_project(&mut self, id: &DocumentId) {
        if self.has_project() {
            return;
        }
        BackstitchConfigAccessor::set_project_value("project_doc_id", id.to_string().as_str());
        self.start();
    }

    fn clear_project(&mut self) {
        if !self.has_project() {
            return;
        }
        self.stop();
        BackstitchConfigAccessor::set_user_value("user_name", "");
        BackstitchConfigAccessor::set_project_value("project_doc_id", "");
        BackstitchConfigAccessor::set_project_value("checked_out_branch_doc_id", "");
    }

    fn has_user_name(&self) -> bool {
        BackstitchConfigAccessor::get_user_value("user_name", "") != ""
    }

    fn get_user_name(&self) -> String {
        BackstitchConfigAccessor::get_user_value("user_name", "Anonymous")
    }

    fn set_user_name(&self, name: String) {
        BackstitchConfigAccessor::set_user_value("user_name", &name);
        self.with_driver_blocking("Set username", |driver| async move {
            driver.as_ref()?.set_username(Some(name)).await;
            Some(())
        });
    }

    fn can_create_merge_preview_branch(&self) -> bool {
        let Some(main_branch) = self.get_main_branch() else {
            return false;
        };
        match self.get_checked_out_branch_state() {
            Some(branch_state) => branch_state.id != main_branch.get_id(),
            _ => false,
        }
    }

    fn create_merge_preview_branch(&mut self) {
        let Some(checked_out_branch) = self.get_checked_out_branch_state() else {
            return;
        };
        let Some(fork_info) = checked_out_branch.forked_from else {
            return;
        };

        let source = checked_out_branch.id;
        let target = fork_info.branch().clone();
        self.with_driver_blocking("Create merge preview branch", |driver| async move {
            driver
                .as_ref()?
                .create_merge_preview_branch(&source, &target)
                .await;
            Some(())
        });
    }

    fn can_create_revert_preview_branch(&self, head: ChangeHash) -> bool {
        if self.is_revert_preview_branch_active() || self.is_merge_preview_branch_active() {
            return false;
        }
        if self.get_change(head).is_some_and(|c|
        	// Allow reverts for only the second setup commit
        	!c.is_setup() || self.get_branch_history().iter().position(|&h| h == head) == Some(1))
        {
            return self.get_checked_out_branch_state().is_some();
        }
        false
    }
    fn create_revert_preview_branch(&mut self, head: ChangeHash) {
        let Some(checked_out_branch) = self.get_checked_out_branch_state() else {
            return;
        };

        self.with_driver_blocking("Create revert preview branch", move |driver| async move {
            driver.as_ref()?.create_revert_preview_branch(&HistoryRef::new(checked_out_branch.id, vec![head])).await;
            Some(())
        });
    }

    fn is_revert_preview_branch_active(&self) -> bool {
        let branch_state = self.get_checked_out_branch_state();
        match branch_state {
            Some(state) => state.reverted_to.is_some(),
            _ => false,
        }
    }

    fn is_merge_preview_branch_active(&self) -> bool {
        let branch_state = self.get_checked_out_branch_state();
        match branch_state {
            Some(state) => state.merge_into.is_some(),
            _ => false,
        }
    }

    fn is_safe_to_merge(&self) -> bool {
        let Some(current_branch) = self.get_checked_out_branch_state() else {
            return false;
        };
        let Some(merge_info) = current_branch.merge_into.as_ref() else {
            return false;
        };
        let Some(fork_info) = current_branch.forked_from.as_ref() else {
            return false;
        };

        let forked_from = fork_info.branch().clone();
        let merge_into = merge_info.branch().clone();
        let Some((source_branch, latest_dest_heads)) =
            self.with_driver_blocking("Is safe to merge", |driver| async move {
                let source_branch = driver
                    .as_ref()?
                    .get_branch_db()
                    .get_branch_state(&forked_from)
                    .await;
                let latest_dest_heads = driver
                    .as_ref()?
                    .get_branch_db()
                    .get_latest_ref_on_branch(&merge_into)
                    .await?
                    .heads()
                    .clone();
                Some((source_branch, latest_dest_heads))
            })
        else {
            return false;
        };

        source_branch.is_some_and(|s| {
            s.forked_from
                .as_ref()
                .is_some_and(|i| i.heads() == &latest_dest_heads)
        })
    }

    fn confirm_preview_branch(&mut self) {
        let Some(branch_state) = self.get_checked_out_branch_state() else {
            return;
        };

        if let Some(_) = &branch_state.reverted_to {
            self.with_driver_blocking("Confirm merge preview branch", |driver| async move {
                driver.as_ref()?.confirm_revert_preview_branch().await;
                Some(())
            });
        } else if let Some(merge_info) = branch_state.merge_into {
            let source = branch_state.id.clone();
            let target = merge_info.branch().clone();
            self.with_driver_blocking("Confirm merge preview branch", |driver| async move {
                driver.as_ref()?.merge_branch(&source, &target).await;
                Some(())
            });
        }
    }
    fn discard_preview_branch(&mut self) {
        let Some(branch_state) = self.get_checked_out_branch_state() else {
            return;
        };
        if branch_state.reverted_to.is_none() && branch_state.merge_into.is_none() {
            tracing::error!("Cannot discard branch; not a preview branch!");
            return;
        };
        self.with_driver_blocking("Discard preview branch", |driver| async move {
            driver.as_ref()?.discard_current_branch().await;
            Some(())
        });
    }

    fn get_branch_history(&self) -> Vec<ChangeHash> {
        self.history.clone().unwrap_or(Vec::new())
    }

    fn get_sync_status(&self) -> SyncStatus {
        if !self.has_project() {
            // We have no reason to be connected, therefore just mark it as OK.
            return SyncStatus::UpToDate;
        }

        let Some((info, ref_)) =
            self.with_driver_blocking("Print sync debug", |driver| async move {
                let info = driver.as_ref()?.get_connection_info().await?;
                let ref_ = driver
                    .as_ref()?
                    .get_branch_db()
                    .get_checked_out_ref()
                    .await?;
                Some((info, ref_))
            })
        else {
            return SyncStatus::Unknown;
        };

        let Some(branch) = self.get_checked_out_branch_state() else {
            return SyncStatus::Unknown;
        };
        let Some(status) = info.docs.get(&branch.id) else {
            return SyncStatus::Unknown;
        };
        let is_connected = info.last_received.is_some();
        if status
            .last_acked_heads
            .as_ref()
            .is_some_and(|s| s == ref_.heads())
        {
            if is_connected {
                return SyncStatus::UpToDate;
            }
            return SyncStatus::Disconnected(0);
        }

        if is_connected {
            return SyncStatus::Syncing;
        }

        let unsynced_count = self.changes.iter().filter(|(_hash, c)| !c.synced).count();

        return SyncStatus::Disconnected(unsynced_count);
    }

    fn print_sync_debug(&self) {
        if !self.has_project() {
            return;
        }
        let info = self.with_driver_blocking("Print sync debug", |driver| async move {
            driver.as_ref()?.get_connection_info().await
        });
        let Some(info) = info else {
            tracing::debug!("Sync info UNAVAILABLE!!!");
            return;
        };
        let is_connected = info.last_received.is_some();

        tracing::debug!("Sync info ===========================");
        tracing::debug!("is connected: {is_connected}");
        tracing::debug!("last received: {:?}", info.last_received);
        tracing::debug!("last sent: {:?}", info.last_sent);

        if let Some(branch) = self.get_checked_out_branch_state() {
            if let Some(status) = info.docs.get(&branch.id) {
                tracing::debug!("\t{}:", branch.name);
                tracing::debug!("\tacked heads: {:?}", status.last_acked_heads);
                tracing::debug!("\tsent heads: {:?}", status.last_sent_heads);
                tracing::debug!("\tlast sent: {:?}", status.last_sent);
                tracing::debug!("\tlast sent: {:?}", status.last_received);
            }
        }
        tracing::debug!("=====================================");
    }

    fn get_branch(&self, id: &DocumentId) -> Option<impl BranchViewModel + use<>> {
        let id = id.clone();

        let Some((state, mut children)) =
            self.with_driver_blocking("Get branch", |driver| async move {
                tracing::trace!("Getting branch state...");
                let branch_db = driver.as_ref()?.get_branch_db();
                let Some(state) = branch_db.get_branch_state(&id).await else {
                    return None;
                };
                tracing::trace!("Getting branch children...");
                let children = branch_db.get_branch_children(&id).await;
                Some((state, children))
            })
        else {
            return None;
        };

        children.sort_by(|a, b| {
            let a_state = self.get_branch(a);
            let b_state = self.get_branch(b);
            let Some(a_state) = a_state else {
                return std::cmp::Ordering::Less;
            };
            let Some(b_state) = b_state else {
                return std::cmp::Ordering::Greater;
            };
            a_state
                .get_name()
                .to_lowercase()
                .cmp(&b_state.get_name().to_lowercase())
        });

        Some(BranchWrapper {
            state: state.clone(),
            children,
        })
    }

    fn get_main_branch(&self) -> Option<impl BranchViewModel> {
        let id = self.with_driver_blocking("Get main branch", |driver| async move {
            driver.as_ref()?.get_main_branch().await
        })?;
        self.get_branch(&id)
    }

    fn get_checked_out_branch(&self) -> Option<impl BranchViewModel> {
        let id = self.with_driver_blocking("Get checked out branch", |driver| async move {
            driver.as_ref()?.get_branch_db().get_checked_out_ref().await
        })?;
        self.get_branch(id.branch())
    }

    fn create_branch(&mut self, name: String) {
        let Some(branch_state) = self.get_checked_out_branch_state() else {
            return;
        };
        self.with_driver_blocking("Create branch", |driver| async move {
            driver.as_ref()?.fork_branch(name, &branch_state.id).await;
            Some(())
        });
    }

    fn checkout_branch(&mut self, branch: &DocumentId) {
        let branch = branch.clone();
        self.with_driver_blocking("Checkout branch", |driver| async move {
            driver.as_ref()?.request_checkout(&branch).await;
            Some(())
        });
    }

    fn get_change(&self, hash: ChangeHash) -> Option<&impl ChangeViewModel> {
        self.changes.get(&hash)
    }

    fn get_default_diff(&self) -> Option<impl DiffViewModel> {
        let heads_before;

        let (branch_state, heads_after) =
            self.with_driver_blocking("Get default diff", |driver| async move {
                let branch_db = driver.as_ref()?.get_branch_db();

                let branch = branch_db.get_checked_out_ref().await?.branch().clone();

                let state = branch_db.get_branch_state(&branch).await?;
                let synced_heads = branch_db
                    .get_latest_ref_on_branch(&branch)
                    .await?
                    .heads()
                    .clone();
                Some((state, synced_heads))
            })?;

        // There is no default diff for the main branch!
        if branch_state.id == self.get_main_branch().unwrap().get_id() {
            return None;
        }

        if self.is_merge_preview_branch_active() {
            heads_before = branch_state.merge_into.as_ref()?.heads();
        }
        // revert preview and regular branch both use forked_at
        else {
            heads_before = branch_state.forked_from.as_ref()?.heads();
        }

        // generate the summary
        let title;
        if self.is_merge_preview_branch_active() {
            let source_name = self
                .get_branch(branch_state.forked_from.as_ref()?.branch())?
                .get_name();
            let target_name = self
                .get_branch(branch_state.merge_into.as_ref()?.branch())?
                .get_name();
            title = format!("Showing changes for {} -> {}", source_name, target_name);
        } else if self.is_revert_preview_branch_active() {
            let source_name = self
                .get_branch(&branch_state.forked_from.as_ref()?.branch())?
                .get_name();
            // assume reverted_to is always just 1 hash
            let short_heads = &branch_state.reverted_to?.heads().first()?.to_string()[..7];
            title = format!(
                "Showing changes for {} reverted to {}",
                source_name, short_heads
            );
        } else {
            let source_name = self
                .get_branch(&branch_state.forked_from.as_ref()?.branch())?
                .get_name();
            title = format!(
                "Showing changes from {} -> {}",
                source_name, branch_state.name
            );
        }

        let before = HistoryRef::new(branch_state.id.clone(), heads_before.clone());
        let after = HistoryRef::new(branch_state.id.clone(), heads_after.clone());

        Some(DiffWrapper {
            diff: self.get_cached_diff(before, after),
            title,
        })
    }

    fn get_diff(&self, selected_hash: ChangeHash) -> Option<impl DiffViewModel> {
        let change = self.changes.get(&selected_hash)?;
        if change.is_setup() {
            return None;
        }
        let heads_before;
        let heads_after = vec![change.hash];

        let history = self.get_branch_history();
        let mut prev_hash = None;
        for (i, el) in history.iter().enumerate() {
            if *el == selected_hash {
                prev_hash = history.get(i - 1).copied();
                break;
            }
        }

        let branch_state = self.get_checked_out_branch_state()?;

        if let Some(prev_hash) = prev_hash {
            heads_before = vec![prev_hash];
        } else {
            heads_before = branch_state.forked_from.as_ref()?.heads().clone();
        }

        let before = HistoryRef::new(branch_state.id.clone(), heads_before);
        let after = HistoryRef::new(branch_state.id.clone(), heads_after);

        Some(DiffWrapper {
            diff: self.get_cached_diff(before, after),
            title: format!(
                "Showing changes from {} - {}",
                change.get_summary(),
                change.get_human_timestamp()
            ),
        })
    }

    fn get_current_ref(&self) -> Option<HistoryRef> {
        self.with_driver_blocking("Get current ref", |driver| async move {
            driver.as_ref()?.get_branch_db().get_checked_out_ref().await
        })
    }

    fn get_file_at_ref(&self, path: &String, ref_: &HistoryRef) -> Option<FileContent> {
        let path = path.clone();
        let ref_ = ref_.clone();
        self.with_driver_blocking("Get file at ref", |driver| async move {
            let files = driver
                .as_ref()?
                .get_branch_db()
                .get_files_at_ref(&ref_, &HashSet::from_iter(vec![path.clone()]))
                .await;
            files?.get(&path).cloned()
        })
    }

    fn get_files_at_ref(
        &self,
        ref_: &HistoryRef,
        filters: &HashSet<String>,
    ) -> Option<HashMap<String, FileContent>> {
        let ref_ = ref_.clone();
        let filters = filters.clone();
        self.with_driver_blocking("Get files at ref", |driver| async move {
            driver
                .as_ref()?
                .get_branch_db()
                .get_files_at_ref(&ref_, &filters)
                .await
        })
    }

    fn is_branch_loaded(&self, branch: &DocumentId) -> bool {
        let branch = branch.clone();
        self.with_driver_blocking("Is branch loaded", |driver| async move {
            let Some(dr) = driver.as_ref() else {
                return false;
            };
            dr.get_branch_db().is_branch_loaded(&branch).await
        })
    }

    fn dump_current_branch(&self) {
        let Some(ref_) = self.get_current_ref() else {
            return;
        };
        self.with_driver_blocking("Dump current branch", |driver| async move {
            let Some(dr) = driver.as_ref() else {
                return;
            };
            dr.get_branch_db().dump_branch_doc(ref_.branch()).await;
        });
    }
}

impl ChangeViewModel for CommitInfo {
    fn get_hash(&self) -> ChangeHash {
        self.hash
    }

    fn get_username(&self) -> String {
        if let Some(meta) = &self.metadata {
            if let Some(author) = &meta.username {
                return author.clone();
            }
        };
        "Anonymous".to_string()
    }

    fn is_synced(&self) -> bool {
        self.synced
    }

    fn get_summary(&self) -> String {
        self.summary.clone()
    }

    fn is_merge(&self) -> bool {
        let Some(meta) = &self.metadata else {
            return false;
        };
        return meta.merge_metadata.is_some();
    }

    fn is_setup(&self) -> bool {
        let Some(meta) = &self.metadata else {
            return false;
        };
        return meta.is_setup.unwrap_or(false);
    }

    fn get_exact_timestamp(&self) -> String {
        exact_human_readable_timestamp(self.timestamp)
    }

    fn get_human_timestamp(&self) -> String {
        human_readable_timestamp(self.timestamp)
    }

    fn get_merge_id(&self) -> Option<DocumentId> {
        Some(
            self.metadata
                .as_ref()?
                .merge_metadata
                .as_ref()?
                .merged_branch_id
                .clone(),
        )
    }
}

impl BranchViewModel for BranchWrapper {
    fn get_id(&self) -> DocumentId {
        self.state.id.clone()
    }

    fn get_name(&self) -> String {
        self.state.name.clone()
    }

    fn get_parent(&self) -> Option<DocumentId> {
        Some(self.state.forked_from.as_ref()?.branch().clone())
    }

    fn get_children(&self) -> Vec<DocumentId> {
        self.children.clone()
    }

    fn is_available(&self) -> bool {
        !self.get_merge_into().is_some() && !self.get_reverted_to().is_some()
    }

    fn get_reverted_to(&self) -> Option<ChangeHash> {
        Some(self.state.reverted_to.as_ref()?.heads().first()?.clone())
    }

    fn get_merge_into(&self) -> Option<DocumentId> {
        Some(self.state.merge_into.as_ref()?.branch().clone())
    }
}

impl DiffViewModel for DiffWrapper {
    fn get_diff(&self) -> &ProjectDiff {
        &self.diff
    }

    fn get_title(&self) -> &String {
        &self.title
    }
}
