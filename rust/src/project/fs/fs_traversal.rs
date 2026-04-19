use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::{fs, sync::Mutex, task::JoinSet};

use crate::project::fs::fs_index::FileSystemIndex;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChangeType {
    Created,
    Modified,
    Deleted,
}

pub struct FileSystemTraversal;

// we might be able to speed this up by caching directories in the index? unsure
// this needs profiling to understand the perf impact. maybe it's fine.
impl FileSystemTraversal {
    /// Recursively traverses directory and returns hashes
    pub async fn get_all_files<P: AsRef<Path>, F: Fn(&Path) -> bool>(
        root: P,
        index: &FileSystemIndex,
        ignore: F
    ) -> HashMap<PathBuf, blake3::Hash> {
        let mut result = HashMap::new();
        Self::walk_dir(root.as_ref().to_path_buf(), index, &mut result, ignore).await;
        result
    }

    // can we parallelize this?
    async fn walk_dir<F: Fn(&Path) -> bool>(
        root: PathBuf,
        index: &FileSystemIndex,
        out: &mut HashMap<PathBuf, blake3::Hash>,
        ignore: F
    ) {
        let mut stack = vec![root];

        while let Some(path) = stack.pop() {
            if ignore(&path) {
                continue;
            }
            let mut entries = match fs::read_dir(&path).await {
                Ok(e) => e,
                Err(_) => continue,
            };

            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();

                if path.is_dir() {
                    stack.push(path);
                } else {
                    if let Some(hash) = index.get_hash(&path).await.ok() {
                        out.insert(path, hash);
                    }
                }
            }
        }
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
