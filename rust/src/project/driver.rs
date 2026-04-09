use crate::diff::differ::{Differ, ProjectDiff};
use crate::fs::file_utils::FileSystemEvent;
use crate::helpers::history_ref::HistoryRef;
use crate::helpers::spawn_utils::spawn_named;
use crate::helpers::utils::CommitInfo;
use crate::project::branch_db::BranchDb;
use crate::project::change_ingester::ChangeIngester;
use crate::project::connection::RemoteConnection;
use crate::project::document_watcher::DocumentWatcher;
use crate::project::main_thread_block::MainThreadBlock;
use crate::project::peer_watcher::PeerWatcher;
use crate::project::sync_automerge_to_fs::SyncAutomergeToFileSystem;
use crate::project::sync_fs_to_automerge::SyncFileSystemToAutomerge;
use futures::StreamExt;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use samod::{ConcurrencyConfig, ConnectionInfo, DocHandle, DocumentId, Repo, Url};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::select;
use tokio::sync::{Mutex, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::instrument;

#[cfg(test)]
mod tests;

/// The main driver for the project.
/// Hooks together all the various controllers.
/// When this object is constructed, it is started. When the handle is dropped, it shuts down.
#[derive(Debug)]
pub struct Driver {
    inner: Arc<DriverInner>,
    repo: Repo,
    token: CancellationToken,
    // receivers go outside Inner, so we don't have to mutex them
    file_changes_rx: mpsc::UnboundedReceiver<FileSystemEvent>,
}

#[derive(Debug)]
pub struct DriverInner {
    // external synchronization
    main_thread_block: MainThreadBlock,
    file_changes_tx: mpsc::UnboundedSender<FileSystemEvent>,
    ref_tx: watch::Sender<Option<HistoryRef>>,
    safe_to_update_editor: AtomicBool,
    token: CancellationToken,

    // internal synchronization
    requested_checkout: Arc<Mutex<Option<DocumentId>>>,

    // subtasks
    #[allow(unused)]
    connection: RemoteConnection,
    branch_db: BranchDb,
    peer_watcher: Arc<PeerWatcher>,
    change_ingester: ChangeIngester,
    #[allow(unused)]
    document_watcher: DocumentWatcher,
    sync_automerge_to_fs: SyncAutomergeToFileSystem,
    sync_fs_to_automerge: SyncFileSystemToAutomerge,
    differ: Differ,
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.token.cancel();
        // just use the default executor for this one I think?
        futures::executor::block_on(self.repo.stop());
    }
}

impl Driver {
    const DEFAULT_IGNORE_GLOBS: [&str; 7] = [
        "**/.DS_Store",
        "**/thumbs.db",
        "**/desktop.ini",
        "**/backstitch.cfg",
        "**/addons/backstitch*",
        "**/target/*",
        "**/.*",
    ];

    fn build_gitignore(project_dir: &PathBuf) -> Gitignore {
        let mut gitignore = GitignoreBuilder::new(project_dir.clone());
        let _err = gitignore.case_insensitive(true);
        let _err = gitignore.add(project_dir.join(".gitignore"));
        let _err = gitignore.add(project_dir.join(".backstitchignore"));
        for glob in Self::DEFAULT_IGNORE_GLOBS {
            let _ = gitignore.add_line(None, glob);
        }
        gitignore.build().unwrap()
    }

