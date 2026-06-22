use crate::diff::differ::ProjectDiff;
use crate::fs::file_utils::FileSystemEvent;
use crate::helpers::branch::Branch;
use crate::helpers::history_ref::HistoryRef;
use crate::helpers::spawn_utils::spawn_named_on;
use crate::helpers::utils::{ChangedFile, CommitInfo};
use crate::interop::godot_accessors::{
    BackstitchConfigAccessor, BackstitchEditorAccessor, EditorFilesystemAccessor,
};
use crate::project::driver::{Driver, ProjectLoadError};
use crate::project::main_thread_block::MainThreadBlock;
use crate::project::project_api::{ProjectStartError, ProjectViewModel};
use automerge::ChangeHash;
use samod::{ConnectionInfo, DocumentId, Url};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::{collections::HashMap, str::FromStr};
use tokio::runtime::Runtime;
use tokio::sync::{Mutex, OwnedMutexGuard, watch};

#[derive(Debug, PartialEq, Clone)]
pub(super) enum ProjectCreateMode {
    New,
    ManuallyLoaded,
    AutoLoaded,
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
    ServerStatusChanged,
    ChangesIngested,
}

struct LoadSuccess {
    found_locally: bool,
    found_on_provided_branch: bool,
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
            connection_info_rx: None,
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
        !(EditorFilesystemAccessor::is_scanning()
            || BackstitchEditorAccessor::is_editor_importing()
            || BackstitchEditorAccessor::unsaved_files_open())
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
        branch_id: Option<&DocumentId>,
    ) -> Result<LoadSuccess, ProjectStartError> {
        // I am going to become the joker because of this method, but I think it's all necessary/as simple as possible. Maybe I'm wrong...
        // Either way, here's the logic:
        // - Try locally. Good? Return!
        // - If no server URL was provided:
        //      - ... and the metadata doc wasn't found, give up
        //      - ... and branch or binary docs weren't found, try again on the main branch
        //          - If branch wasn't found, give up
        //          - If binaries weren't found, we'll live!
        // - Otherwise, if ANY docs aren't found, connect to the server and try again.
        //      - If no metadata doc, give up.
        //      - If no branch doc, try main branch. Still no? Give up.
        //      - If no binary docs, we'll live!

        // Simple, easy path -- try and load it locally.
        match driver.load_project(metadata_id, branch_id).await {
            // success? Then just return!
            Ok(_) => {
                tracing::info!("Successfully found project locally!");
                return Ok(LoadSuccess {
                    found_locally: true,
                    found_on_provided_branch: true,
                });
            }
            Err(e) => {
                match e {
                    ProjectLoadError::Unknown => return Err(ProjectStartError::Unknown), // this shouldn't happen
                    // If anything wasn't found locally, that's OK, we want to connect first
                    ProjectLoadError::MetadataIdNotFound { server_status: _ } => match server_url {
                        Some(_) => e,
                        // If no server URL was provided, there's no coming back from this.
                        None => return Err(ProjectStartError::DocumentIdNotFound),
                    },
                    ProjectLoadError::BranchDocNotFound { server_status: _ } => e,
                    ProjectLoadError::BinaryDocNotFound { server_status: _ } => e,
                }
            }
        };

        // Diverge here. If no server URL was provided, our only fallback is to try and check out the main branch.
        let Some(server_url) = server_url else {
            // Try the exact same thing, but this time without a branch ID
            match driver.load_project(metadata_id, None).await {
                // success? Then just return!
                Ok(_) => {
                    tracing::info!("Successfully found project locally, on the main branch!");
                    return Ok(LoadSuccess {
                        found_locally: true,
                        found_on_provided_branch: false,
                    });
                }
                Err(e) => {
                    match e {
                        ProjectLoadError::Unknown => return Err(ProjectStartError::Unknown), // this shouldn't happen
                        // What the heck? We should've already checked this case...
                        ProjectLoadError::MetadataIdNotFound { server_status: _ } => {
                            tracing::error!(
                                "This shouldn't happen!! The metadata doc disappeared!!!!!"
                            );
                            return Err(ProjectStartError::DocumentIdNotFound);
                        }
                        // The project is broken!!!! We can't find the main
                        ProjectLoadError::BranchDocNotFound { server_status: _ } => {
                            tracing::error!(
                                "Main branch not found. Your document is most likely corrupted. Please zip up your project and send it to the Backstitch team!"
                            );
                            return Err(ProjectStartError::MainBranchNotFound);
                        }
                        // This is more reasonable, and recoverable.
                        ProjectLoadError::BinaryDocNotFound { server_status: _ } => {
                            tracing::error!(
                                "Not all binary docs synced properly... but we can recover."
                            );
                            // TODO: Actually recover
                            return Ok(LoadSuccess {
                                found_locally: true,
                                found_on_provided_branch: false,
                            });
                        }
                    }
                }
            };
        };

        // try and start the connection
        driver
            .start_connection(server_url)
            .await
            .map_err(|_| ProjectStartError::Unknown)?;

        // try again to load
        match driver.load_project(metadata_id, branch_id).await {
            // success? Then just return!
            Ok(_) => {
                tracing::info!("Successfully found project remotely!");
                return Ok(LoadSuccess {
                    found_locally: false,
                    found_on_provided_branch: true,
                });
            }
            Err(e) => {
                match e {
                    ProjectLoadError::Unknown => return Err(ProjectStartError::Unknown), // this shouldn't happen
                    ProjectLoadError::MetadataIdNotFound { server_status: _ } => {
                        return Err(ProjectStartError::DocumentIdNotFound);
                    }
                    ProjectLoadError::BranchDocNotFound { server_status: _ } => {}
                    ProjectLoadError::BinaryDocNotFound { server_status: _ } => {
                        tracing::error!(
                            "Not all binary docs synced properly, even after a remote connection... but we can recover."
                        );
                        // TODO: Actually recover
                        return Ok(LoadSuccess {
                            found_locally: false,
                            found_on_provided_branch: true,
                        });
                    }
                }
            }
        };

        // our branch doc is definitely invalid. It doesn't exist locally or on the server.
        // Try and load the main branch instead.
        match driver.load_project(metadata_id, None).await {
            // success? Then just return!
            Ok(_) => {
                tracing::info!("Successfully found project remotely, using the main branch!");
                Ok(LoadSuccess {
                    found_locally: false,
                    found_on_provided_branch: false,
                })
            }
            Err(e) => {
                match e {
                    ProjectLoadError::Unknown => Err(ProjectStartError::Unknown), // this shouldn't happen
                    ProjectLoadError::MetadataIdNotFound { server_status: _ } => {
                        tracing::error!(
                            "What?!?!? The metadata doc went bad when trying to load the main branch!!"
                        );
                        Err(ProjectStartError::DocumentIdNotFound)
                    }
                    ProjectLoadError::BranchDocNotFound { server_status: _ } => {
                        tracing::error!(
                            "Main branch not found, even on the server. Your document is most likely corrupted. Please zip up your project and send it to the Backstitch team!"
                        );
                        Err(ProjectStartError::MainBranchNotFound)
                    }
                    ProjectLoadError::BinaryDocNotFound { server_status: _ } => {
                        tracing::error!(
                            "What?!?! Binary docs didn't sync on a second try, but they did on the first try!!! Well, we can recover..."
                        );
                        // TODO: Actually recover
                        Ok(LoadSuccess {
                            found_locally: false,
                            found_on_provided_branch: false,
                        })
                    }
                }
            }
        }
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
    pub(super) fn start(&mut self, mode: ProjectCreateMode) -> Result<(), ProjectStartError> {
        tracing::info!("Creating with mode: {:?}", mode);
        if self.driver.blocking_lock().is_some() {
            tracing::error!("Driver is already started!");
            return Ok(());
        }

        let storage_dir = self.project_dir.join(".backstitch");
        let server_url = self.acquire_server_url()?;

        // If the metadata ID is not a valid document ID, give up.
        // Not relevant for new projects.
        let metadata_id = if mode == ProjectCreateMode::New {
            None
        } else {
            let id = BackstitchConfigAccessor::get_project_value("project_doc_id", "");
            Some(DocumentId::from_str(&id).map_err(|_| {
                tracing::error!("Invalid metadata document ID! Not starting driver.");
                ProjectStartError::DocumentIdInvalid(id)
            })?)
        };

        let saved_branch_id = if mode == ProjectCreateMode::New {
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
        tracing::debug!("Attempting to create driver...");
        let init_branch = initial_branch.clone();
        let (driver, local_changes, load_success) = self
            .runtime
            .block_on(
                // I think it's correct to spawn this on a different task explicitly, because block_on runs the future on the current thread, not a worker thread.
                spawn_named_on("Create driver", self.runtime.handle(), async move {
                    tracing::debug!("Creating driver...");
                    let Some(mut driver) =
                        Driver::new(block, project_dir, username, storage_dir).await
                    else {
                        tracing::error!("Could not create driver!");
                        return Err(ProjectStartError::Unknown);
                    };

                    // We've created the driver. Before connecting, we need to load the doc and handle local changes.
                    // If we're making a new project, we don't have to worry about that.
                    if mode_clone == ProjectCreateMode::New {
                        driver
                            .create_project()
                            .await
                            .map_err(|_| ProjectStartError::Unknown)?;
                        Ok((
                            driver,
                            Default::default(),
                            LoadSuccess {
                                found_locally: true,
                                found_on_provided_branch: false,
                            },
                        ))
                    } else {
                        let success = Self::try_and_retry_load(
                            &mut driver,
                            server_url.as_ref(),
                            &metadata_id.unwrap(), // we know this is valid, from earlier
                            init_branch.as_ref(),
                        )
                        .await?;

                        let local_changes = driver
                            .get_local_changes(saved_branch_id.as_ref())
                            .await
                            .map_err(|_| {
                                tracing::error!("Couldn't get local changes!");
                                ProjectStartError::Unknown
                            })?;
                        Ok((driver, local_changes, success))
                    }
                }),
            )
            .unwrap()?;

        self.changes_rx = Some(driver.get_changes_rx());
        self.connection_info_rx = Some(driver.get_connection_info_rx());
        self.checked_out_ref_rx = Some(driver.get_ref_rx());
        self.server_url = server_url_clone;
        self.initial_branch = if load_success.found_on_provided_branch {
            initial_branch
        } else {
            None
        };
        self.local_changes = local_changes
            .iter()
            .cloned()
            .map(|(path, change_type)| ChangedFile { change_type, path })
            .collect();

        *self.driver.blocking_lock() = Some(driver);

        if !local_changes.is_empty() {
            // we can't start the sync until we confirm or reject the local changes
            // the one exception: if we found the project locally AND it was automatically loaded, we can automatically checkin the changes.
            if mode == ProjectCreateMode::AutoLoaded && load_success.found_locally {
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
        self.connection_info_rx = None;
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
                res
            }))
            .unwrap()
    }

    #[tracing::instrument(skip_all, level = "trace")]
    pub fn process(
        &mut self,
        _delta: f64,
        safe_to_update_godot: bool,
    ) -> (Vec<FileSystemEvent>, Vec<GodotProjectSignal>) {
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
                .set_safe_to_update_editor(safe_to_update_godot);
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

        let rx = self.connection_info_rx.as_mut().unwrap();
        if rx.has_changed().unwrap_or(false) {
            rx.mark_unchanged();
            signals.push(GodotProjectSignal::ServerStatusChanged);
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
            rx.mark_unchanged();
        }

        tracing::trace!("Done with process.");
        (fs_changes, signals)
    }
}
