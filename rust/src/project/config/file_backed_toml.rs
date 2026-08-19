use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use thiserror::Error;
use tokio::{fs, io::AsyncWriteExt, sync::Mutex};
use toml::Table;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct FileBackedToml {
    path: PathBuf,
    inner: Arc<Mutex<FileBackedTomlInner>>,
}

#[derive(Debug, Default)]
struct FileBackedTomlInner {
    table: toml::Table,
    metadata: Option<(SystemTime, u64)>,
}

#[derive(Error, Debug)]
pub enum FileBackedTomlError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Serialize(#[from] toml::ser::Error),
}

async fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");

    let tmp = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));

    let result = async {
        let mut file = tokio::fs::File::create(&tmp).await?;
        file.write_all(contents.as_bytes()).await?;
        file.sync_all().await?;
        drop(file);

        // rename is atomic
        fs::rename(&tmp, path).await?;

        Ok::<_, std::io::Error>(())
    }
    .await;

    // Cleanup on failure
    if result.is_err() {
        let _ = fs::remove_file(&tmp).await;
    }
    result
}

impl FileBackedToml {
    pub fn new(path: &Path) -> Self {
        // Lazy-loaded; will be initialized on the first get() or set()
        Self {
            path: path.to_owned(),
            inner: Default::default(),
        }
    }

    pub async fn get(
        &self,
        section: &str,
        key: &str,
    ) -> Result<Option<toml::Value>, FileBackedTomlError> {
        let mut inner = self.inner.lock().await;
        inner.reload_backing(&self.path).await?;
        Ok(inner
            .table
            .get(section)
            .and_then(|section| section.as_table())
            .and_then(|table| table.get(key))
            .cloned())
    }

    pub async fn set(
        &self,
        section: &str,
        key: &str,
        value: Option<toml::Value>,
    ) -> Result<(), FileBackedTomlError> {
        let mut inner = self.inner.lock().await;
        inner.reload_backing(&self.path).await?;

        match value {
            Some(value) => {
                let table = inner
                    .table
                    .entry(section.to_owned())
                    .or_insert_with(|| toml::Value::Table(Table::new()));

                if !table.is_table() {
                    *table = toml::Value::Table(Table::new());
                }

                table.as_table_mut().unwrap().insert(key.to_owned(), value);
            }
            None => {
                if let Some(table) = inner.table.get_mut(section).and_then(|v| v.as_table_mut()) {
                    table.remove(key);
                }
            }
        }

        inner.write_backing(&self.path).await?;
        Ok(())
    }
}

impl FileBackedTomlInner {
    async fn reload_backing(&mut self, path: &Path) -> Result<(), FileBackedTomlError> {
        let metadata = Self::metadata(path).await?;
        // If these are both None, it means the file doesn't exist and our table is empty.
        if metadata != self.metadata {
            let table = self.read_or_create_file(path).await?;
            self.metadata = Self::metadata(path).await?;
            self.table = table;
        }
        Ok(())
    }

    async fn write_backing(&mut self, path: &Path) -> Result<(), FileBackedTomlError> {
        let contents = toml::to_string_pretty(&self.table)?;
        atomic_write(path, &contents).await?;

        // Refresh metadata after writing so that our next reload doesn't reread our own write.
        self.metadata = Self::metadata(path).await?;

        Ok(())
    }

    async fn read_or_create_file(
        &mut self,
        path: &Path,
    ) -> Result<toml::Table, FileBackedTomlError> {
        // Make the file if it doesn't exist.
        let str = match tokio::fs::read_to_string(path).await {
            Ok(str) => str,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Assuming the user deleted the file intentionally, clear the data.
                atomic_write(path, "").await?;
                return Ok(Table::new());
            }
            Err(e) => Err(e)?,
        };
        match str.parse::<Table>() {
            Ok(table) => Ok(table),
            Err(e) => {
                // The backing file is invalid. Log an error and try rewriting our stored data (which may be empty, that's OK.)
                // We need to rewrite our existing state in case a user makes a single typo in the TOML; we don't want to wipe the whole config!
                // So just use the known last-good file, if possible. (Otherwise if they just booted up, wipe. oh well.)
                tracing::error!(
                    "invalid TOML in backing file {:?}; resetting file (error: {e})",
                    path
                );
                self.write_backing(path).await?;
                Ok(self.table.clone())
            }
        }
    }

    async fn metadata(path: &Path) -> Result<Option<(SystemTime, u64)>, FileBackedTomlError> {
        let meta = match tokio::fs::metadata(path).await {
            Ok(meta) => meta,
            Err(e) => match e.kind() {
                std::io::ErrorKind::NotFound => return Ok(None),
                _ => Err(e)?,
            },
        };
        Ok(Some((
            // We want to early-panic here because if this ends up running on unsupported platforms,
            // we should know immediately that it can't run without refactor
            meta.modified()
                .expect("File modified time is not available on this platform!"),
            meta.len(),
        )))
    }
}
