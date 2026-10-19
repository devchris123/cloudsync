use cloudsync_common::{hash_bytes, hash_file};

pub const HASH_SIZE: usize = 64;

#[derive(Debug)]
pub enum StorageError {
    //// The hash of the reassembled file does not match the expected hash.
    HashMismatch,

    //// The size of the reassembled file does not match the expected size.
    SizeMismatch,

    //// An I/O error occurred while reading or writing files.
    Io(std::io::Error),

    //// An unexpected "unhandled" internal error occurred.
    Internal(anyhow::Error),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::HashMismatch => write!(f, "hash mismatch"),
            StorageError::SizeMismatch => write!(f, "size mismatch"),
            StorageError::Io(err) => write!(f, "io error: {}", err),
            StorageError::Internal(err) => write!(f, "internal error: {}", err),
        }
    }
}

impl From<anyhow::Error> for StorageError {
    fn from(err: anyhow::Error) -> Self {
        StorageError::Internal(err)
    }
}

impl From<std::io::Error> for StorageError {
    fn from(err: std::io::Error) -> Self {
        StorageError::Io(err)
    }
}

impl From<tempfile::PersistError> for StorageError {
    fn from(err: tempfile::PersistError) -> Self {
        StorageError::Io(err.error)
    }
}

/// Writes the given content to a file in the storage directory for the given namespace.
/// The file is named by the SHA-256 hash of the content, and the directory structure is
/// derived from the namespace and the first two characters of the hash.
///
/// Returns the SHA-256 hash of the content.
pub fn write(
    data_dir: &std::path::Path,
    namespace: &str,
    content: &[u8],
) -> anyhow::Result<String> {
    let content_hash = hash_bytes(content);
    let storage_dir = get_storage_dir(data_dir, namespace, &content_hash);
    std::fs::create_dir_all(&storage_dir)?;
    let path = storage_dir.join(&content_hash);
    std::fs::write(&path, content)?;
    Ok(content_hash)
}

/// Returns the path to the file in the storage directory for the given namespace and content hash.
pub fn get_storage_path(
    data_dir: &std::path::Path,
    namespace: &str,
    total_hash: &str,
) -> std::path::PathBuf {
    get_storage_dir(data_dir, namespace, total_hash).join(total_hash)
}

/// Returns true if the file exists in the storage directory for the given namespace and content hash.
pub fn exist_storage_path(data_dir: &std::path::Path, namespace: &str, total_hash: &str) -> bool {
    get_storage_dir(data_dir, namespace, total_hash)
        .join(total_hash)
        .exists()
}

/// Returns the path to the storage directory for the given namespace and content hash.
fn get_storage_dir(
    data_dir: &std::path::Path,
    namespace: &str,
    total_hash: &str,
) -> std::path::PathBuf {
    let namespace_hash = hash_bytes(namespace.as_bytes());
    data_dir.join(namespace_hash).join(&total_hash[0..2])
}

/// Reassembles the chunks of a file upload into a single file in the storage directory for the given namespace.
/// The chunks are expected to be in the staging directory, named by their index (0, 1, 2, ...).
///
/// The function checks that the total size of the reassembled file matches the expected size,
/// and that the hash of the reassembled file matches the expected hash.
pub fn reassemble_chunks(
    data_dir: &std::path::Path,
    namespace: &str,
    staging_dir: &std::path::Path,
    chunk_count: u64,
    expected_hash: &str,
    total_size: u64,
) -> Result<(std::path::PathBuf, u64), StorageError> {
    let storage_path = get_storage_path(data_dir, namespace, expected_hash);
    if storage_path.exists() {
        return Ok((storage_path, 0));
    }

    let storage_dir = storage_path
        .parent()
        .expect("blob path must have a parent direcory");
    std::fs::create_dir_all(storage_dir)?;
    let mut temp_storage_file = tempfile::NamedTempFile::new_in(storage_dir)?;

    let mut seen_bytes: u64 = 0;
    for chunk_index in 0..chunk_count {
        let chunk_path = staging_dir.join(chunk_index.to_string());
        let mut chunk_file = std::fs::File::open(chunk_path)?;
        let copied_bytes = std::io::copy(&mut chunk_file, &mut temp_storage_file)?;
        seen_bytes += copied_bytes;
    }
    if seen_bytes != total_size {
        tracing::warn!(
            "reassemble_chunks: expected {} bytes, but wrote {} bytes",
            total_size,
            seen_bytes
        );
        return Err(StorageError::SizeMismatch);
    }

    let actual_hash = hash_file(temp_storage_file.path())?;
    if actual_hash != expected_hash {
        return Err(StorageError::HashMismatch);
    }

    temp_storage_file.persist(&storage_path)?;

    Ok((storage_path, seen_bytes))
}

#[cfg(test)]
mod test {
    use crate::storage;

    use super::*;
    use tempfile::TempDir;
    use tokio::io::AsyncReadExt;

