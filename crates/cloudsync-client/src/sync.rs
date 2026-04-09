use std::io::Read;
use std::path::Path;

use cloudsync_common::{
    CreateFileResponse, DeleteFileResponse, FileMeta, FinalizeUploadResponse, GetUploadResponse,
    InitUploadRequest, InitUploadResponse, ListFilesResponse, hash_bytes, hash_file,
};
use redb::Database;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncRead;

use crate::db;
use crate::scanner::{self, scan_dir};

pub const CHUNK_SIZE: u64 = 4 * 1024 * 1024;

#[allow(async_fn_in_trait)]
pub trait SyncApi {
    async fn list_files(&self) -> anyhow::Result<ListFilesResponse>;
    async fn create_file(&self, path: &str, content: Vec<u8>)
    -> anyhow::Result<CreateFileResponse>;
    async fn get_file(&self, path: &str) -> anyhow::Result<Box<dyn AsyncRead + Unpin + Send>>;
    async fn delete_file(&self, path: &str) -> anyhow::Result<DeleteFileResponse>;
    async fn init_upload(&self, request: InitUploadRequest) -> anyhow::Result<InitUploadResponse>;
    async fn send_chunk(
        &self,
        upload_id: &str,
        chunk_index: u32,
        content: Vec<u8>,
    ) -> anyhow::Result<()>;
    async fn get_upload(&self, upload_id: &str) -> anyhow::Result<GetUploadResponse>;
    async fn finalize_upload(&self, upload_id: &str) -> anyhow::Result<FinalizeUploadResponse>;
}

#[derive(Serialize, Deserialize)]
pub struct SyncRecord {
    pub path: String,
    pub local_hash: String,
    pub server_version: u64,
}

pub async fn push(
    db: &Database,
    sync_client: &impl SyncApi,
    sync_root: &Path,
) -> anyhow::Result<()> {
    let ignored = scanner::get_ignored(sync_root);
    let local_paths = scanner::scan_dir(sync_root, &ignored)?;

    for local_path in local_paths.iter() {
        let total_size = local_path.metadata()?.len();
        if total_size < CHUNK_SIZE {
            if let Err(e) = push_single_file(db, sync_client, sync_root, local_path).await {
                println!(
                    "error pushing {}: {}",
                    &local_path.to_str().unwrap().to_string(),
                    e
                );
                continue;
            }
        } else if let Err(e) =
            push_single_file_chunked(db, sync_client, sync_root, local_path).await
        {
            println!(
                "error pushing chunked {}: {}",
                &local_path.to_str().unwrap().to_string(),
                e
            );
            continue;
        }
    }

    let sync_records = db::list(db)?;
    for sr in sync_records {
        if !sync_root.join(&sr.path).exists()
            && let Err(e) = delete_file(sync_client, db, &sr.path).await
        {
            println!("error deleting {}: {}", &sr.path, e);
            continue;
        }
    }
    Ok(())
}

async fn push_single_file(
    db: &Database,
    sync_client: &impl SyncApi,
    sync_root: &Path,
    local_path: &Path,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(local_path)?;
    let hash = hash_bytes(&bytes);
    let rel_path: String = local_path
        .strip_prefix(sync_root)?
        .to_str()
        .unwrap()
        .to_string();
    let sync_record = db::get(db, &rel_path)?;
    if let Some(sr) = sync_record
        && sr.local_hash == hash
    {
        return Ok(());
    }
    let resp = sync_client.create_file(&rel_path, bytes).await?;
    let sync_record = SyncRecord {
        path: rel_path.clone(),
        local_hash: hash,
        server_version: resp.file.version,
    };
    db::put(db, sync_record)?;
    println!("pushed: {}", &rel_path);
    Ok(())
}

