use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Serialize,Deserialize};

#[derive(Serialize, Deserialize)]
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
    pub status: String
}

#[derive(Serialize, Deserialize)]
pub struct ListFilesRequest {}

#[derive(Serialize, Deserialize)]
pub struct ListFilesResponse {
    pub files: Vec<FileMeta>
}

#[derive(Serialize, Deserialize)]
pub struct GetFileRequest {
    pub path: String
}

#[derive(Serialize, Deserialize)]
pub struct GetFileResponse {
    pub file: FileMeta
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
    pub file: FileMeta
}

#[derive(Serialize, Deserialize)]
pub struct DeleteFileRequest {
    pub path: String
}

#[derive(Serialize, Deserialize)]
pub struct DeleteFileResponse {}


pub fn hash_file(fp: &Path) -> Result<String> {
    let bytes = std::fs::read(fp)?;
    Ok(hash_bytes(&bytes))
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
}
