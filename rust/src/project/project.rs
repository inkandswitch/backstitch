use crate::diff::differ::ProjectDiff;
use crate::fs::file_utils::FileSystemEvent;
use crate::helpers::branch::Branch;
use crate::helpers::history_ref::HistoryRef;
use crate::helpers::spawn_utils::spawn_named_on;
use crate::helpers::utils::{ChangeType, ChangedFile, CommitInfo};
use crate::interop::godot_accessors::{
    BackstitchConfigAccessor, BackstitchEditorAccessor, EditorFilesystemAccessor,
};
use crate::project::driver::{Driver, ProjectLoadError};
use crate::project::main_thread_block::MainThreadBlock;
use crate::project::project_api::{ProjectStartError, ProjectViewModel};
use automerge::ChangeHash;
use samod::{DocumentId, Url};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::{collections::HashMap, str::FromStr};
use tokio::runtime::Runtime;
use tokio::sync::{Mutex, OwnedMutexGuard, watch};

#[derive(Debug, PartialEq, Clone)]
pub(super) enum CreateMode {
    NewProject,
    ManuallyLoadedProject,
    AutoLoadedProject,
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

    // Project driver. If some, is running.
    // I'd prefer this not be a mutex, but we need to move it into temporary threads in order to dispatch async code from sync code.
    // What's annoying is that we never actually block on this mutex!
    pub(super) driver: Arc<Mutex<Option<Driver>>>,
    pub(super) local_changes: Vec<ChangedFile>,
    pub(super) server_url: Option<Url>,
    pub(super) initial_branch: Option<DocumentId>, // the initial branch to checkout, only valid before finalize_start is called
    project_dir: PathBuf,
    pub(super) runtime: Runtime,

    // Tracked changes for the UI
    pub(super) history: Option<Vec<ChangeHash>>,
    pub(super) changes: HashMap<ChangeHash, CommitInfo>,

    // Cached diffs between refs
    pub(super) diff_cache: RefCell<HashMap<(HistoryRef, HistoryRef), ProjectDiff>>,
}

/// Notifications that can be emitted via process and consumed by GodotProject, in order to trigger signals to GDScript.
pub enum GodotProjectSignal {
    CheckedOutBranch,
    ChangesIngested,
}

impl Project {
    pub fn new(project_dir: PathBuf) -> Self {
        // TODO (Lilith): ensure we make this work across the ENTIRE program, not just the driver.
        // For now this encapsulates everything we multi-thread, since Project is the barrier for public async access.
        // So it's fine. But if we want other code besides the driver to be multi-threaded...
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("backstitch-driver-worker")
            .build()
            .unwrap();

        Self {
            main_thread_block: MainThreadBlock::new(),
            changes_rx: None,
            checked_out_ref_rx: None,
            driver: Arc::new(Mutex::new(None)),
            project_dir,
            runtime,
            history: None,
            changes: HashMap::new(),
            diff_cache: RefCell::new(HashMap::new()),
            local_changes: Default::default(),
            server_url: None,
            initial_branch: None,
        }
    }

    fn ingest_changes(&mut self, changes: Vec<CommitInfo>) {
        tracing::info!("Ingesting changes...");

        let history = self.history.get_or_insert(Vec::new());

        history.clear();
        self.changes.clear();

        // Consume changes into self.changes
        for change in changes {
            history.push(change.hash);
            self.changes.insert(change.hash, change);
        }
    }

    pub fn get_cached_diff(&self, before: HistoryRef, after: HistoryRef) -> ProjectDiff {
        self.diff_cache
            .borrow_mut()
            .entry((before.clone(), after.clone()))
            .or_insert_with(|| self.get_diff(before, after))
            .clone()
    }

    pub fn clear_diff_cache(&self) {
        self.diff_cache.borrow_mut().clear();
    }

    pub fn clear_fs_cache(&self) {
        self.with_driver_blocking("Clear FS Cache", |driver| async move {
            let _ = driver.as_ref().unwrap().get_fs_index().clear_cache();
        })
    }