pub async fn push_single_file_chunked(
    db: &Database,
    sync_client: &impl SyncApi,
    sync_root: &Path,
    local_path: &Path,
) -> anyhow::Result<()> {
    let total_size = local_path.metadata()?.len();
    let chunk_count = total_size.div_ceil(CHUNK_SIZE);
    let hash = hash_file(local_path)?;
    let rel_path = local_path
        .strip_prefix(sync_root)?
        .to_str()
        .unwrap()
        .to_string();
    let sync_record = db::get(db, &rel_path)?;
    if let Some(sr) = sync_record
        && sr.local_hash == hash
    {
        return Ok(());
    }
    let upload = sync_client
        .init_upload(InitUploadRequest {
            path: rel_path.clone(),
            total_size,
            total_hash: hash.clone(),
            chunk_count,
        })
        .await?;
    let mut file = std::fs::File::open(local_path)?;
    for i in 0..chunk_count {
        let mut buf = Vec::new();
        (&mut file).take(CHUNK_SIZE).read_to_end(&mut buf)?;
        sync_client
            .send_chunk(&upload.upload_id, i as u32, buf)
            .await?;
    }
    let resp = sync_client.finalize_upload(&upload.upload_id).await?;
    let sync_record = SyncRecord {
        path: rel_path.clone(),
        local_hash: hash,
        server_version: resp.file.version,
    };
    db::put(db, sync_record)?;
    println!("pushed: {}", &rel_path);
    Ok(())
}

pub async fn delete_file(
    sync_client: &impl SyncApi,
    db: &Database,
    path: &str,
) -> anyhow::Result<()> {
    sync_client.delete_file(path).await?;
    db::delete(db, path)?;
    println!("deleted (server): {}", path);
    Ok(())
}

pub async fn pull(
    db: &Database,
    sync_client: &impl SyncApi,
    sync_root: &Path,
) -> anyhow::Result<()> {
    let file_metas = sync_client.list_files().await?.files;
    for file_meta in file_metas {
        if let Err(e) = pull_single_file(db, sync_client, sync_root, &file_meta).await {
            println!("error pulling {}: {}", &file_meta.path, e);
            continue;
        }
    }
    Ok(())
}

async fn pull_single_file(
    db: &Database,
    sync_client: &impl SyncApi,
    sync_root: &Path,
    file_meta: &FileMeta,
) -> anyhow::Result<()> {
    let record = db::get(db, &file_meta.path)?;

    if let Some(record) = record {
        if record.server_version == file_meta.version {
            return Ok(());
        }
        if record.server_version < file_meta.version {
            let local_path = &sync_root.join(&file_meta.path);
            if local_path.exists() {
                let hash = hash_file(local_path)?;
                if hash != record.local_hash {
                    println!("Conflict: {} changed locally and on server", file_meta.path);
                    let rel_path: &Path = std::path::Path::new(&file_meta.path);
                    let stem = rel_path.file_stem().unwrap_or_default().to_str().unwrap();
                    let ext = rel_path
                        .extension()
                        .map(|e| format!(".{}", e.to_str().unwrap()))
                        .unwrap_or_default();
                    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
                    let conflict_path = format!("{}.conflict.{}{}", stem, timestamp, ext);
                    let conflict_path = sync_root
                        .join(&file_meta.path)
                        .with_file_name(conflict_path);
                    let mut file = tokio::fs::File::create(&conflict_path).await?;
                    let mut stream_reader = sync_client.get_file(&file_meta.path).await?;
                    tokio::io::copy(&mut stream_reader, &mut file).await?;
                    println!("Conflict: resolve conflict in {}", conflict_path.display());
                    return Ok(());
                }
            }
        }
    }
    let local_path = &sync_root.join(&file_meta.path);
    let parent_dir = std::path::Path::new(local_path).parent();
    if let Some(parent_dir) = parent_dir {
        std::fs::create_dir_all(parent_dir)?;
    };
    let mut stream_reader = sync_client.get_file(&file_meta.path).await?;
    let mut file = tokio::fs::File::create(local_path).await?;
    tokio::io::copy(&mut stream_reader, &mut file).await?;
    let sync_record = SyncRecord {
        path: file_meta.path.clone(),
        local_hash: hash_file(local_path)?,
        server_version: file_meta.version,
    };
    db::put(db, sync_record)?;
    println!("pulled: {}", &file_meta.path);

    Ok(())
}