    /// Creates a new instance of [Driver].
    /// Causes tasks to run in the background. To cancel everything, drop the handle.
    /// If we couldn't start the driver, [None] is returned.
    pub async fn new(
        main_thread_block: MainThreadBlock,
        server_url: Option<Url>,
        project_path: PathBuf,
        username: String,
        storage_directory: PathBuf,
        metadata_id: Option<DocumentId>,
        saved_branch_id: Option<DocumentId>,
    ) -> Option<Self> {
        let storage = samod::storage::TokioFilesystemStorage::new(storage_directory);
        let repo = Repo::build_tokio()
            .with_concurrency(ConcurrencyConfig::Threadpool(
                rayon::ThreadPoolBuilder::new().build().unwrap(),
            ))
            .with_storage(storage)
            .load()
            .await;

        // Start the connection
        // Terrible hack: If we're not provided a server URL, default to a garbage localhost URL.
        let Some(connection) = RemoteConnection::new(
            repo.clone(),
            server_url.unwrap_or(Url::parse("tcp://localhost:9999").unwrap()),
        )
        .await
        else {
            tracing::error!("Could not start connection!");
            return None;
        };
        let git_ignore: Gitignore = Self::build_gitignore(&project_path);
        let branch_db = BranchDb::new(repo.clone(), project_path, git_ignore);
        branch_db
            .set_username(if username.trim() == "" {
                None
            } else {
                Some(username.trim().to_string())
            })
            .await;
        let peer_watcher = Arc::new(PeerWatcher::new(repo.clone()));
        let sync_automerge_to_fs = SyncAutomergeToFileSystem::new(branch_db.clone());
        let sync_fs_to_automerge = SyncFileSystemToAutomerge::new(branch_db.clone());

        let metadata_handle = match &metadata_id {
            // If we're expecting an existing ID, try and fetch it.
            Some(id) => {
                let Some(handle) = Self::get_metadata_handle(&repo, id, &connection).await else {
                    return None;
                };
                handle
            }
            // If we need to make a new ID, make a doc and check in the initial state of the filesystem.
            None => branch_db.create_metadata_doc().await,
        };

        // The document watcher will auto-ingest the provided metadata handle.
        let document_watcher =
            DocumentWatcher::new(repo.clone(), branch_db.clone(), metadata_handle).await;

        // If this is a new project (i.e. we earlier made a metadata doc), check in the files.
        // This has to go after the document watcher ingests the metadata doc, of course.
        if metadata_id.is_none() {
            sync_fs_to_automerge.checkin().await;
        }

        let change_ingester = ChangeIngester::new(peer_watcher.clone(), branch_db.clone());
        change_ingester.request_ingestion();
        let differ = Differ::new(branch_db.clone());

        // At this point, if we loaded an existing project, we may not have checked it out yet.
        // We'll discover that while processing updates, and check it out then.

        let (file_changes_tx, file_changes_rx) = mpsc::unbounded_channel();
        let (ref_tx, _) = watch::channel(None);
        let token = CancellationToken::new();

        let this = Some(Driver {
            file_changes_rx,
            inner: Arc::new(DriverInner {
                main_thread_block,
                file_changes_tx,
                ref_tx,
                safe_to_update_editor: AtomicBool::new(false),
                token: token.clone(),
                requested_checkout: Arc::new(Mutex::new(saved_branch_id)),
                connection,
                branch_db,
                peer_watcher,
                change_ingester,
                document_watcher,
                sync_automerge_to_fs,
                sync_fs_to_automerge,
                differ,
            }),
            repo,
            token,
        });

        // Spawn off the sync task
        let inner_clone = this.as_ref().unwrap().inner.clone();
        spawn_named("Sync", async move {
            inner_clone.sync_main().await;
            tracing::info!("Sync shutting down");
        });
        this
    }

    pub async fn set_username(&self, username: Option<String>) {
        self.inner.branch_db.set_username(username).await;
    }

    /// If we're connected to the server, returns true.
    /// Otherwise, retries the server connection on state change until it is either connected,
    /// or we give up, then returns true if success or false if failure.
    async fn ensure_server_connection(connection: &RemoteConnection, retries: u32) -> bool {
        // We must subscribe to the events stream BEFORE checking the status.
        // This is so that between two lines of code, the status doesn't change before we've inited our stream.
        let mut events = connection.events();
        if connection.is_connected() {
            return true;
        }
        loop {
            let Some(event) = events.next().await else {
                continue;
            };
            match event {
                samod::DialerEvent::Connected { peer_info: _ } => return true,
                samod::DialerEvent::Reconnecting { attempt } => {
                    if attempt < retries {
                        continue;
                    }
                    return false;
                }
                _ => return false,
            }
        }
    }

    /// Request the sync task to checkout the latest ref on a branch the next opportunity.
    /// This will only work once Godot is safe to update.
    pub async fn request_checkout(&self, branch: &DocumentId) {
        let mut req = self.inner.requested_checkout.lock().await;
        *req = Some(branch.clone());
    }

    async fn get_metadata_handle(
        repo: &Repo,
        metadata_id: &DocumentId,
        connection: &RemoteConnection,
    ) -> Option<DocHandle> {
        // TODO: This is awkward; instead it would be great to create a fake old document and have it updated from the server asynchronously.
        // For now we must wait til the server connects, if we don't have the doc ID locally.
        // Before we continue, we must acquire a handle to the metadata document.
        // There are three cases to handle:
        //  a: The document exists on the local repository.
        //  b: The document exists on the server, and not the local repository.
        //  c: The document doesn't exist at all.

        // First, we check the local repository.
        let Ok(metadata_handle) = repo.find(metadata_id.clone()).await else {
            tracing::error!("Can't start the driver; the repo was immediately stopped!");
            return None;
        };

        match metadata_handle {
            // We found it locally, or the server connected REALLY quickly.
            Some(metadata_handle) => Some(metadata_handle),
            // We didn't find it locally. Try the server for a bit.
            None => {
                // If the hang gets annoying when starting, we could set this to 1 to reduce it to a minimum.
                if !Self::ensure_server_connection(&connection, 3).await {
                    tracing::error!(
                        "Couldn't find the metadata doc handle locally, and the server couldn't connect!"
                    );
                    return None;
                }

                // Try again on the server, if we were able to connect
                match repo.find(metadata_id.clone()).await {
                    Ok(Some(handle)) => Some(handle),
                    Ok(None) => {
                        tracing::error!(
                            "Couldn't find the metadata doc handle, even after connecting to the server!"
                        );
                        return None;
                    }
                    Err(_) => {
                        tracing::error!(
                            "Can't start the driver; the repo was immediately stopped!"
                        );
                        return None;
                    }
                }
            }
        }
    }

