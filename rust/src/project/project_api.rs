use std::collections::{HashMap, HashSet};

use automerge::ChangeHash;
use samod::DocumentId;
use thiserror::Error;

use crate::{
    auth::server_manager::ServerStatus,
    fs::file_utils::FileContent,
    helpers::{history_ref::HistoryRef, utils::ChangedFile},
    project::project_base::DiffStatus,
};

pub use crate::project::branch_db::DbError;

/// Represents synchronization status for a project.
pub enum SyncStatus {
    /// The server is disconnected, but we have no idea if we have extra changes.
    Unknown,
    /// The server is disconnected, and we know how many changes we haven't pushed.
    Disconnected(usize),
    /// The server is currently syncing our changes.
    Syncing,
    /// The server is up to date with our changes, or the project is not started.
    UpToDate,
}

#[derive(Error, Debug)]
pub enum CreateRevertPreviewBranchError {
    #[error("no checked out branch")]
    NoCheckedOutBranch,
    #[error("no driver found!")]
    NoDriver,
    #[error("no changes to revert!")]
    NoChangesToRevert,
    #[error(transparent)]
    DbError(#[from] Box<DbError>),
}

#[derive(Error, Debug)]
pub enum CreateMergePreviewBranchError {
    #[error("no checked out branch")]
    NoCheckedOutBranch,
    #[error("no forked from branch found!")]
    NoForkedFrom,
    #[error("no driver found!")]
    NoDriver,
    #[error("no changes to merge!")]
    NoChangesToMerge,
    #[error(transparent)]
    DbError(#[from] Box<DbError>),
}

#[derive(Error, Debug, Clone)]
pub enum RequestDiffError {
    #[error("No diff available for selection")]
    NoDiffAvailable,
    #[error("the selected hash is invalid!")]
    CommitNotFound,
    #[error("no branch is checked out!")]
    NoBranchCheckedOut,
    #[error("no previous change found!")]
    BranchesDiverge,
    #[error("no driver found!")]
    NoDriver,
}

/// Defines the surface for the UI layer interacting with the GodotProject core logic.
pub trait ProjectViewModel {
    /// Whether the user has set a username.
    fn has_user_name(&self) -> bool;
    /// Get the user's username.
    fn get_user_name(&self) -> String;
    /// Set a new username.
    fn set_user_name(&self, name: String);

    /// Checks to see if a server URL is valid. Returns the URL if the server is valid, with any necessary corrections.
    fn validate_server(&self, server: &str) -> Option<String>;
    /// Get a server's status, handshaking and registering the server if needed.
    fn ping_server(&self, server: &str, retry: bool) -> ServerStatus;
    /// Begins an interactive authentication on the server. Does nothing if already authenticated.
    fn authenticate_server(&self, server: &str);
    /// If there's currently an interactive authentication, cancels it.
    fn cancel_authenticate(&self);
    /// Get the last-connected server URL.
    fn get_saved_server(&self) -> Option<String>;
    /// Change the current server URL. May trigger an authentication.
    /// Only valid if the project is started.
    fn change_server(&self, server: Option<&str>);
    /// Add a server to the list of available servers
    fn add_server(&self, server: &str);
    /// Remove a server from the list of available servers
    fn remove_server(&self, server: &str);
    /// Get the list of available servers
    fn get_available_servers(&self) -> Vec<String>;
    /// Get the base URL of the Backstitch Webviewer for the current server.
    fn webviewer_url(&self) -> Option<String>;

    /// Remove the existing project and de-init.
    fn clear_project(&mut self);
    /// Whether we have initialized with a project yet.
    fn has_project(&self) -> bool;
    /// Get the current project [DocumentId], if it exists. Otherwise, return [None]
    fn get_project_id(&self) -> Option<DocumentId>;
    /// Starts the creation of a new project, in the background.
    fn new_project(&self, server_url: Option<&str>);
    /// Starts the load of a project, in the background, given a [DocumentId].
    /// If `autostart` is true, this is treated as automatically restarting a loaded project.
    /// Otherwise, it behaves as if the user is loading into a project.
    fn load_project(&self, id: &DocumentId, server_url: Option<&str>, autostart: bool);

    /// Get the current unresolved local changes from the project.
    /// We'll need to ask the user if they want to check these in.
    fn local_changes(&self) -> Vec<ChangedFile>;
    /// Check in the unresolved local changes to the project.
    fn check_in_local_changes(&self);
    /// Discard the local changes, reverting the project to its canonical state.
    fn discard_local_changes(&self);

    /// Gets the current project [SyncStatus].
    fn get_sync_status(&self) -> SyncStatus;
    /// Prints a sync debug message to the console.
    fn print_sync_debug(&self);

    /// Gets the [BranchViewModel] for the provided branch [DocumentId],
    /// or [None] if the document ID isn't a branch in the project.
    fn get_branch(&self, id: &DocumentId) -> Option<impl BranchViewModel + use<Self>>;
    /// Gets the [BranchViewModel] for the main root branch, or [None] if we have no project.
    fn get_main_branch(&self) -> Option<impl BranchViewModel>;
    /// Gets the [BranchViewModel] for the current checked out branch, or [None] if we have no project.
    fn get_checked_out_branch(&self) -> Option<impl BranchViewModel>;
    /// Create a new branch, forked off the current branch with the given name.
    fn create_branch(&self, branch_name: String);
    /// Check out a branch by ID.
    fn checkout_branch(&self, branch: &DocumentId);
    /// Returns true if the branch is loaded (i.e. has all of its binary docs synced).
    fn is_branch_loaded(&self, branch: &DocumentId) -> bool;
    /// Dumps a binary representation of the current branch to ./.backstitch/.
    fn dump_current_branch(&self);