    // Do not run this on anything except the main thread!
    pub fn safe_to_update_godot() -> bool {
        return !(EditorFilesystemAccessor::is_scanning()
            || BackstitchEditorAccessor::is_editor_importing()
            || BackstitchEditorAccessor::is_changing_scene()
            || BackstitchEditorAccessor::unsaved_files_open());
    }

    pub fn get_diff(&self, before: HistoryRef, after: HistoryRef) -> ProjectDiff {
        self.with_driver_blocking("Get diff", |driver| async move {
            driver.as_ref().unwrap().get_diff(&before, &after).await
        })
    }

    fn acquire_server_url(&self) -> Result<Option<Url>, ProjectStartError> {
        let server_url = BackstitchConfigAccessor::get_project_value("server_url", "");

        if server_url.is_empty() {
            return Ok(None);
        }

        tracing::info!("Using project override for server url: {:?}", server_url);
        let url = if server_url.contains("://") {
            server_url
        } else {
            format!("tcp://{}", server_url)
        };

        let url = Url::parse(&url)
            .ok()
            .filter(|url| url.scheme() == "tcp" || url.scheme() == "ws" || url.scheme() == "wss")
            .ok_or(ProjectStartError::ServerUrlInvalid(url))?;

        Ok(Some(url))
    }

    /// Returns whether we found the document locally.
    async fn try_and_retry_load(
        driver: &mut Driver,
        server_url: Option<&Url>,
        metadata_id: &DocumentId,
    ) -> Result<bool, ProjectStartError> {
        match driver.load_project(metadata_id).await {
            // success? Then just return!
            Ok(_) => {
                tracing::info!("Successfully found project locally!");
                return Ok(true);
            }
            Err(e) => {
                match e {
                    // If it wasn't found locally, that's OK, we try to connect first
                    ProjectLoadError::DocumentIdNotFoundLocally => (),
                    _ => return Err(ProjectStartError::Unknown), // this shouldn't happen
                }
            }
        }

        let server_url = server_url.ok_or(ProjectStartError::DocumentIdNotFoundLocally)?;

        // try and start the connection
        driver
            .start_connection(server_url)
            .await
            .map_err(|_| ProjectStartError::Unknown)?;

        // try again to load
        driver.load_project(metadata_id).await.map_err(|e| {
            match e {
                ProjectLoadError::Unknown => ProjectStartError::Unknown,
                ProjectLoadError::DocumentIdNotFoundLocally => {
                    ProjectStartError::DocumentIdNotFoundLocally
                } // this shouldn't happen at this stage
                ProjectLoadError::DocumentIdNotFoundLocallyOrRemotely => {
                    ProjectStartError::DocumentIdNotFoundLocallyOrRemotely
                }
                ProjectLoadError::DocumentIdNotFoundLocallyAndServerDidNotConnect => {
                    ProjectStartError::DocumentIdNotFoundLocallyAndServerDidNotConnect
                }
            }
        })?;
        tracing::info!("Successfully found project remotely!");
        Ok(false)
    }