pub async fn status(
    db: &Database,
    sync_client: &impl SyncApi,
    sync_root: &Path,
) -> anyhow::Result<()> {
    let ignored = &scanner::get_ignored(sync_root);
    let local_files = scan_dir(sync_root, ignored)?;

    let file_metas = sync_client.list_files().await?.files;
    for local_file in local_files.iter() {
        let rel_path = local_file
            .strip_prefix(sync_root)?
            .to_str()
            .unwrap()
            .to_string();
        let sync_record = db::get(db, &rel_path)?;

        let Some(sync_record) = sync_record else {
            println!("{} - new (local)", &rel_path);
            continue;
        };
        let server_hash = file_metas.iter().find(|f| f.path == rel_path);
        let hash = hash_file(local_file)?;
        if hash != sync_record.local_hash {
            if let Some(sf) = server_hash
                && sf.version > sync_record.server_version
            {
                println!("{} - conflict", &rel_path);
                continue;
            }
            println!("{} - update (local)", &rel_path);
            continue;
        }
        if let Some(server_hash) = server_hash
            && server_hash.content_hash != sync_record.local_hash
        {
            println!("{} - update (server)", &rel_path);
            continue;
        }
        println!("{} - no update", &rel_path);
    }

    for server_file in file_metas {
        if local_files.iter().any(|f| {
            let rel_path = f.strip_prefix(sync_root).ok().and_then(|f| f.to_str());
            rel_path == Some(&server_file.path)
        }) {
            continue;
        }
        let path = sync_root.join(&server_file.path);
        let sync_record = db::get(db, &server_file.path)?;
        if sync_record.is_none() {
            println!("{} - new (server)", server_file.path)
        } else if !path.exists() {
            println!("{} - deleted (local)", &server_file.path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use chrono::Utc;
    use cloudsync_common::Upload;
    use tempfile::TempDir;

    use crate::db::open_db;

    use super::*;

    struct MockClient {
        files: RefCell<Vec<FileMeta>>,
        list_count: RefCell<u64>,
        create_count: RefCell<u64>,
        get_count: RefCell<u64>,
        delete_count: RefCell<u64>,
        init_upload_count: RefCell<u64>,
        send_chunk_count: RefCell<u64>,
        get_upload_count: RefCell<u64>,
        finalize_upload_count: RefCell<u64>,
        fail_path: RefCell<Option<String>>,
    }

    impl MockClient {
        fn new(files: Vec<FileMeta>) -> Self {
            MockClient {
                files: RefCell::new(files),
                list_count: RefCell::new(0),
                create_count: RefCell::new(0),
                get_count: RefCell::new(0),
                delete_count: RefCell::new(0),
                init_upload_count: RefCell::new(0),
                send_chunk_count: RefCell::new(0),
                get_upload_count: RefCell::new(0),
                finalize_upload_count: RefCell::new(0),
                fail_path: RefCell::new(None),
            }
        }

        fn set_files(&self, files: Vec<FileMeta>) {
            *self.files.borrow_mut() = files;
        }

        fn set_fail_path(&self, path: String) {
            *self.fail_path.borrow_mut() = Some(path);
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

        async fn get_file(&self, path: &str) -> anyhow::Result<Box<dyn AsyncRead + Unpin + Send>> {
            if self.fail_path.borrow().as_deref() == Some(path) {
                anyhow::bail!("error");
            }
            *self.get_count.borrow_mut() += 1;
            Ok(Box::new(std::io::Cursor::new(Vec::new())))
        }

        async fn delete_file(&self, _path: &str) -> anyhow::Result<DeleteFileResponse> {
            *self.delete_count.borrow_mut() += 1;
            Ok(DeleteFileResponse {})
        }

        async fn init_upload(
            &self,
            _request: InitUploadRequest,
        ) -> anyhow::Result<InitUploadResponse> {
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
            Ok(())
        }

        async fn get_upload(&self, _upload_id: &str) -> anyhow::Result<GetUploadResponse> {
            *self.get_upload_count.borrow_mut() += 1;
            Ok(GetUploadResponse {
                upload: Upload {
                    path: "".to_string(),
                    total_size: 10,
                    upload_id: "".to_string(),
                    total_hash: "".to_string(),
                    chunk_count: 1,
                    chunks_received: Vec::new(),
                    created_at: Utc::now(),
                    modified_at: Utc::now(),
                },
            })
        }

        async fn finalize_upload(
            &self,
            _upload_id: &str,
        ) -> anyhow::Result<FinalizeUploadResponse> {
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

    fn setup() -> (Database, MockClient, TempDir) {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp_dir.path().join(".cloudsync")).unwrap();
        let db = open_db(temp_dir.path()).unwrap();
        let mock_client = MockClient::new(Vec::new());
        return (db, mock_client, temp_dir);
    }

    #[tokio::test]
    async fn test_push_single_file() {
        let (db, mock_client, temp_dir) = setup();
        let file = temp_dir.path().join("file0");
        let bytes = b"hello world";
        std::fs::write(&file, bytes).unwrap();

        push_single_file(&db, &mock_client, temp_dir.path(), &file)
            .await
            .unwrap();

        let record = db::get(&db, "file0").unwrap();
        assert!(record.is_some());
        assert_eq!(*mock_client.create_count.borrow(), 1);
    }

    #[tokio::test]
    async fn test_push_single_file_chunked() {
        let (db, mock_client, temp_dir) = setup();
        let file = temp_dir.path().join("file0");
        let bytes = vec![0u8; 10 * 1024 * 1024];
        std::fs::write(&file, bytes).unwrap();

        push_single_file_chunked(&db, &mock_client, temp_dir.path(), &file)
            .await
            .unwrap();

        let record = db::get(&db, "file0").unwrap();
        assert!(record.is_some());
        assert_eq!(*mock_client.init_upload_count.borrow(), 1);
        assert_eq!(*mock_client.send_chunk_count.borrow(), 3);
        assert_eq!(*mock_client.finalize_upload_count.borrow(), 1);
    }

    #[tokio::test]
    async fn test_skip_file() {
        let (db, mock_client, temp_dir) = setup();
        let file = temp_dir.path().join("file0");
        std::fs::write(&file, b"hello world").unwrap();
        let bytes = b"hello world";
        let sync_record = SyncRecord {
            path: "file0".to_string(),
            local_hash: hash_bytes(bytes),
            server_version: 0,
        };
        db::put(&db, sync_record).unwrap();

        push_single_file(&db, &mock_client, temp_dir.path(), &file)
            .await
            .unwrap();

        assert_eq!(*mock_client.create_count.borrow(), 0);
    }

    #[tokio::test]
    async fn test_push_creates_files() {
        let (db, mock_client, temp_dir) = setup();
        let file0 = temp_dir.path().join("file0");
        let file1 = temp_dir.path().join("file1");
        std::fs::write(file0, b"hello world").unwrap();
        std::fs::write(file1, b"hello world").unwrap();

        push(&db, &mock_client, temp_dir.path()).await.unwrap();

        assert_eq!(*mock_client.create_count.borrow(), 2);
        assert_eq!(*mock_client.delete_count.borrow(), 0);
    }

    #[tokio::test]
    async fn test_push_deletes_files() {
        let (db, mock_client, temp_dir) = setup();
        let bytes: &[u8; 11] = b"hello world";
        let sync_record = SyncRecord {
            path: "file0".to_string(),
            local_hash: hash_bytes(bytes),
            server_version: 0,
        };
        db::put(&db, sync_record).unwrap();

        push(&db, &mock_client, temp_dir.path()).await.unwrap();

        assert_eq!(*mock_client.create_count.borrow(), 0);
        assert_eq!(*mock_client.delete_count.borrow(), 1);
    }

    #[tokio::test]
    async fn test_push_creates_updated_files() {
        let (db, mock_client, temp_dir) = setup();
        let file0 = temp_dir.path().join("file0");
        let file1 = temp_dir.path().join("file1");
        std::fs::write(file0, b"hello world").unwrap();
        std::fs::write(file1, b"hello world").unwrap();
        let bytes: &[u8; 11] = b"hello world";
        let sync_record = SyncRecord {
            path: "file0".to_string(),
            local_hash: hash_bytes(bytes),
            server_version: 0,
        };
        db::put(&db, sync_record).unwrap();

        push(&db, &mock_client, temp_dir.path()).await.unwrap();

        assert_eq!(*mock_client.create_count.borrow(), 1);
        assert_eq!(*mock_client.delete_count.borrow(), 0);
    }

    #[tokio::test]
    async fn test_pull_downloads_and_saves() {
        let (db, mock_client, temp_dir) = setup();

        let file_meta = make_file_meta("file0", 0);

        pull_single_file(&db, &mock_client, temp_dir.path(), &file_meta)
            .await
            .unwrap();

        assert_eq!(*mock_client.get_count.borrow(), 1);
    }

    #[tokio::test]
    async fn test_pull_skips_file() {
        let (db, mock_client, temp_dir) = setup();
        let file_meta = make_file_meta("file0", 2);
        let bytes: &[u8; 11] = b"hello world";
        let sync_record = SyncRecord {
            path: "file0".to_string(),
            local_hash: hash_bytes(bytes),
            server_version: 2,
        };
        db::put(&db, sync_record).unwrap();

        pull_single_file(&db, &mock_client, temp_dir.path(), &file_meta)
            .await
            .unwrap();

        assert_eq!(*mock_client.get_count.borrow(), 0);
    }

    #[tokio::test]
    async fn test_pull_downloads_server_update() {
        let (db, mock_client, temp_dir) = setup();
        let file0 = temp_dir.path().join("file0");
        let bytes: &[u8; 11] = b"hello world";
        std::fs::write(file0, bytes).unwrap();

        let file_meta = make_file_meta("file0", 2);

        let sync_record = SyncRecord {
            path: "file0".to_string(),
            local_hash: hash_bytes(bytes),
            server_version: 1,
        };
        db::put(&db, sync_record).unwrap();

        pull_single_file(&db, &mock_client, temp_dir.path(), &file_meta)
            .await
            .unwrap();

        assert_eq!(*mock_client.get_count.borrow(), 1);
    }

    #[tokio::test]
    async fn test_pull_records_conflict() {
        let (db, mock_client, temp_dir) = setup();
        let subdir = temp_dir.path().join("subdir");
        std::fs::create_dir_all(&subdir).unwrap();
        let file0 = subdir.join("file0");
        let bytes: &[u8; 11] = b"hello world";
        std::fs::write(file0, bytes).unwrap();

        let file_meta = make_file_meta("subdir/file0", 2);

        let sync_record = SyncRecord {
            path: "subdir/file0".to_string(),
            local_hash: "somethingelse".to_string(),
            server_version: 1,
        };
        db::put(&db, sync_record).unwrap();

        pull_single_file(&db, &mock_client, temp_dir.path(), &file_meta)
            .await
            .unwrap();

        assert_eq!(*mock_client.get_count.borrow(), 1);
        let conflict_exists = std::fs::read_dir(subdir).unwrap().into_iter().any(|f| {
            f.unwrap()
                .file_name()
                .into_string()
                .unwrap()
                .contains("file0.conflict")
        });
        assert!(conflict_exists);
    }

    fn make_file_meta(path: &str, version: u64) -> FileMeta {
        FileMeta {
            path: path.to_string(),
            size: 0,
            content_hash: "".to_string(),
            version,
            is_deleted: false,
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_pull_downloads_files() {
        let (db, mock_client, temp_dir) = setup();
        let file0 = temp_dir.path().join("file0");
        let bytes: &[u8; 11] = b"hello world";
        std::fs::write(file0, bytes).unwrap();

        let file_meta0 = make_file_meta("file0", 2);
        let file_meta1 = make_file_meta("file01", 2);
        mock_client.set_files(vec![file_meta0, file_meta1]);

        pull(&db, &mock_client, temp_dir.path()).await.unwrap();

        assert_eq!(*mock_client.list_count.borrow(), 1);
        assert_eq!(*mock_client.get_count.borrow(), 2)
    }

    #[tokio::test]
    async fn test_pull_continues_on_error() {
        let (db, mock_client, temp_dir) = setup();
        let file0 = temp_dir.path().join("file0");
        let bytes: &[u8; 11] = b"hello world";
        std::fs::write(file0, bytes).unwrap();

        let file_meta0 = make_file_meta("file0", 2);
        let file_meta1 = make_file_meta("file1", 2);
        mock_client.set_files(vec![file_meta0, file_meta1]);
        mock_client.set_fail_path("file1".to_string());

        pull(&db, &mock_client, temp_dir.path()).await.unwrap();

        assert_eq!(*mock_client.list_count.borrow(), 1);
        assert_eq!(*mock_client.get_count.borrow(), 1)
    }
}
