use crate::db::open_db;
use chrono::Utc;
use cloudsync_common::{
    CreateFileResponse, DeleteFileResponse, FileMeta, FinalizeUploadResponse, GetUploadResponse,
    InitUploadRequest, InitUploadResponse, ListFilesResponse, Upload,
};
use redb::Database;
use std::cell::RefCell;
use tempfile::TempDir;

use crate::sync::core::{DownloadFileResponse, SyncApi};

pub struct MockClient {
    pub files: RefCell<Vec<FileMeta>>,
    pub list_count: RefCell<u64>,
    pub create_count: RefCell<u64>,
    pub get_count: RefCell<u64>,
    pub delete_count: RefCell<u64>,
    pub init_upload_count: RefCell<u64>,
    pub send_chunk_count: RefCell<u64>,
    pub send_chunk_fails: RefCell<bool>,
    pub get_upload_count: RefCell<u64>,
    pub get_upload_chunks_received: RefCell<Vec<u32>>,
    pub get_upload_chunk_count: RefCell<u64>,
    pub get_upload_fail_id: RefCell<Option<String>>,
    pub finalize_upload_count: RefCell<u64>,
    pub fail_path: RefCell<Option<String>>,
}

impl MockClient {
    pub fn new(files: Vec<FileMeta>) -> Self {
        MockClient {
            files: RefCell::new(files),
            list_count: RefCell::new(0),
            create_count: RefCell::new(0),
            get_count: RefCell::new(0),
            delete_count: RefCell::new(0),
            init_upload_count: RefCell::new(0),
            send_chunk_count: RefCell::new(0),
            send_chunk_fails: RefCell::new(false),
            get_upload_count: RefCell::new(0),
            get_upload_chunks_received: RefCell::new(vec![]),
            get_upload_chunk_count: RefCell::new(0),
            get_upload_fail_id: RefCell::new(None),
            finalize_upload_count: RefCell::new(0),
            fail_path: RefCell::new(None),
        }
    }

    pub fn set_files(&self, files: Vec<FileMeta>) {
        *self.files.borrow_mut() = files;
    }

    pub fn set_fail_path(&self, path: String) {
        *self.fail_path.borrow_mut() = Some(path);
    }

    pub fn set_send_chunk_fails(&self, fails: bool) {
        *self.send_chunk_fails.borrow_mut() = fails;
    }

    pub fn set_upload_chunks_received(&self, chunks: Vec<u32>) {
        *self.get_upload_chunks_received.borrow_mut() = chunks;
    }

    pub fn set_upload_chunk_count(&self, chunk_count: u64) {
        *self.get_upload_chunk_count.borrow_mut() = chunk_count;
    }

    pub fn set_upload_fail_id(&self, upload_id: String) {
        *self.get_upload_fail_id.borrow_mut() = Some(upload_id);
    }
}

impl SyncApi for MockClient {
    async fn list_files(&self) -> anyhow::Result<ListFilesResponse> {
        *self.list_count.borrow_mut() += 1;
        let files = self.files.borrow().clone();
        Ok(ListFilesResponse { files })
    }

    async fn create_file(
        &self,
        _path: &str,
        _content: Vec<u8>,
    ) -> anyhow::Result<CreateFileResponse> {
        *self.create_count.borrow_mut() += 1;
        Ok(CreateFileResponse {
            file: FileMeta {
                path: "".to_string(),
                size: 0,
                content_hash: "".to_string(),
                version: 0,
                is_deleted: false,
                created_at: chrono::Utc::now(),
                modified_at: chrono::Utc::now(),
            },
        })
    }

    async fn get_file(&self, path: &str, _bytes: u64) -> anyhow::Result<DownloadFileResponse> {
        if self.fail_path.borrow().as_deref() == Some(path) {
            anyhow::bail!("error");
        }
        *self.get_count.borrow_mut() += 1;
        Ok(DownloadFileResponse {
            resumed: false,
            stream: Box::new(std::io::Cursor::new(Vec::new())),
        })
    }

    async fn delete_file(&self, _path: &str) -> anyhow::Result<DeleteFileResponse> {
        *self.delete_count.borrow_mut() += 1;
        Ok(DeleteFileResponse {})
    }

    async fn init_upload(&self, _request: InitUploadRequest) -> anyhow::Result<InitUploadResponse> {
        *self.init_upload_count.borrow_mut() += 1;
        Ok(InitUploadResponse {
            upload_id: "".to_string(),
        })
    }

    async fn send_chunk(
        &self,
        _upload_id: &str,
        _chunk_index: u32,
        _content: Vec<u8>,
    ) -> anyhow::Result<()> {
        *self.send_chunk_count.borrow_mut() += 1;
        if *self.send_chunk_fails.borrow() {
            anyhow::bail!("error sending chunks");
        }
        Ok(())
    }

    async fn get_upload(&self, upload_id: &str) -> anyhow::Result<GetUploadResponse> {
        *self.get_upload_count.borrow_mut() += 1;
        if self.get_upload_fail_id.borrow().as_deref() == Some(upload_id) {
            anyhow::bail!("error");
        }
        Ok(GetUploadResponse {
            upload: Upload {
                path: "".to_string(),
                total_size: 10,
                upload_id: "".to_string(),
                total_hash: "".to_string(),
                chunk_count: *self.get_upload_chunk_count.borrow(),
                chunks_received: (*self.get_upload_chunks_received.borrow()).clone(),
                created_at: Utc::now(),
                modified_at: Utc::now(),
            },
        })
    }

    async fn finalize_upload(&self, _upload_id: &str) -> anyhow::Result<FinalizeUploadResponse> {
        *self.finalize_upload_count.borrow_mut() += 1;
        Ok(FinalizeUploadResponse {
            file: FileMeta {
                path: "".to_string(),
                size: 0,
                content_hash: "".to_string(),
                version: 1,
                is_deleted: false,
                created_at: Utc::now(),
                modified_at: Utc::now(),
            },
        })
    }
}

pub fn setup_test_deps() -> (Database, MockClient, TempDir) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(temp_dir.path().join(".cloudsync")).unwrap();
    let db = open_db(temp_dir.path()).unwrap();
    let mock_client = MockClient::new(Vec::new());
    return (db, mock_client, temp_dir);
}