    /// Starting a project is a multi-step process. It looks like:
    /// - Parse the document ID
    /// - Attempt to load a repo by ID from the local filesystem.
    /// - If failed, attempt to load a repo by ID from the server.
    /// - If failed, give up.
    /// - If we successfully connected locally OR remotely, scan the filesystem for changes.
    /// - If there were changes, check to see if we can make an automatic decision to check-in the changes.
    ///     + We auto-check-in if we're connected locally AND we're restarting from a previous session (project
    ///       ID stored in the config file).
    /// - If we can't make an automatic decision, so we pause, and the `confirm` or `discard` API continues the load.
    /// - Once we've made a decision to handle the local changes, we can finish the checkin: connect if we're not
    ///   connected, and start the sync loop.
    ///
    /// If we're creating a new project, instead of loading, we can skip most of this!
    pub(super) fn start(&mut self, mode: CreateMode) -> Result<(), ProjectStartError> {
        tracing::info!("Creating with mode: {:?}", mode);
        if self.driver.blocking_lock().is_some() {
            tracing::error!("Driver is already started!");
            return Ok(());
        }

        let storage_dir = self.project_dir.join(".backstitch");
        let server_url = self.acquire_server_url()?;

        // If the metadata ID is not a valid document ID, give up.
        // Not relevant for new projects.
        let metadata_id = if mode == CreateMode::NewProject {
            None
        } else {
            let id = BackstitchConfigAccessor::get_project_value("project_doc_id", "");
            Some(DocumentId::from_str(&id).map_err(|_| {
                tracing::error!("Invalid metadata document ID! Not starting driver.");
                return ProjectStartError::DocumentIdInvalid(id);
            })?)
        };

        let saved_branch_id = if mode == CreateMode::NewProject {
            None
        } else {
            match Some(BackstitchConfigAccessor::get_project_value(
                "checked_out_branch_doc_id",
                "",
            ))
            .filter(|s| !s.is_empty())
            {
                Some(s) => match DocumentId::from_str(&s) {
                    Ok(id) => Some(id),
                    Err(_) => {
                        tracing::error!("Invalid saved branch ID! Not using.");
                        None
                    }
                },
                None => None,
            }
        };
        let initial_branch = saved_branch_id.clone();

        tracing::info!(
            "Starting GodotProject with metadata doc id: {:?}",
            metadata_id
        );

        let project_dir = self.project_dir.clone();
        let username = BackstitchConfigAccessor::get_user_value("user_name", "");
        let block = self.main_thread_block.clone();

        // TODO: Don't block on main thread for checkin
        let mode_clone = mode.clone();
        let server_url_clone = server_url.clone();
        let (driver, local_changes, found_locally) = self
            .runtime
            .block_on(
                // I think it's correct to spawn this on a different task explicitly, because block_on runs the future on the current thread, not a worker thread.
                spawn_named_on("Create driver", self.runtime.handle(), async move {
                    let Some(mut driver) =
                        Driver::new(block, project_dir, username, storage_dir).await
                    else {
                        tracing::error!("Could not create driver!");
                        return Err(ProjectStartError::Unknown);
                    };

                    // We've created the driver. Before connecting, we need to load the doc and handle local changes.
                    // If we're making a new project, we don't have to worry about that.
                    if mode_clone == CreateMode::NewProject {
                        driver
                            .create_project()
                            .await
                            .map_err(|_| ProjectStartError::Unknown)?;
                        return Ok((driver, Default::default(), true));
                    } else {
                        let found_locally = Self::try_and_retry_load(
                            &mut driver,
                            server_url.as_ref(),
                            &metadata_id.unwrap(), // we know this is valid, from earlier
                        )
                        .await?;

                        let local_changes = driver
                            .get_local_changes(saved_branch_id.as_ref())
                            .await
                            .map_err(|_| {
                                tracing::error!("Couldn't get local changes!");
                                ProjectStartError::Unknown
                            })?;
                        return Ok((driver, local_changes, found_locally));
                    }
                }),
            )
            .unwrap()?;

        self.changes_rx = Some(driver.get_changes_rx());
        self.checked_out_ref_rx = Some(driver.get_ref_rx());
        self.server_url = server_url_clone;
        self.initial_branch = initial_branch;
        self.local_changes = local_changes
            .iter()
            .cloned()
            .map(|(path, change_type)| ChangedFile { change_type, path })
            .collect();

        *self.driver.blocking_lock() = Some(driver);

        if local_changes.len() > 0 {
            // we can't start the sync until we confirm or reject the local changes
            // the one exception: if we found the project locally AND it was automatically loaded, we can automatically checkin the changes.
            if mode == CreateMode::AutoLoadedProject && found_locally {
                self.checkin_local_changes();
            }
            return Ok(());
        }

        self.finalize_start()?;
        Ok(())
    }

    pub(super) fn finalize_start(&mut self) -> Result<(), ProjectStartError> {
        tracing::info!("Finalizing start...");
        let server_url = self.server_url.clone();
        let initial_branch = self.initial_branch.take();
        let metadata = self.with_driver_blocking("Finalize start", |mut driver| async move {
            let driver = driver.as_mut().ok_or(ProjectStartError::Unknown)?;
            // start the connection, if we didn't before.
            if let Some(server_url) = server_url {
                match driver.start_connection(&server_url).await {
                    Ok(_) => {}
                    Err(e) => tracing::error!("Remote connection error: {:?}", e),
                }
            }
            let metadata = driver
                .get_metadata_doc()
                .await
                .ok_or(ProjectStartError::Unknown)?;

            driver.start_sync(initial_branch.as_ref()).await;
            Ok(metadata)
        })?;
        self.local_changes = Default::default();
        BackstitchConfigAccessor::set_project_value("project_doc_id", &metadata.to_string());
        Ok(())
    }

