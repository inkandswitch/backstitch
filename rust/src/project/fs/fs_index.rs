use anyhow::Result;
use fjall::{Database, KeyspaceCreateOptions, PersistMode};
use std::fs::Metadata;
use std::path::Path;
use std::time::SystemTime;
use tokio::fs::{self, File};
use tokio::io::{self, AsyncReadExt};
use wincode::{SchemaRead, SchemaWrite};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, SchemaRead, SchemaWrite)]
struct FileEntry {
    last_modified: SystemTime,
    size: u64,
    hash: [u8; 32],
}

impl FileEntry {
    fn from_metadata(metadata: &Metadata, hash: blake3::Hash) -> io::Result<Self> {
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

        Ok(self.last_modified == modified && self.size == metadata.len())
    }
}

#[derive(Clone)]
pub struct FileSystemIndex {
    db: fjall::Database,
}

impl FileSystemIndex {
    pub async fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = Self::init_db(path).await?;

        Ok(Self { db })
    }

    async fn init_db<P: AsRef<Path>>(path: P) -> fjall::Result<Database> {
        let db = Database::builder(path).open()?;

        let version = db.keyspace("version", KeyspaceCreateOptions::default)?;
        let bytes = version.get("version")?;

        // increment this when changes
        let current_version = "0";

        if let Some(version) = bytes {
            if version != current_version.as_bytes() {
                let index = db.keyspace("index", KeyspaceCreateOptions::default)?;
                db.delete_keyspace(index)?;
            }
        }

        db.persist(PersistMode::SyncAll)?;
        Ok(db)
    }

    /// Get the current hash of a file, recomputing if needed
    pub async fn get_hash<P: AsRef<Path>>(&self, path: P) -> Result<blake3::Hash> {
        let index = self.db.keyspace("index", KeyspaceCreateOptions::default)?;
        let path_buf = path.as_ref().to_path_buf();

        let metadata = fs::metadata(&path_buf).await?;

        let path_str = path.as_ref().to_string_lossy().to_string();
        {
            let index = index.clone();
            if let Some(entry) = tokio::task::spawn_blocking(move || index.get(path_str)).await?? {
                let entry: FileEntry = wincode::deserialize(&entry)?;
                if entry.matches(&metadata)? {
                    return Ok(entry.hash());
                }
            }
        }

        // Recompute hash
        let hash = compute_hash(&path_buf).await?;
        let entry = FileEntry::from_metadata(&metadata, hash)?;
        let entry = wincode::serialize(&entry)?;
        {
            let index = index.clone();
            tokio::task::spawn_blocking(move || index.insert(path_buf, entry)).await??;
        }
        self.db.persist(PersistMode::SyncAll)?;
        Ok(hash)
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
