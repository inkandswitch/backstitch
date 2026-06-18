use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use std::fs::Metadata;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime};
use thiserror::Error;
use tokio::fs::{self};
use tokio::io::{self};
use tokio::select;
use tokio_util::sync::CancellationToken;
use wincode::{SchemaRead, SchemaWrite};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, SchemaRead, SchemaWrite)]
struct FileEntry {
    last_modified: SystemTime,
    size: u64,
    hash: [u8; 32],
}

#[derive(Error, Debug)]
pub enum IndexError {
    #[error("there was an error with the Fjall database: {0}")]
    DatabaseError(String),
    #[error("there was an error with the filesystem: {0}")]
    FilesystemError(String),
}

impl From<fjall::Error> for IndexError {
    fn from(value: fjall::Error) -> Self {
        Self::DatabaseError(value.to_string())
    }
}

impl From<std::io::Error> for IndexError {
    fn from(value: std::io::Error) -> Self {
        Self::FilesystemError(value.to_string())
    }
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
    keyspace: Option<Keyspace>,
    token: CancellationToken,
}

impl Drop for FileSystemIndex {
    fn drop(&mut self) {
        self.token.cancel()
    }
}

impl std::fmt::Debug for FileSystemIndex {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Result::Ok(())
    }
}

impl FileSystemIndex {
    pub async fn new<P: AsRef<Path>>(path: P) -> Result<Self, IndexError> {
        let db = Self::init_db(path).await?;
        let keyspace = db.keyspace("index", KeyspaceCreateOptions::default)?;
        let token = CancellationToken::new();

        let db_clone = db.clone();
        let token_clone = token.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));

            loop {
                select! {
                    _ = token_clone.cancelled() => {
                        break;
                    }
                    _ = interval.tick() => {
                        let db = db_clone.clone();
                        tokio::task::spawn_blocking(move || {
                            let _ = db.persist(PersistMode::Buffer);
                        });
                    }
                }
            }
        });

        Ok(Self {
            db,
            keyspace: Some(keyspace),
            token,
        })
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

        let _ = version.insert("version", current_version);

        Ok(db)
    }

    pub fn clear_cache(&mut self) -> Result<(), IndexError> {
        if let Some(keyspace) = self.keyspace.take() {
            self.db.delete_keyspace(keyspace)?;
            self.db.persist(PersistMode::SyncAll)?;
        };

        self.keyspace = Some(self.db.keyspace("index", KeyspaceCreateOptions::default)?);
        Ok(())
    }

    /// Get the current hash of a file, recomputing if needed
    pub async fn get_hash<P: AsRef<Path>>(&self, path: P) -> Result<blake3::Hash, IndexError> {
        let path_buf = path.as_ref().to_path_buf();
        let path_bytes = path.as_ref().as_os_str().as_encoded_bytes();
        let metadata = fs::metadata(&path_buf).await?;

        let db_hash = self.get_hash_from_db(&path_bytes, &metadata).await;
        let hash = match db_hash {
            Ok(hash) => hash,
            Err(e) => {
                tracing::error!(
                    "An index error occurred while trying to get the hash for {:?}: {:?}. Trying to manually recompute and return.",
                    path_buf,
                    e
                );
                None
            }
        };

        // DB fetch success
        if let Some(hash) = hash {
            return Ok(hash);
        }

        // Recompute hash
        let hash = tokio::task::spawn_blocking({
            let path = path_buf.clone();
            move || compute_hash(&path)
        })
        .await
        .map_err(|_| IndexError::FilesystemError(format!("Some sort of join error??")))??;

        match self.insert_hash_into_db(path_bytes, hash, &metadata).await {
            Ok(_) => {}
            Err(e) => {
                tracing::error!("Error reinserting hash for path {:?}: {:?}", path_buf, e)
            }
        };

        Ok(hash)
    }

    async fn insert_hash_into_db(
        &self,
        path: &[u8],
        hash: blake3::Hash,
        metadata: &Metadata,
    ) -> Result<(), IndexError> {
        let entry = FileEntry::from_metadata(&metadata, hash)?;
        let entry =
            wincode::serialize(&entry).map_err(|e| IndexError::DatabaseError(e.to_string()))?;
        self.keyspace
            .as_ref()
            .ok_or(IndexError::DatabaseError(format!("keyspace doesn't exist")))?
            .insert(path, entry)?;
        Ok(())
    }

    async fn get_hash_from_db(
        &self,
        path: &[u8],
        metadata: &Metadata,
    ) -> Result<Option<blake3::Hash>, IndexError> {
        if let Some(entry) = self
            .keyspace
            .as_ref()
            .ok_or(IndexError::DatabaseError(format!("keyspace doesn't exist")))?
            .get(path)?
        {
            let entry: FileEntry = wincode::deserialize(&entry)
                .map_err(|e| IndexError::DatabaseError(e.to_string()))?;
            if entry.matches(&metadata)? {
                return Ok(Some(entry.hash()));
            }
        }

        Ok(None)
    }
}

/// Compute BLAKE3 hash of a file
// I'm making this synchronous since it should be CPU-bound anyways, not io-bound.
// I don't want thread swapping to happen mid-hash due to tokio swapping us out.
// So, make sure to spawn_blocking this.
fn compute_hash(path: &Path) -> io::Result<blake3::Hash> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();

    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(hasher.finalize())
}
