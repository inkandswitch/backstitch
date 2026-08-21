use crate::auth::server_manager::{AuthStatus, ServerManager};
use crate::diff::differ::ProjectDiff;
use crate::fs::file_utils::FileSystemEvent;
use crate::helpers::branch::Branch;
use crate::helpers::spawn_utils::spawn_named_on;
use crate::helpers::utils::{CommitInfo, DiffId};
use crate::project::config::Config;
use crate::project::driver::Driver;
use crate::project::main_thread_block::MainThreadBlock;
use crate::project::project_api::RequestDiffError;
use crate::project::{Project, ProjectStartStatus};
use samod::DocumentId;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{OwnedRwLockReadGuard, RwLock, watch};

#[derive(Debug, PartialEq, Clone)]
pub(super) enum ProjectCreateMode {
    New,
    ManuallyLoaded,
    AutoLoaded,
}

#[derive(Debug, Clone)]
pub enum DiffStatus {
    Loading,
    Ready(ProjectDiff),
}

/// Notifications that can be emitted via process and consumed by GodotProject, in order to trigger signals to GDScript.
// TODO: We need to rework this signaling method to remove the reliance of [Project] on Godot concepts.
// Instead, [GodotProject] should handle converting streams into signals each frame via polling.
// [Project] should simply provide the streams, forwarding them as needed from the driver on async tasks.
pub enum GodotProjectSignal {
    SyncStatusChanged,
    ChangesIngested,
    BranchCheckedOut,
    StartStatusChanged(ProjectStartStatus),
    AuthStatusChanged(AuthStatus),
}

