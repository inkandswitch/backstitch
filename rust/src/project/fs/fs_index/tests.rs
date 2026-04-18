
use super::*;
use std::io::Write;
use tempfile::tempdir;
use tokio::fs;

fn write_file(path: &Path, contents: &[u8]) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(contents).unwrap();
    f.sync_all().unwrap();
}

#[tokio::test]
async fn test_new_creates_file_if_missing() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("index.bin");

    assert!(!index_path.exists());

    let _index = FileSystemIndex::new(&index_path).await.unwrap();

    assert!(index_path.exists());
}

#[tokio::test]
async fn test_get_hash_computes_and_returns_hash() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("file.txt");
    let index_path = dir.path().join("index.bin");

    write_file(&file_path, b"hello world");

    let index = FileSystemIndex::new(&index_path).await.unwrap();
    let hash = index.get_hash(&file_path).await.unwrap();
    let expected = blake3::hash(b"hello world");

    assert_eq!(hash, expected);
}

#[tokio::test]
async fn test_get_hash_uses_cached_value_if_unchanged() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("file.txt");
    let index_path = dir.path().join("index.bin");

    write_file(&file_path, b"hello");

    let index = FileSystemIndex::new(&index_path).await.unwrap();

    let first = index.get_hash(&file_path).await.unwrap();
    let second = index.get_hash(&file_path).await.unwrap();

    assert_eq!(first, second);
}

#[tokio::test]
async fn test_get_hash_recomputes_if_file_changes() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("file.txt");
    let index_path = dir.path().join("index.bin");

    write_file(&file_path, b"hello");

    let index = FileSystemIndex::new(&index_path).await.unwrap();
    let first = index.get_hash(&file_path).await.unwrap();

    // Modify file
    write_file(&file_path, b"goodbye");

    let second = index.get_hash(&file_path).await.unwrap();

    assert_ne!(first, second);
    assert_eq!(second, blake3::hash(b"goodbye"));
}

#[tokio::test]
async fn test_persistence_across_reloads() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("file.txt");
    let index_path = dir.path().join("index.bin");

    write_file(&file_path, b"persistent");

    // First instance
    let index = FileSystemIndex::new(&index_path).await.unwrap();
    let hash1 = index.get_hash(&file_path).await.unwrap();

    // Drop and reload
    drop(index);

    let index2 = FileSystemIndex::new(&index_path).await.unwrap();
    let hash2 = index2.get_hash(&file_path).await.unwrap();

    assert_eq!(hash1, hash2);
}

#[tokio::test]
async fn test_empty_index_file_is_handled() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("index.bin");

    // Create empty file
    fs::File::create(&index_path).await.unwrap();

    let index = FileSystemIndex::new(&index_path).await.unwrap();

    let entries = index.entries.read().await;
    assert!(entries.is_empty());
}

#[tokio::test]
async fn test_invalid_data_in_index_file_returns_error() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("index.bin");

    // Write garbage
    write_file(&index_path, b"not valid wincode");

    let result = FileSystemIndex::new(&index_path).await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn test_multiple_files_are_tracked_independently() {
    let dir = tempdir().unwrap();
    let index_path = dir.path().join("index.bin");

    let file1 = dir.path().join("a.txt");
    let file2 = dir.path().join("b.txt");

    write_file(&file1, b"a");
    write_file(&file2, b"b");

    let index = FileSystemIndex::new(&index_path).await.unwrap();

    let h1 = index.get_hash(&file1).await.unwrap();
    let h2 = index.get_hash(&file2).await.unwrap();

    assert_eq!(h1, blake3::hash(b"a"));
    assert_eq!(h2, blake3::hash(b"b"));
    assert_ne!(h1, h2);
}
