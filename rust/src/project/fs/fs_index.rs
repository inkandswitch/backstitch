use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tokio::sync::RwLock;
use wincode::{SchemaRead, SchemaWrite};
use tokio::fs::{self, File};
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, SchemaRead, SchemaWrite)]
struct FileEntry {
    last_modified: SystemTime,
    size: u64,
    hash: [u8; 32],
}

impl FileEntry {
    fn from_metadata(
        metadata: &Metadata,
        hash: blake3::Hash,
    ) -> io::Result<Self> {
        let modified = metadata.modified()?;
        let hash = hash.as_bytes();
        Ok(Self {
            last_modified: modified,
            size: metadata.len(),
            hash: *hash,
        })
    }

    fn hash(&self) -> blake3::Hash {
        blake3::Hash::from_bytes(self.hash)
    }

    fn matches(&self, metadata: &Metadata) -> io::Result<bool> {
        let modified = metadata.modified()?;

        Ok(self.last_modified == modified
            && self.size == metadata.len())
    }
}

// TODO: This is probably slow, consider using a proper database instead.
// Maybe redb? fjall?
#[derive(Debug, SchemaRead, SchemaWrite)]
struct PersistedIndex {
    entries: HashMap<String, FileEntry>,
}

#[derive(Debug, Default)]
pub struct FileSystemIndex {
    entries: RwLock<HashMap<PathBuf, FileEntry>>,
    backing_path: PathBuf,
}

impl FileSystemIndex {
    pub async fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path_buf = path.as_ref().to_path_buf();

        let entries = if path_buf.exists() {
            let mut file = File::open(&path_buf).await?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).await?;

            if buf.is_empty() {
                HashMap::new()
            } else {
                let persisted: PersistedIndex =
                    wincode::deserialize(&buf)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                persisted.entries.into_iter().map(|(k, v)| (PathBuf::from(k), v)).collect()
            }
        } else {
            // Create empty file
            File::create(&path_buf).await?;
            HashMap::new()
        };

        Ok(Self {
            entries: RwLock::new(entries),
            backing_path: path_buf,
        })
    }

    /// Get the current hash of a file, recomputing if needed
    pub async fn get_hash<P: AsRef<Path>>(&self, path: P) -> io::Result<blake3::Hash> {
        let entries = self.entries.read().await;
        let path_buf = path.as_ref().to_path_buf();

        let metadata = fs::metadata(&path_buf).await?;

        if let Some(entry) = entries.get(&path_buf) {
            if entry.matches(&metadata)? {
                return Ok(entry.hash());
            }
        }

        drop(entries);

        // Technically, there's a brief race condition here where the read lock gets discarded and the write lock gets reacquired.
        // That's OK though, because we're about to recompute the hash while the write guard is got, and there's never anything
        // wrong with recomputing the hash on a single entry twice. It just means it's more up to date.
        let mut entries = self.entries.write().await;

        // Recompute hash
        let hash = compute_hash(&path_buf).await?;
        let entry = FileEntry::from_metadata(&metadata, hash)?;
        entries.insert(path_buf, entry);
    println!("persisting...");
        self.persist(entries.clone()).await?; // persist after update

        Ok(hash)
    }

    /// Persist entire index to disk
    async fn persist(&self, entries: HashMap<PathBuf, FileEntry>) -> io::Result<()> {
        let tmp_path = self.backing_path.with_extension("tmp");

        let entries = entries.into_iter().map(|(k, v)| (k.to_string_lossy().to_string(), v)).collect();

        let data = wincode::serialize(&PersistedIndex {
            entries,
        })
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        {
            let mut tmp_file = File::create(&tmp_path).await?;
            tmp_file.write_all(&data).await?;
            tmp_file.sync_all().await?;
        }

        fs::rename(tmp_path, &self.backing_path).await?;
        Ok(())
    }
}

/// Compute BLAKE3 hash of a file
async fn compute_hash(path: &Path) -> io::Result<blake3::Hash> {
    let mut file = File::open(path).await?;
    let mut hasher = blake3::Hasher::new();

    let mut buffer = [0u8; 8192];
    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(hasher.finalize())
}