    #[test]
    fn test_malicious_namespace() {
        let dir = TempDir::new().unwrap();
        let dir = dir.path();
        let mal_ns = "../mytenant";
        let content = b"hello world";
        let content_hash = hash_bytes(content);
        let mal_ns_hash = hash_bytes(mal_ns.as_bytes());
        let expected_path = dir
            .join(mal_ns_hash)
            .join(&content_hash[0..2])
            .join(&content_hash);

        let written_hash = write(dir, mal_ns, content).unwrap();
        let read_path = get_storage_path(dir, mal_ns, &content_hash);

        assert_eq!(written_hash, content_hash);
        assert_eq!(read_path, expected_path);
        assert_eq!(std::fs::read(&expected_path).unwrap(), content);
    }

    #[test]
    fn test_tenant_isolation() {
        let dir = TempDir::new().unwrap();
        let dir = dir.path();
        let ns0 = "tenant1";
        let ns1 = "tenant2";
        let content = b"hello world";
        let content_hash = hash_bytes(content);

        write(dir, ns0, content).unwrap();
        write(dir, ns1, content).unwrap();

        let path0 = get_storage_path(dir, ns0, &content_hash);
        let path1 = get_storage_path(dir, ns1, &content_hash);

        assert_ne!(path0, path1);

        std::fs::remove_file(path0).unwrap();

        assert_eq!(std::fs::read(path1).unwrap(), content);
    }

    #[tokio::test]
    async fn test_write_read() {
        let dir = TempDir::new().unwrap();
        let dir = dir.path();

        let bytes = b"hello world";
        let hash = write(dir, "test", bytes).unwrap();

        assert_eq!(hash, hash_bytes(bytes));

        let file = read_async(dir, &hash).await.unwrap();
        let mut buf = Vec::new();
        file.take(1000).read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf.as_slice(), bytes);
    }

    async fn read_async(
        data_dir: &std::path::Path,
        content_hash: &str,
    ) -> anyhow::Result<tokio::fs::File> {
        let path = get_storage_path(data_dir, "test", content_hash);
        let file = tokio::fs::File::open(path).await?;
        Ok(file)
    }

    #[test]
    fn reassemble_chunks_writes_concatenated_blob() {
        let dir = TempDir::new().unwrap();
        let dir = dir.path();
        let staging = dir.join("staging");
        write_chunks(&staging, &[b"abc", b"def"]);
        let hash = hash_bytes(b"abcdef");

        let (path, _) = reassemble_chunks(dir, "test", &staging, 2, &hash, 6).unwrap();

        assert_eq!(std::fs::read(path).unwrap(), b"abcdef");
    }

    #[test]
    fn reassemble_chunks_is_noop_when_blob_already_exists() {
        // Regression for the soft-delete + re-upload bug. The first call writes
        // the blob; the second call uses *different* chunk bytes but the same
        // expected_hash — the bug was that we'd append/overwrite anyway and
        // corrupt the stored content. The guard means existing bytes win.
        let dir = TempDir::new().unwrap();
        let data_dir = dir.path();
        let staging1 = dir.path().join("staging-1");
        write_chunks(&staging1, &[b"abcdef"]);
        let hash = hash_bytes(b"abcdef");

        let (storage_path, _) =
            reassemble_chunks(data_dir, "test", &staging1, 1, &hash, 6).unwrap();
        assert_eq!(std::fs::read(&storage_path).unwrap(), b"abcdef");

        let staging2 = dir.path().join("staging-2");
        write_chunks(&staging2, &[b"XXXXXX"]);

        let (storage_path2, _) =
            reassemble_chunks(data_dir, "test", &staging2, 1, &hash, 6).unwrap();

        assert_eq!(storage_path2, storage_path);
        assert_eq!(std::fs::read(&storage_path).unwrap(), b"abcdef");
    }

    #[test]
    fn reassemble_chunks_errors_when_hash_does_not_match() {
        let dir = TempDir::new().unwrap();
        let staging = dir.path().join("staging");
        write_chunks(&staging, &[b"abc"]);
        let wrong_hash = hash_bytes(b"not-abc");

        let result = reassemble_chunks(dir.path(), "test", &staging, 1, &wrong_hash, 3);

        assert!(result.is_err());
        // And: no blob should be left lying around at the wrong-hash path.
        let leaked = get_storage_path(dir.path(), "test", &wrong_hash);
        assert!(!leaked.exists());
    }

    fn write_chunks(staging: &std::path::Path, chunks: &[&[u8]]) {
        std::fs::create_dir_all(staging).unwrap();
        for (i, c) in chunks.iter().enumerate() {
            std::fs::write(staging.join(i.to_string()), c).unwrap();
        }
    }

    #[test]
    fn test_exist_storage_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let content = b"test";
        let content_hash = hash_bytes(content);

        storage::write(dir.path(), "test", content).unwrap();

        let exist = storage::exist_storage_path(dir.path(), "test", &content_hash);

        assert!(exist);
    }
}