impl Project {
    // kinda awkward, considering the existence of the driver get_project_id... don't confuse them!
    // this one is for before we've started the driver. (consider API change here)
    pub fn get_project_doc_id(&self) -> Option<DocumentId> {
        let config = self.config.clone();
        self.runtime
            .block_on(async move { config.project_doc_id().await })
    }
    pub fn new(project_dir: PathBuf, user_data_dir: PathBuf) -> Self {
        // TODO (Lilith): ensure we make this work across the ENTIRE program, not just the driver.
        // For now this encapsulates everything we multi-thread, since Project is the barrier for public async access.
        // So it's fine. But if we want other code besides the driver to be multi-threaded...
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("backstitch-driver-worker")
            .build()
            .unwrap();

        let (start_tx, start_rx) = watch::channel(ProjectStartStatus::NotStarted);

        let server_manager = ServerManager::new();

        let config = Config::new(&project_dir, &user_data_dir);

        Self {
            main_thread_block: MainThreadBlock::new(),
            changes_rx: None,
            checked_out_ref_rx: None,
            driver: Arc::new(RwLock::new(None)),
            project_dir,
            runtime,
            history: None,
            changes: HashMap::new(),
            diff_cache: Default::default(),
            connection_info_rx: None,
            start_status_tx: start_tx,
            start_status_rx: start_rx,
            start_lock: Default::default(),
            local_changes_tx: Default::default(),
            auth_status_rx: server_manager.subscribe_status(),
            server_manager,
            config,
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

    /// Get the diff status of a particular [DiffId]
    pub(super) fn request_diff(&self, diff_id: DiffId) -> Result<DiffStatus, RequestDiffError> {
        // If we've already spun off a task to handle this, return whatever's the current result.
        let mut cache = self.diff_cache.blocking_lock();
        if let Some(status) = cache.get(&diff_id) {
            return status.clone();
        }

        // Set the status to "Loading"
        cache.insert(diff_id.clone(), Ok(DiffStatus::Loading));
        drop(cache);

        // Spawn off a task to handle the load
        let driver = self.driver.clone();
        let diff_cache = self.diff_cache.clone();
        spawn_named_on("Request diff", self.runtime.handle(), async move {
            let status = Self::get_diff(driver, &diff_id).await;
            let mut cache = diff_cache.lock().await;
            cache.insert(diff_id, status.map(DiffStatus::Ready));
        });

        // While that's working, we just tell the caller it's loading
        Ok(DiffStatus::Loading)
    }

    async fn get_diff(
        driver: Arc<RwLock<Option<Driver>>>,
        id: &DiffId,
    ) -> Result<ProjectDiff, RequestDiffError> {
        let guard = driver.read().await;
        let driver = guard.as_ref().ok_or(RequestDiffError::NoDriver)?;
        let diff = driver.get_diff(&id.before, &id.after).await;
        Ok(diff)
    }

    pub fn clear_diff_cache(&self) {
        let mut cache = self.diff_cache.blocking_lock();
        // Keep those that are still loading, so we don't double-spawn a task.
        // If we reaaalllly want we could stick a token inside DiffStatus::Loading to cancel those tasks explicitly.
        cache.retain(|_, v| matches!(v, Ok(DiffStatus::Loading)));
    }

    pub fn clear_fs_cache(&self) {
        self.with_driver_blocking("Clear FS Cache", |driver| async move {
            driver
                .as_ref()
                .unwrap()
                .get_fs_index()
                .clear_cache()
                .inspect_err(|e| tracing::error!("error clearing cache: {e}"))
                .ok();
        });
    }

    pub fn stop(&mut self) {
        // TODO: There's maybe a potential bug here if someone stops the driver immediately after starting it...
        // The start continues in the background, and we clean up this stuff immediately before it gets reinitialized.
        // I don't think that's possible if we're smart about the frontend, but ideally this contract violation wouldn't
        // be possible!
        self.driver.blocking_write().take();
        self.start_status_tx
            .send_replace(ProjectStartStatus::NotStarted);
        self.changes_rx = None;
        self.checked_out_ref_rx = None;
        self.connection_info_rx = None;
        self.history = None;
    }

    // common utility function within this class
    pub(super) fn get_checked_out_branch_state(&self) -> Option<Branch> {
        self.with_driver_blocking("Get checked out branch state", |driver| async move {
            let branch_db = driver.as_ref()?.get_branch_db();
            let checked_out_ref = branch_db.get_checked_out_ref().await?;
            branch_db
                .get_branch_state(checked_out_ref.branch())
                .await
                .inspect_err(|e| tracing::error!("Error getting checked out branch state: {e}"))
                .ok()
        })
    }

    /// Jank utility function to lock on the driver and run on a different thread.
    /// Allows us to easily block on async code when we need the driver.
    pub(super) fn with_driver_blocking<F, Fut, R>(&self, name: &str, f: F) -> R
    where
        F: FnOnce(OwnedRwLockReadGuard<Option<Driver>>) -> Fut + Send + 'static,
        Fut: Future<Output = R> + Send + 'static,
        R: Send + 'static,
    {
        let driver = self.driver.clone();
        let name_clone = name.to_string();
        self.runtime
            .block_on(spawn_named_on(name, self.runtime.handle(), async move {
                tracing::trace!("Starting block on {name_clone}...");
                let driver = driver.read_owned().await;
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
        // These signals aren't dependent on the driver's behavior; the driver might not be present
        let mut signals = Vec::new();
        if self.start_status_rx.has_changed().unwrap_or(false) {
            tracing::info!(
                "START STATUS CHANGED {:?}",
                self.start_status_rx.borrow_and_update().clone()
            );
            signals.push(GodotProjectSignal::StartStatusChanged(
                self.start_status_rx.borrow_and_update().clone(),
            ))
        }

        if self.auth_status_rx.has_changed().unwrap_or(false) {
            self.auth_status_rx.mark_unchanged();
            signals.push(GodotProjectSignal::AuthStatusChanged(
                self.auth_status_rx.borrow().clone(),
            ));
        }

        tracing::trace!("Running project process...");
        let fs_changes = {
            let driver_guard = self.driver.blocking_read();
            if driver_guard.is_none() {
                return (Vec::new(), signals);
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
            driver_guard.as_ref().unwrap().get_filesystem_changes()
        };

        // This is super awkward; lazy initialize these.
        // Normally this should be done on the driver creation thread, but since it's async, initializing these
        // gets really weird (mutexes needed etc). So just initialize on the main thread for now.
        // TODO: When we need to refactor/add more of these, just expose these as streams to GodotProject, let that handle...
        // (Actually, don't expose them as streams -- just expose them as async waiter functions. GodotProject can handle spinning off
        // threads to turn them into signals, I think.)
        {
            // we know driver exists
            let driver_guard = self.driver.blocking_read();
            let driver = driver_guard.as_ref().unwrap();
            if self.changes_rx.is_none() {
                self.changes_rx = Some(driver.get_changes_rx());
            }
            if self.connection_info_rx.is_none() {
                self.connection_info_rx = Some(driver.get_connection_info_rx());
            }
            if self.checked_out_ref_rx.is_none() {
                self.checked_out_ref_rx = Some(driver.get_ref_rx());
            }
        }

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
            signals.push(GodotProjectSignal::SyncStatusChanged);
        }

        // Check to see if we need to produce a CheckedOutBranch signal
        let rx = self.checked_out_ref_rx.as_mut().unwrap();
        if rx.has_changed().unwrap_or(false) {
            let doc_id = rx.borrow().as_ref().map(|r| r.branch()).cloned();
            // TODO: do we need to block on this? Maybe just spawn this off
            let config = self.config.clone();
            self.runtime.block_on(async move {
                config.set_checked_out_branch_doc_id(doc_id.as_ref()).await;
            });
            rx.mark_unchanged();
            signals.push(GodotProjectSignal::BranchCheckedOut);
        }

        tracing::trace!("Done with process.");
        (fs_changes, signals)
    }
}
