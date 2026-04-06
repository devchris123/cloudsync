use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::FileMeta;

#[derive(Serialize, Deserialize, Clone)]
pub struct Upload {
    pub path: String,
    pub total_size: u64,
    pub upload_id: String,
    pub total_hash: String,
    pub chunk_count: u64,
    pub chunks_received: Vec<u32>,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize)]
pub struct InitUploadRequest {
    pub path: String,
    pub total_size: u64,
    pub total_hash: String,
    pub chunk_count: u64,
}

#[derive(Serialize, Deserialize)]
pub struct InitUploadResponse {
    pub upload_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct ReplaceChunkResponse {
    pub chunk_index: u32,
}

#[derive(Serialize, Deserialize)]
pub struct GetUploadResponse {
    pub upload: Upload,
}

#[derive(Serialize, Deserialize)]
pub struct FinalizeUploadResponse {
    pub file: FileMeta,
}