    pub fn stop(&mut self) {
        self.driver.blocking_lock().take();
        self.server_url = None;
        self.local_changes = Default::default();
        self.changes_rx = None;
        self.checked_out_ref_rx = None;
        self.history = None;
    }

    // common utility function within this class
    pub(super) fn get_checked_out_branch_state(&self) -> Option<Branch> {
        self.with_driver_blocking("Get checked out branch state", |driver| async move {
            let checked_out_ref = driver
                .as_ref()?
                .get_branch_db()
                .get_checked_out_ref()
                .await?;
            driver
                .as_ref()?
                .get_branch_db()
                .get_branch_state(checked_out_ref.branch())
                .await
        })
    }

    /// Jank utility function to lock on the driver and run on a different thread.
    /// Allows us to easily block on async code when we need the driver.
    pub(super) fn with_driver_blocking<F, Fut, R>(&self, name: &str, f: F) -> R
    where
        F: FnOnce(OwnedMutexGuard<Option<Driver>>) -> Fut + Send + 'static,
        Fut: Future<Output = R> + Send + 'static,
        R: Send + 'static,
    {
        let driver = self.driver.clone();
        let name_clone = name.to_string();
        self.runtime
            .block_on(spawn_named_on(name, self.runtime.handle(), async move {
                tracing::trace!("Starting block on {name_clone}...");
                let driver = driver.lock_owned().await;
                let res = f(driver).await;
                tracing::trace!("Finishing block on {name_clone}!");
                return res;
            }))
            .unwrap()
    }

    #[tracing::instrument(skip_all, level = "trace")]
    pub fn process(&mut self, _delta: f64) -> (Vec<FileSystemEvent>, Vec<GodotProjectSignal>) {
        tracing::trace!("Running project process...");
        let fs_changes = {
            let mut driver_guard = self.driver.blocking_lock();
            if driver_guard.is_none() {
                return (Vec::new(), Vec::new());
            }
            // Run the blocking sync
            driver_guard
                .as_ref()
                .unwrap()
                .set_safe_to_update_editor(Self::safe_to_update_godot());
            let block = self.main_thread_block.clone();
            tracing::trace!("Blocking for dependents...");
            self.runtime
                .block_on(spawn_named_on(
                    "Blocking guard",
                    self.runtime.handle(),
                    async move {
                        block.checkpoint().await;
                    },
                ))
                .unwrap();
            tracing::trace!("Done blocking.");

            // Consume any modified files to send to Godot
            driver_guard.as_mut().unwrap().get_filesystem_changes()
        };

        let mut signals = Vec::new();

        // Ingest changes if the driver produced a new changeset, or if we've never ingested.
        let changes = {
            let rx = self.changes_rx.as_mut().unwrap();
            if self.history.is_none() || rx.has_changed().unwrap_or(false) {
                rx.mark_unchanged();
                signals.push(GodotProjectSignal::ChangesIngested);
                Some(rx.borrow().clone())
            } else {
                None
            }
        };

        if let Some(changes) = changes {
            self.ingest_changes(changes);
        }

        // Check to see if we need to produce a CheckedOutBranch signal
        let rx = self.checked_out_ref_rx.as_mut().unwrap();
        if rx.has_changed().unwrap_or(false) {
            let doc_id = rx
                .borrow()
                .as_ref()
                .map(|r| r.branch().to_string())
                .unwrap_or("".to_string());
            BackstitchConfigAccessor::set_project_value("checked_out_branch_doc_id", &doc_id);
            signals.push(GodotProjectSignal::CheckedOutBranch);
            rx.mark_unchanged();
        }

        tracing::trace!("Done with process.");
        (fs_changes, signals)
    }
}