    pub async fn fork_branch(&self, name: String, branch: &DocumentId) {
        if let Some(id) = self.inner.branch_db.fork_branch(name, branch).await {
            self.request_checkout(&id).await;
        }
    }

    pub async fn merge_branch(&self, source: &DocumentId, target: &DocumentId) {
        self.inner.branch_db.merge_branch(source, target).await;
        self.inner.branch_db.delete_branch(source).await;
        self.request_checkout(target).await;
    }

    pub async fn discard_current_branch(&self) {
        let Some(checked_out_ref) = self.get_branch_db().get_checked_out_ref().await else {
            return;
        };

        let Some(branch_state) = self
            .get_branch_db()
            .get_branch_state(&checked_out_ref.branch())
            .await
        else {
            return;
        };

        let Some(fork_info) = &branch_state.forked_from else {
            return;
        };
        self.inner.branch_db.delete_branch(&branch_state.id).await;

        self.request_checkout(fork_info.branch()).await;
    }

    pub async fn create_merge_preview_branch(&self, source: &DocumentId, target: &DocumentId) {
        if let Some(id) = self
            .inner
            .branch_db
            .create_merge_preview_branch(source, target)
            .await
        {
            self.request_checkout(&id).await;
        }
    }

    pub async fn create_revert_preview_branch(&self, ref_: &HistoryRef) {
        if let Some(id) = self
            .get_branch_db()
            .create_revert_preview_branch(ref_.branch(), ref_)
            .await
        {
            self.request_checkout(&id).await;
        }
    }

    pub async fn confirm_revert_preview_branch(&self) {
        let Some(branch) = self.get_branch_db().get_checked_out_ref().await else {
            return;
        };
        let Some(branch_state) = self.get_branch_db().get_branch_state(branch.branch()).await
        else {
            return;
        };
        let Some(forked_from) = branch_state.forked_from else {
            return;
        };
        self.get_branch_db()
            .confirm_revert_preview_branch(branch.branch())
            .await;
        self.inner.branch_db.delete_branch(branch.branch()).await;
        self.request_checkout(forked_from.branch()).await;
    }

    pub async fn get_diff(&self, before: &HistoryRef, after: &HistoryRef) -> ProjectDiff {
        self.inner.differ.get_diff(before, after).await
        // ProjectDiff::default()
    }

    pub async fn get_metadata_doc(&self) -> Option<DocumentId> {
        self.inner
            .branch_db
            .get_metadata_state()
            .await
            .map(|(handle, _)| handle.document_id().clone())
    }

    pub async fn get_main_branch(&self) -> Option<DocumentId> {
        self.inner
            .branch_db
            .get_metadata_state()
            .await
            .map(|(_, doc)| doc.main_doc_id)
    }

    pub async fn get_connection_info(&self) -> Option<ConnectionInfo> {
        self.inner.peer_watcher.get_server_info()
    }

    pub fn set_safe_to_update_editor(&self, safe: bool) {
        self.inner
            .safe_to_update_editor
            .store(safe, Ordering::Relaxed);
    }

    pub fn get_branch_db(&self) -> BranchDb {
        self.inner.branch_db.clone()
    }

    // awkward
    pub fn get_filesystem_changes(&mut self) -> Vec<FileSystemEvent> {
        let mut fs_changes = Vec::new();
        while let Ok(msg) = self.file_changes_rx.try_recv() {
            fs_changes.push(msg);
        }
        fs_changes
    }

    // also awkward
    pub fn get_changes_rx(&self) -> watch::Receiver<Vec<CommitInfo>> {
        self.inner.change_ingester.get_changes_rx()
    }

    pub fn get_ref_rx(&self) -> watch::Receiver<Option<HistoryRef>> {
        self.inner.ref_tx.subscribe()
    }
}