    /// Whether we can begin a merge preview for the current branch into its direct ancestor.
    fn can_create_merge_preview_branch(&self) -> bool;
    /// Create a new merge preview branch, for merging the current branch into its direct ancestor.
    fn create_merge_preview_branch(&self) -> Result<(), CreateMergePreviewBranchError>;
    /// Whether we can create a revert preview branch for the given head.
    fn can_create_revert_preview_branch(&self, head: ChangeHash) -> bool;
    /// Create a new revert preview branch for the given head.
    fn create_revert_preview_branch(
        &self,
        head: ChangeHash,
    ) -> Result<(), CreateRevertPreviewBranchError>;
    /// Whether there is currently a revert preview active.
    fn is_revert_preview_branch_active(&self) -> bool;
    /// Whether there is currently a merge preview active.
    fn is_merge_preview_branch_active(&self) -> bool;
    /// Whether there has been changes in the root branch since we forked.
    fn is_safe_to_merge(&self) -> bool;
    /// Confirm the active preview branch, reverting or merging as necessary.
    fn confirm_preview_branch(&self);
    /// Discard the active preview branch.
    fn discard_preview_branch(&self);

    /// Get the full history for the currently checked-out branch, in chronological order.
    fn get_branch_history(&self) -> Vec<ChangeHash>;
    /// Get a [ChangeViewModel] for a given commit hash, or [None] if we haven't ingested the desired commit.
    fn get_change(&self, hash: ChangeHash) -> Option<&impl ChangeViewModel>;

    /// Get a [DiffViewModel] for a given commit hash, containing a [DiffStatus] of the diff's loading status.
    fn try_get_diff(
        &self,
        selected_hash: ChangeHash,
    ) -> Result<impl DiffViewModel, RequestDiffError>;
    /// Get a [DiffViewModel] for the default display, containing a [DiffStatus] of the diff's loading status.
    fn try_get_default_diff(&self) -> Result<impl DiffViewModel, RequestDiffError>;

    fn get_current_ref(&self) -> Option<HistoryRef>;
    /// Get the file at a given history reference.
    fn get_file_at_ref(&self, path: &str, ref_: &HistoryRef) -> Option<FileContent>;
    /// Get the files at a given history reference, with optional filters.
    fn get_files_at_ref(
        &self,
        ref_: &HistoryRef,
        filters: &HashSet<String>,
    ) -> Option<HashMap<String, FileContent>>;
}

/// API surface for a Change exposed to the UI.
pub trait ChangeViewModel {
    /// Get the hash of the change.
    fn get_hash(&self) -> ChangeHash;
    /// Get the username for the change, or "Anonymous" if there was no username logged.
    fn get_username(&self) -> String;
    /// Whether the change has been synced to the server.
    fn is_synced(&self) -> bool;
    /// The text summary for the change.
    fn get_summary(&self) -> String;
    /// Whether the change was from a branch being merged.
    fn is_merge(&self) -> bool;
    /// If the change is a merge change, returns the [DocumentId] for the branch that was merged in.
    /// Otherwise [None]
    fn get_merge_id(&self) -> Option<DocumentId>;
    /// Whether the change was an initial setup change for the main branch.
    fn is_setup(&self) -> bool;
    /// Get an exact timestamp for the change.
    fn get_exact_timestamp(&self) -> String;
    /// Get a user readable timestamp (e.g. "3 weeks ago") for the change.
    fn get_human_timestamp(&self) -> String;
}

/// API surface for a Branch exposed to the UI.
pub trait BranchViewModel {
    /// Get the unique [DocumentId] for the branch.
    fn get_id(&self) -> DocumentId;
    /// Get the name of the branch.
    fn get_name(&self) -> String;
    /// Get the parent branch, i.e. the branch this was originally forked from. If the branch is
    /// main, returns [None].
    fn get_parent(&self) -> Option<DocumentId>;
    /// Get the children of the branch, i.e. any branches that were forked from this branch.
    fn get_children(&self) -> Vec<DocumentId>;
    /// Whether the branch is user-exposed for checkout (i.e. isn't a merge or revert preview)
    fn is_available(&self) -> bool;
    /// If the branch is a revert preview, get the change reversion target. Otherwise, [None]
    fn get_reverted_to(&self) -> Option<ChangeHash>;
    /// If the branch is a merge preview, get the target branch. Otherwise, [None]
    fn get_merge_into(&self) -> Option<DocumentId>;
}

/// API surface for a Diff exposed to the UI.
pub trait DiffViewModel {
    /// Get the [ProjectDiff] containing the diff data.
    fn get_diff(&self) -> &DiffStatus;
    /// Get the display title of the diff.
    fn get_title(&self) -> &String;
}
