use std::{collections::HashMap, path::PathBuf, sync::Arc};

use samod::{DocHandle, DocumentId, Repo};
use tokio::sync::{Mutex, RwLock, broadcast};

use crate::{
    helpers::{branch::BranchesMetadataDoc, history_ref::HistoryRef},
    project::branch_db::branch_sync::BranchSyncState,
};

mod branch;
mod branch_sync;
mod commit;
mod file;
mod merge_revert;
mod util;
use ignore::gitignore::Gitignore;

pub enum CanonicalBranchStatus {
    Pending,
    BranchNotIngested,
    BinaryDocNotFound,
    Healthy,
}
pub enum ShadowDocWaitError {
    BranchNotIngested,
    Unknown,
}

/// [BranchDb] is the primary data source for project data.
/// It stores the project state, and provides a handful of convenient state-manipulation methods for controllers to use.
#[derive(Clone, Debug)]
pub struct BranchDb {
    // Path is immutable, so it can be outside the inner
    project_dir: PathBuf,
    gitignore: Arc<Gitignore>,
    repo: Repo,

    username: Arc<Mutex<Option<String>>>,

    binary_states: Arc<Mutex<HashMap<DocumentId, Option<DocHandle>>>>,
    branch_sync_states: Arc<Mutex<HashMap<DocumentId, Arc<Mutex<BranchSyncState>>>>>,
    metadata_state: Arc<Mutex<Option<(DocHandle, BranchesMetadataDoc)>>>,

    // The checked out ref is the ref that the filesystem is currently synced with.
    // Has a separate lock because of its importance; it needs to be locked while we're prepping a commit or checking out stuff
    checked_out_ref: Arc<RwLock<Option<HistoryRef>>>,

    // Notified whenever we make or ingest changes to a branch
    branch_change_tx: broadcast::Sender<()>,
}

impl BranchDb {
    pub fn new(repo: Repo, project_dir: PathBuf, gitignore: Gitignore) -> Self {
        let (tx, _) = broadcast::channel(1);
        Self {
            project_dir,
            repo,
            gitignore: Arc::new(gitignore),
            username: Default::default(),
            binary_states: Default::default(),
            metadata_state: Default::default(),
            checked_out_ref: Default::default(),
            branch_sync_states: Default::default(),
            branch_change_tx: tx,
        }
    }

    pub fn get_project_dir(&self) -> PathBuf {
        self.project_dir.clone()
    }

    pub async fn set_username(&self, username: Option<String>) {
        let mut user = self.username.lock().await;
        *user = username;
    }
}