impl DriverInner {
    /// Primary sync loop.
    async fn sync_main(&self) {
        loop {
            select! {
                _ = self.token.cancelled() => { break; }
                // If it lags, turn this down. Alternatively, we could use a different signal to sync.
                // Will cap to only once per frame due to the guard.
                _ = tokio::time::sleep(Duration::from_millis(5)) => {
                    self.sync().await;
                }
            }
        }
    }

    #[instrument(skip_all)]
    async fn sync(&self) {
        tracing::trace!("Syncing...");
        let old_checked_out_ref = self
            .branch_db
            .get_checked_out_ref_mut()
            .read()
            .await
            .clone();

        // Ensure we block the main thread inside of Rust while checking out a ref.
        // Very important to not allow Godot to explode while we're writing files!
        {
            tracing::trace!("Sync guarding...");
            let _guard;
            // rather awkward; we don't want to get stuck on the guard if we've canceled!
            select! {
                _ = self.token.cancelled() => { return; }
                _g = self.main_thread_block.wait() => {
                    _guard = _g;
                }
            }
            tracing::trace!("Passed guard.");
            if self.safe_to_update_editor.load(Ordering::Relaxed) {
                let changes = self.sync_correct_ref().await;
                for change in changes {
                    self.file_changes_tx.send(change).unwrap();
                }
            }
        }
        tracing::trace!("Attepmting to sync FS to automerge...");
        // Apply any watched FS updates to Automerge.
        // It doesn't matter if we're safe to update Godot, so this can go outside of the guard.
        if self.sync_fs_to_automerge.commit().await {
            self.change_ingester.request_ingestion();
        }

        let new_checked_out_ref = self
            .branch_db
            .get_checked_out_ref_mut()
            .read()
            .await
            .clone();

        // If we've changed branches, send the new checked out ref.
        if let Some(ref_) = &new_checked_out_ref {
            tracing::trace!("CHECKED OUT REF: {}", ref_.branch());
        } else {
            tracing::trace!("NO CHECKED OUT REF");
        }

        if new_checked_out_ref.as_ref().map(|r| r.branch())
            != old_checked_out_ref.as_ref().map(|r| r.branch())
        {
            // Whenever we change branches, we must manually ingest changes.
            // This is because we're in need of a new change list for sure!
            self.change_ingester.request_ingestion();
            self.ref_tx.send(new_checked_out_ref).unwrap();
        }
        tracing::trace!("Done with sync.");
    }

    async fn get_ref_for_sync(&self) -> Option<HistoryRef> {
        let mut requested_checkout = self.requested_checkout.lock().await;

        // The logic here:
        // - If we have a requested checkout that is valid, use that, and clear it
        // - If the requested checkout is invalid or empty, use the branch from the currently checked out ref
        // - If we don't have anything currently checked out, default to main.
        let req_branch = requested_checkout.clone();
        if let Some(requested_branch) = req_branch {
            if let Some(latest) = self
                .branch_db
                .get_latest_ref_on_branch(&requested_branch)
                .await
            {
                requested_checkout.take(); // clear it
                return Some(latest);
            }
        }

        let current_ref = self.branch_db.get_checked_out_ref_mut();
        let current_ref = current_ref.read().await;
        if let Some(current_ref) = current_ref.clone() {
            if let Some(ref_) = self
                .branch_db
                .get_latest_ref_on_branch(current_ref.branch())
                .await
            {
                return Some(ref_);
            }
        }
        if let Some(main_branch) = self.branch_db.get_main_branch().await {
            if let Some(ref_) = self.branch_db.get_latest_ref_on_branch(&main_branch).await {
                return Some(ref_);
            }
            tracing::error!(
                "Found main branch, but couldn't get the latest ref. Skipping checkout!"
            );
            return None;
        }
        tracing::error!(
            "No metadata doc checked out, or otherwise couldn't get main branch. Skipping checkout!"
        );
        return None;
    }

    /// If our current ref is out-of-date, try and check out a new ref.
    #[instrument(skip_all)]
    async fn sync_correct_ref(&self) -> Vec<FileSystemEvent> {
        // TODO (Lilith): There are inefficiencies with this strategy.
        // Basically, every time we save a file, it'll do a bunch of extra work.
        // It will first commit the changes, then it will check out the changes we just committed.
        // No files will be written of course, but it will still walk the tree of changes.
        // The same happens in reverse: when we check out a ref, it will attempt to commit and not
        // find any actual changes.
        // Maybe that's OK, we need to profile to see if it's a problem.

        if let Some(goal_ref) = self.get_ref_for_sync().await {
            self.sync_automerge_to_fs.checkout_ref(goal_ref).await
        } else {
            Vec::new()
        }
    }
}
