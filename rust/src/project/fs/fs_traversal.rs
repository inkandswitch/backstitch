use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use futures::{StreamExt, stream};
use jwalk::WalkDir;
use tokio::{fs, sync::Mutex, task::JoinSet};
use tracing::instrument;

use crate::project::fs::fs_index::FileSystemIndex;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChangeType {
    Created,
    Modified,
    Deleted,
}

pub struct FileSystemTraversal;

impl FileSystemTraversal {
    pub async fn get_all_files<P, F>(
        root: P,
        index: &FileSystemIndex,
        ignore: F,
    ) -> HashMap<PathBuf, blake3::Hash>
    where
        P: AsRef<Path> + Send + 'static,
        F: Fn(&Path) -> bool + Sync + Send + 'static,
    {
        let ignore = Arc::new(ignore);
        let ignore2 = ignore.clone();

        let files = tokio::task::spawn_blocking(move || {
            WalkDir::new(root)
                .process_read_dir(move |_, _, _, children| {
                    children.retain(|dir_entry_result| {
                        if let Ok(entry) = dir_entry_result {
                            !ignore2(entry.path().as_path())
                        } else {
                            false
                        }
                    });
                })
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| entry.path())
                .filter(|path| !ignore(path))
                .collect::<Vec<PathBuf>>()
        }).await.unwrap();

        stream::iter(files)
            .map(|file| {
                let index = index.clone();
                async move {
                    index
                        .get_hash(&file)
                        .await
                        .map(|hash| (file, hash))
                }
            })
            .buffer_unordered(64)
            .filter_map(|r| async move { r.ok() })
            .collect()
            .await
    }
    
    pub fn get_file_changes<K: AsRef<Path>>(
        before: HashMap<K, blake3::Hash>,
        after: HashMap<K, blake3::Hash>,
    ) -> HashMap<K, ChangeType> where K: Eq + std::hash::Hash + Clone {
        let mut changes = HashMap::new();

        let before_keys: HashSet<_> = before.keys().cloned().collect();
        let after_keys: HashSet<_> = after.keys().cloned().collect();

        for (path, after_hash) in &after {
            match before.get(path) {
                None => {
                    changes.insert(path.clone(), ChangeType::Created);
                }
                Some(before_hash) => {
                    if before_hash != after_hash {
                        changes.insert(path.clone(), ChangeType::Modified);
                    }
                }
            }
        }

        for path in before_keys.difference(&after_keys) {
            changes.insert((*path).clone(), ChangeType::Deleted);
        }

        changes
    }
}
