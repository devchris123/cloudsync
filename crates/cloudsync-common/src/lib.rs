use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use std::io::Read;

#[derive(Serialize, Deserialize, Clone)]
pub struct FileMeta {
    pub path: String,
    pub size: u64,
    pub content_hash: String,
    pub version: u64,
    pub is_deleted: bool,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize)]
pub enum FileAction {
    Created,
    Modified,
    Deleted,
    Unchanged,
}

#[derive(Serialize, Deserialize)]
pub struct GetHealthRequest {}

#[derive(Serialize, Deserialize)]
pub struct GetHealthResponse {
    pub status: String,
}

#[derive(Serialize, Deserialize)]
pub struct ListFilesRequest {}

#[derive(Serialize, Deserialize)]
pub struct ListFilesResponse {
    pub files: Vec<FileMeta>,
}

#[derive(Serialize, Deserialize)]
pub struct GetFileRequest {
    pub path: String,
}

#[derive(Serialize, Deserialize)]
pub struct GetFileResponse {
    pub file: FileMeta,
}

// CreateFileRequest exists for consistency, but
// will not be sent over the wire as JSON
// Multipart will be used for file uploads.
#[derive(Serialize, Deserialize)]
pub struct CreateFileRequest {
    pub path: String,
    pub content: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub struct CreateFileResponse {
    pub file: FileMeta,
}

#[derive(Serialize, Deserialize)]
pub struct DeleteFileRequest {
    pub path: String,
}

#[derive(Serialize, Deserialize)]
pub struct DeleteFileResponse {}

/// Hashes a file using BLAKE3 with streaming 4MB reads.
/// This avoids filling up memory when working large files.
pub fn hash_file(fp: &Path) -> Result<String> {
    let mut file = std::fs::File::open(fp)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024]; // 4 MB

    loop {
        let bytes_read = file.read(&mut buf)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buf[..bytes_read]);
    }

    let res = hasher.finalize();
    Ok(res.to_hex().to_string())
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn it_returns_hash_from_bytes() {
        let bytes = b"hello world";
        let bytes2 = b"hello world2";

        let hash1 = hash_bytes(bytes);
        let hash2 = hash_bytes(bytes2);

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn it_returns_hash_from_file() {
        let dir = TempDir::new().unwrap();
        let path1 = dir.path().join("test1.txt");
        let path2 = dir.path().join("test2.txt");
        std::fs::write(&path1, "hello").unwrap();
        std::fs::write(&path2, "hello").unwrap();

        let hash1 = hash_file(&path1).unwrap();
        let hash2 = hash_file(&path2).unwrap();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn streaming_matches_bytes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello").unwrap();

        let hash1 = hash_file(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let hash2 = hash_bytes(&bytes);

        assert_eq!(hash1, hash2);
    }
}
