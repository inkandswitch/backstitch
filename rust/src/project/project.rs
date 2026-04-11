use crate::diff::differ::ProjectDiff;
use crate::fs::file_utils::FileSystemEvent;
use crate::helpers::branch::Branch;
use crate::helpers::history_ref::HistoryRef;
use crate::helpers::spawn_utils::spawn_named_on;
use crate::helpers::utils::CommitInfo;
use crate::interop::godot_accessors::{
    BackstitchConfigAccessor, BackstitchEditorAccessor, EditorFilesystemAccessor,
};
use crate::project::driver::Driver;
use crate::project::main_thread_block::MainThreadBlock;
use automerge::ChangeHash;
use samod::{DocumentId, Url};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::{collections::HashMap, str::FromStr};
use tokio::runtime::Runtime;
use tokio::sync::{Mutex, OwnedMutexGuard, watch};
use tracing::instrument;

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

    pub fn start(&mut self) -> Result<(), String> {
        if self.driver.blocking_lock().is_some() {
            tracing::error!("Driver is already started!");
            return Ok(());
        }

        let storage_dir = self.project_dir.join(".backstitch");
        let server_url = {
            let project = BackstitchConfigAccessor::get_project_value("server_url", "");
            if !project.is_empty() {
                tracing::info!("Using project override for server url: {:?}", project);
                project
            } else {
                "".to_string()
            }
        };

        // if the URL doesn't have a scheme, add tcp://
        let server_url = {
            if server_url == "" {
                None
            } else {
                let server_url = if server_url.contains("://") {
                    server_url
                } else {
                    format!("tcp://{}", server_url)
                };

                let Ok(url) = Url::parse(&server_url) else {
                    tracing::error!("Could not start project! Invalid server URL {server_url}");
                    return Err("Invalid server URL!".to_string());
                };
                Some(url)
            }
        };

        // If the metadata ID is not a valid document ID, give up.
        // If it's an empty string, returns None so we can make a new doc.
        let metadata_id = match Some(BackstitchConfigAccessor::get_project_value(
            "project_doc_id",
            "",
        ))
        .filter(|s| !s.is_empty())
        {
            Some(s) => match DocumentId::from_str(&s) {
                Ok(id) => Some(id),
                Err(_) => {
                    tracing::error!("Invalid metadata document ID! Not starting driver.");
                    return Err("Invalid metadata document ID!".to_string());
                }
            },
            None => None,
        };

        let saved_branch_id = match Some(BackstitchConfigAccessor::get_project_value(
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
        };

        tracing::info!(
            "Starting GodotProject with metadata doc id: {:?}",
            metadata_id
        );

        let project_dir = self.project_dir.clone();
        let username = BackstitchConfigAccessor::get_user_value("user_name", "");
        let block = self.main_thread_block.clone();

        // TODO: Don't block on main thread for checkin
        let (driver, metadata) = self
            .runtime
            .block_on(
                // I think it's correct to spawn this on a different task explicitly, because block_on runs the future on the current thread, not a worker thread.
                spawn_named_on("Create driver", self.runtime.handle(), async move {
                    let driver = Driver::new(
                        block,
                        server_url,
                        project_dir,
                        username,
                        storage_dir,
                        metadata_id,
                        saved_branch_id,
                    )
                    .await;
                    let metadata = if let Some(driver) = &driver {
                        driver.get_metadata_doc().await
                    } else {
                        None
                    };
                    (driver, metadata)
                }),
            )
            .unwrap();

        let Some(driver) = driver else {
            tracing::error!("Could not start the driver!");
            return Err("Could not start driver!".to_string());
        };

        BackstitchConfigAccessor::set_project_value(
            "project_doc_id",
            &metadata
                .expect("Driver started, but metadata null!")
                .to_string(),
        );
        self.changes_rx = Some(driver.get_changes_rx());
        self.checked_out_ref_rx = Some(driver.get_ref_rx());

        *self.driver.blocking_lock() = Some(driver);
        Ok(())
    }

    pub fn stop(&mut self) {
        self.driver.blocking_lock().take();
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

    #[instrument(skip_all)]
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
