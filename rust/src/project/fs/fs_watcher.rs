use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use async_stream::stream;
use futures::Stream;
use md5::Digest;
use notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use tokio::{
    sync::{
        Mutex,
        mpsc::{self},
    },
    task::JoinSet,
    time::sleep,
};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::{
    fs::file_utils::{FileContent, FileSystemEvent, get_buffer_and_hash},
    project::{branch_db::BranchDb, fs::fs_index::FileSystemIndex},
};

/// Watches a directory for filesystem changes, and emits them as a stream.
#[derive(Debug, Clone)]
pub struct FileSystemWatcher {
    watch_path: PathBuf,
    branch_db: BranchDb,
    found_ignored_paths: Arc<Mutex<HashSet<PathBuf>>>,
}

pub enum WatcherEvent {
    FileTouched(PathBuf)
}

impl FileSystemWatcher {
    // Handle file creation and modification events
    async fn handle_file_event(
        &self,
        path: &PathBuf,
    ) -> Option<WatcherEvent> {
        // Skip if path matches any ignore pattern
        if self.branch_db.should_ignore(&path) {
            return None;
        }

        tracing::debug!("handling filesystem event: {:?}", path);

        // file deleted
        if !path.exists() {
            return Some(WatcherEvent::FileTouched(path.clone()));
        }

        if path.is_file() {
            return Some(WatcherEvent::FileTouched(path.clone()));
        }
        return None;
    }

    // Watch the filesystem for meaningful changes
    pub async fn start_watching(
        path: PathBuf,
        branch_db: BranchDb,
    ) -> impl Stream<Item = WatcherEvent> {
        let (notify_tx, notify_rx) = mpsc::unbounded_channel();
        let notify_tx_clone = notify_tx.clone();
        let mut debouncer = new_debouncer(
            Duration::from_millis(100),
            None,
            move |events: DebounceEventResult| {
                let Ok(events) = events else {
                    return;
                };
                notify_tx_clone.send(events).unwrap();
            },
        )
        .unwrap();

        // Begin the watch
        // I'm assuming that notify uses good RAII and stops watching when we kill the handle.... hopefully.
        debouncer.watch(&path, RecursiveMode::Recursive).unwrap();

        let this = FileSystemWatcher {
            watch_path: path,
            branch_db,
            found_ignored_paths: Arc::new(Mutex::new(HashSet::new())),
        };

        for path in this.found_ignored_paths.lock().await.iter() {
            let _ret = debouncer.unwatch(path);
        }
        let stream = UnboundedReceiverStream::new(notify_rx);
        // Process both file system events and update eventss
        stream! {
            // move the debouncer into the returned stream
            let _keep_alive = debouncer;
            // Handle file system events
            for await notify_events in stream {
                for notify_event in notify_events {
                    match notify_event.kind {
                        notify::EventKind::Any => continue,
                        notify::EventKind::Access(_) => continue,
                        notify::EventKind::Create(_) => (),
                        notify::EventKind::Modify(_) => (),
                        notify::EventKind::Remove(_) => (),
                        notify::EventKind::Other => continue,
                    };
                    for path in &notify_event.paths {
                        if let Some(evt) = this.handle_file_event(path).await {
                            yield evt;
                        }
                    }
                }
            }
            tracing::debug!("fs_watcher shutting down!");
        }
    }
}
