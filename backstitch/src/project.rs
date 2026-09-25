use std::{collections::HashMap, path::PathBuf, sync::Arc};

use automerge::ChangeHash;
use samod::ConnectionInfo;
use thiserror::Error;
use tokio::{
    runtime::Runtime,
    sync::{Mutex, RwLock, broadcast, oneshot, watch},
    task::JoinError,
};
use url::Url;

use crate::{
    auth::server_manager::{AuthStatus, ServerManager, ServerStatus},
    helpers::{
        history_ref::HistoryRef,
        utils::{ChangedFile, CommitInfo, DiffId},
    },
    project::{
        config::Config,
        connection::RemoteConnectionError,
        driver::{Driver, DriverCreateError, ProjectLoadError},
        main_thread_block::MainThreadBlock,
        project_api::RequestDiffError,
        project_base::DiffStatus,
    },
};

mod config;
mod connection;
mod document_watcher;
mod fs;
pub mod project_api;
pub mod project_api_impl;
pub mod project_starter;
// pub for use in differ; consider restructuring
pub mod branch_db;
mod change_ingester;
mod driver;
mod main_thread_block;
mod peer_watcher;
pub mod project_base;

#[derive(Debug, Clone)]
pub enum ProjectStartStatus {
    NotStarted,
    Starting,
    NeedsCheckIn(Vec<ChangedFile>),
    Done,
    Failed(String),
}

#[derive(Debug)]
enum LocalChangesResult {
    CheckIn,
    Discard,
}

#[derive(Error, Debug)]
pub enum ProjectStartError {
    #[error(transparent)]
    DriverLoad(Box<ProjectLoadError>),
    #[error(transparent)]
    DriverCreate(#[from] DriverCreateError),
    #[error(transparent)]
    Connection(#[from] RemoteConnectionError),
    #[error(transparent)]
    Join(#[from] JoinError),
    #[error(transparent)]
    Oneshot(#[from] oneshot::error::RecvError),
    #[error(
        "we couldn't find a document of the given ID on your computer or on the provided server"
    )]
    DocumentIdNotFound,
    #[error(
        "we couldn't find the referenced main branch on your computer or on the provided server"
    )]
    MainBranchNotFound,
    #[error("the document ID is invalid!")]
    DocumentIdInvalid,
}

impl From<ProjectLoadError> for ProjectStartError {
    fn from(value: ProjectLoadError) -> Self {
        Self::DriverLoad(Box::new(value))
    }
}

/// Manages the state and operations of a Backstitch project within Godot.
/// Its API is exposed to GDScript via the GodotProject struct.
#[derive(Debug)]
pub struct Project {
    // Sync
    main_thread_block: MainThreadBlock,
    // These are here so we don't needlessly block during process
    changes_rx: Option<watch::Receiver<Vec<CommitInfo>>>,
    checked_out_ref_rx: Option<watch::Receiver<Option<HistoryRef>>>,
    connection_info_rx: Option<watch::Receiver<Option<ConnectionInfo>>>,
    auth_status_rx: watch::Receiver<AuthStatus>,
    server_status_rx: broadcast::Receiver<(Url, ServerStatus)>,
    start_status_rx: watch::Receiver<ProjectStartStatus>,

    start_status_tx: watch::Sender<ProjectStartStatus>,
    start_lock: Arc<Mutex<()>>,
    local_changes_tx: Arc<Mutex<Option<oneshot::Sender<LocalChangesResult>>>>,

    // Project driver. If some, is running.
    // Most operations read on the driver -- only replacing the driver is a write operation.
    driver: Arc<RwLock<Option<Driver>>>,
    server_manager: ServerManager,
    project_dir: PathBuf,
    runtime: Runtime,

    // Tracked changes for the UI
    history: Option<Vec<ChangeHash>>,
    changes: HashMap<ChangeHash, CommitInfo>,

    // Cached diffs between refs
    diff_cache: Arc<Mutex<HashMap<DiffId, Result<DiffStatus, RequestDiffError>>>>,

    // Deliberately using std::sync::RwLock so that task threads don't block the main thread by holding the lock.
    // TODO (Lilith): I'm not convinced about this. Validate this; make sure we NEVER hold it across await points.
    // IMO this should be using interior mutability instead...
    config: Config,
}
