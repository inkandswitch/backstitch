use std::{path::PathBuf, sync::Arc};

use samod::DocumentId;
use tokio::sync::{Mutex, oneshot, watch};
use url::Url;

use crate::{
    auth::server_manager::ServerManager,
    helpers::{spawn_utils::spawn_named_on, utils::ChangedFile},
    project::{
        LocalChangesResult, Project, ProjectStartError, ProjectStartStatus,
        config::Config,
        driver::{Driver, ProjectLoadError},
        main_thread_block::MainThreadBlock,
        project_base::ProjectCreateMode,
    },
};

struct LoadSuccess {
    found_locally: bool,
    found_on_provided_branch: bool,
}

// TODO: Consider moving this stuff to the driver? Not sure
impl Project {
    pub(super) fn start(&self, mode: ProjectCreateMode) {
        tracing::info!("Starting a project...");
        let start_lock = self.start_lock.clone();
        let project_dir = self.project_dir.clone();
        let start_status_tx = self.start_status_tx.clone();
        let driver = self.driver.clone();
        let main_thread_block = self.main_thread_block.clone();
        let local_changes_tx = self.local_changes_tx.clone();
        let server_manager = self.server_manager.clone();
        let config = self.config.clone();
        spawn_named_on("start project", self.runtime.handle(), async move {
            let _guard = start_lock.lock().await;
            if driver.read().await.is_some() {
                tracing::error!("Driver is already started!");
                return;
            }
            start_status_tx.send_replace(ProjectStartStatus::Starting);
            let result = Self::start_inner(
                server_manager,
                start_status_tx.clone(),
                local_changes_tx,
                main_thread_block,
                project_dir,
                mode,
                config,
            )
            .await;
            match result {
                Ok(d) => {
                    let mut driver_rw = driver.write().await;
                    *driver_rw = Some(d);
                    start_status_tx.send_replace(ProjectStartStatus::Done);
                }
                Err(e) => {
                    tracing::error!("error starting project: {e}");
                    start_status_tx.send_replace(ProjectStartStatus::Failed(e.to_string()));
                }
            }
        });
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
    async fn start_inner(
        server_manager: ServerManager,
        start_status_tx: watch::Sender<ProjectStartStatus>,
        local_changes_tx: Arc<Mutex<Option<oneshot::Sender<LocalChangesResult>>>>,
        main_thread_block: MainThreadBlock,
        project_dir: PathBuf,
        mode: ProjectCreateMode,
        config: Config,
    ) -> Result<Driver, ProjectStartError> {
        tracing::info!("Creating with mode: {:?}", mode);

        start_status_tx.send_replace(ProjectStartStatus::Starting);

        let server_url = config.server_url().await;

        // If the metadata ID is not a valid document ID, give up.
        // Not relevant for new projects.
        // This should rarely happen; we should be validating document IDs before they hit the config.
        // If the user edits the config by-hand and fucks it up, this may occur.
        let metadata_id = if mode == ProjectCreateMode::New {
            None
        } else {
            Some(
                config
                    .project_doc_id()
                    .await
                    .ok_or(ProjectStartError::DocumentIdInvalid)?,
            )
        };

        let saved_branch_id = if mode == ProjectCreateMode::New {
            None
        } else {
            // If this becomes None due to an invalid value, that's OK; we auto check out main.
            config.checked_out_branch_doc_id().await
        };
        tracing::info!(
            "Starting GodotProject with metadata doc id: {:?}",
            metadata_id
        );

        tracing::debug!("Attempting to create driver...");

        let storage_dir = project_dir.join(".backstitch");
        let mut driver = Driver::new(
            main_thread_block.clone(),
            server_manager,
            project_dir,
            config.user_name().await.unwrap_or("".to_string()),
            storage_dir,
        )
        .await?;

        // We've created the driver. Before connecting, we need to load the doc and handle local changes.
        // If we're making a new project, we don't have to worry about that.
        let (local_changes, load_success) = if mode == ProjectCreateMode::New {
            tracing::debug!("Created driver. Setting up new project...");
            driver.create_project().await?;
            (
                Vec::new(),
                LoadSuccess {
                    found_locally: true,
                    found_on_provided_branch: false,
                },
            )
        } else {
            tracing::debug!("Created driver. Loading project...");
            let success = Self::try_and_retry_load(
                &mut driver,
                server_url.as_ref(),
                &metadata_id.unwrap(), // we know this is valid, from earlier
                saved_branch_id.clone().as_ref(),
            )
            .await?;

            let local_changes = driver.get_local_changes(saved_branch_id.as_ref()).await?;
            (local_changes, success)
        };

        tracing::debug!("Successfully set up project. Checking for local changes...");

        let initial_branch = if load_success.found_on_provided_branch {
            saved_branch_id.clone()
        } else {
            None
        };
        let local_changes: Vec<ChangedFile> = local_changes
            .iter()
            .cloned()
            .map(|(path, change_type)| ChangedFile { change_type, path })
            .collect();

        if !local_changes.is_empty() {
            // we can't start the sync until we confirm or reject the local changes
            // the one exception: if we found the project locally AND it was automatically loaded, we can automatically checkin the changes.
            if mode == ProjectCreateMode::AutoLoaded && load_success.found_locally {
                tracing::debug!("Local changes detected; automatically committing.");
                driver.commit_local_changes(initial_branch.as_ref()).await?
            } else {
                tracing::debug!("Local changes detected; you must confirm or discard them!");
                start_status_tx
                    .send_replace(ProjectStartStatus::NeedsCheckIn(local_changes.clone()));
                let rx = {
                    let mut local_changes_tx = local_changes_tx.lock().await;
                    let (tx, rx) = oneshot::channel();
                    *local_changes_tx = Some(tx);
                    rx
                };
                match rx.await? {
                    LocalChangesResult::CheckIn => {
                        driver.commit_local_changes(initial_branch.as_ref()).await?
                    }
                    LocalChangesResult::Discard => {}
                }
            }
        }

        // start the connection, if we didn't before.
        if let Some(server_url) = server_url {
            tracing::debug!("Starting connection, if it isn't started already....");
            match driver.start_connection(&server_url).await {
                Ok(_) => {}
                Err(e) => tracing::error!("Remote connection error: {:?}", e),
            }
            tracing::debug!("Connection started.");
        }
        let metadata = driver.get_metadata_doc().await?;

        tracing::debug!("Starting sync...");
        driver.start_sync(initial_branch.as_ref()).await;

        config.set_project_doc_id(Some(&metadata)).await;
        Ok(driver)
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
                    // If anything wasn't found locally, that's OK, we want to connect first
                    ProjectLoadError::MetadataIdNotFound { server_status: _ } => match server_url {
                        Some(_) => e,
                        // If no server URL was provided, there's no coming back from this.
                        None => return Err(ProjectStartError::DocumentIdNotFound),
                    },
                    ProjectLoadError::BranchDocNotFound { server_status: _ } => e,
                    ProjectLoadError::BinaryDocNotFound { server_status: _ } => e,
                    // If something else happened, bad!!!
                    _ => return Err(e.into()),
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
                        _ => return Err(e.into()),
                    }
                }
            };
        };

        // try and start the connection
        driver.start_connection(server_url).await?;

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
                    _ => return Err(e.into()),
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
                    _ => Err(e.into()),
                }
            }
        }
    }
}
