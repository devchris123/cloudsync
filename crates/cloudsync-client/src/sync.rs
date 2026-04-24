use std::io::Read;
use std::path::{Path, PathBuf};

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

pub struct DownloadFileResponse {
    pub resumed: bool,
    pub stream: Box<dyn AsyncRead + Unpin + Send>,
}

#[allow(async_fn_in_trait)]
pub trait SyncApi {
    async fn list_files(&self) -> anyhow::Result<ListFilesResponse>;
    async fn create_file(&self, path: &str, content: Vec<u8>)
    -> anyhow::Result<CreateFileResponse>;
    async fn get_file(&self, path: &str, start_bytes: u64) -> anyhow::Result<DownloadFileResponse>;
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
    pub upload_id: Option<String>,
}

pub async fn push(
    db: &Database,
    sync_client: &impl SyncApi,
    sync_root: &Path,
    on_file_start: &impl Fn(&str, u64, u64) -> Box<dyn Fn()>,
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
            push_single_file_chunked(db, sync_client, sync_root, local_path, on_file_start).await
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
        upload_id: None,
    };
    db::put(db, &sync_record)?;
    println!("pushed: {}", &rel_path);
    Ok(())
}

pub async fn push_single_file_chunked(
    db: &Database,
    sync_client: &impl SyncApi,
    sync_root: &Path,
    local_path: &Path,
    on_file_start: &impl Fn(&str, u64, u64) -> Box<dyn Fn()>,
) -> anyhow::Result<()> {
    let total_size = local_path.metadata()?.len();
    let chunk_count = total_size.div_ceil(CHUNK_SIZE);
    let hash = hash_file(local_path)?;
    let rel_path = local_path
        .strip_prefix(sync_root)?
        .to_str()
        .unwrap()
        .to_string();
    let mut sync_record = db::get(db, &rel_path)?;
    if let Some(sr) = &sync_record
        && sr.local_hash == hash
    {
        return Ok(());
    }
    let mut upload_id: Option<String> = None;
    let mut chunks_received: Vec<u32> = vec![];
    let mut chunk_count = chunk_count;

    if let Some(sr) = &mut sync_record
        && let Some(id) = sr.upload_id.as_deref()
    {
        let result = sync_client.get_upload(id).await;
        if let Ok(res) = result {
            upload_id = Some(res.upload.upload_id);
            chunks_received = res.upload.chunks_received;
            chunk_count = res.upload.chunk_count;
        }
    }
    if upload_id.is_none() {
        let upload = sync_client
            .init_upload(InitUploadRequest {
                path: rel_path.clone(),
                total_size,
                total_hash: hash.clone(),
                chunk_count,
            })
            .await?;
        upload_id = Some(upload.upload_id);
    }

    resume_upload(
        &upload_id.unwrap(),
        db,
        sync_client,
        sync_record.as_mut(),
        local_path,
        chunks_received,
        chunk_count,
        rel_path,
        hash,
        on_file_start,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn resume_upload(
    upload_id: &str,
    db: &Database,
    sync_client: &impl SyncApi,
    mut sync_record: Option<&mut SyncRecord>,
    local_path: &Path,
    chunks_received: Vec<u32>,
    chunk_count: u64,
    rel_path: String,
    hash: String,
    on_file_start: impl Fn(&str, u64, u64) -> Box<dyn Fn()>,
) -> anyhow::Result<()> {
    if let Some(sr) = &mut sync_record {
        sr.upload_id = Some(upload_id.to_string());
        db::put(db, sr)?;
    } else {
        let sr = SyncRecord {
            path: rel_path.clone(),
            local_hash: hash.clone(),
            server_version: 0,
            upload_id: Some(upload_id.to_string()),
        };
        db::put(db, &sr)?;
    }

    let mut file = std::fs::File::open(local_path)?;
    let batch_size: usize = std::env::var("CLOUDSYNC_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let mut join_set = vec![];

    let chunks_received: std::collections::HashSet<u32> = chunks_received.into_iter().collect();
    let on_progress = on_file_start(&rel_path, chunk_count, chunks_received.len() as u64);
    for idx in 0..chunk_count {
        let mut buf = Vec::new();
        (&mut file).take(CHUNK_SIZE).read_to_end(&mut buf)?;
        // Do the skip after reading file, so the file pointer correctly advances for the next read.
        if chunks_received.contains(&(idx as u32)) {
            continue;
        }
        let fut = sync_client.send_chunk(upload_id, idx as u32, buf);
        join_set.push(fut);
        if join_set.len() == batch_size {
            let results = futures::future::join_all(join_set.drain(..)).await;
            for res in results {
                res?;
                on_progress();
            }
        }
    }
    // Final flush in case we skipped the last chunk before the loop ends
    if !join_set.is_empty() {
        let results = futures::future::join_all(join_set.drain(..)).await;
        for res in results {
            res?;
        }
    }

    let resp = sync_client.finalize_upload(upload_id).await?;
    let sync_record = SyncRecord {
        path: rel_path.clone(),
        local_hash: hash,
        server_version: resp.file.version,
        upload_id: None,
    };
    db::put(db, &sync_record)?;
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

                    let mut conflict_path_backup = conflict_path.as_os_str().to_owned();
                    conflict_path_backup.push(".part");
                    let conflict_path_backup = PathBuf::from(conflict_path_backup);

                    download_to_file(sync_client, &file_meta.path, &conflict_path_backup).await?;

                    let conflict_backup_hash = hash_file(&conflict_path_backup)?;
                    if file_meta.content_hash != conflict_backup_hash {
                        std::fs::remove_file(conflict_path_backup)?;
                        anyhow::bail!(
                            "Corrupt download: hash does not match for {}",
                            &file_meta.path
                        );
                    }
                    std::fs::rename(conflict_path_backup, &conflict_path)?;
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
    let mut local_path_backup = local_path.as_os_str().to_owned();
    local_path_backup.push(".part");
    let local_path_backup = PathBuf::from(local_path_backup);
    download_to_file(sync_client, &file_meta.path, &local_path_backup).await?;
    let local_backup_hash = hash_file(&local_path_backup)?;
    if file_meta.content_hash != local_backup_hash {
        std::fs::remove_file(local_path_backup)?;
        anyhow::bail!(
            "Corrupt download: hash does not match for {}",
            &file_meta.path
        );
    }
    std::fs::rename(local_path_backup, local_path)?;
    let sync_record = SyncRecord {
        path: file_meta.path.clone(),
        local_hash: local_backup_hash,
        server_version: file_meta.version,
        upload_id: None,
    };
    db::put(db, &sync_record)?;
    println!("pulled: {}", &file_meta.path);

    Ok(())
}

pub async fn download_to_file(
    sync_client: &impl SyncApi,
    src_path: &str,
    dest: &Path,
) -> anyhow::Result<()> {
    let bytes_on_file = tokio::fs::metadata(&dest)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let mut resp = sync_client.get_file(src_path, bytes_on_file).await?;
    let mut file_opts = tokio::fs::OpenOptions::new();
    file_opts.create(true);
    let mut file = if resp.resumed {
        file_opts.append(true).open(&dest).await?
    } else {
        file_opts.truncate(true).write(true).open(&dest).await?
    };
    tokio::io::copy(&mut resp.stream, &mut file).await?;
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
        send_chunk_fails: RefCell<bool>,
        get_upload_count: RefCell<u64>,
        get_upload_chunks_received: RefCell<Vec<u32>>,
        get_upload_chunk_count: RefCell<u64>,
        get_upload_fail_id: RefCell<Option<String>>,
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
                send_chunk_fails: RefCell::new(false),
                get_upload_count: RefCell::new(0),
                get_upload_chunks_received: RefCell::new(vec![]),
                get_upload_chunk_count: RefCell::new(0),
                get_upload_fail_id: RefCell::new(None),
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

        fn set_send_chunk_fails(&self, fails: bool) {
            *self.send_chunk_fails.borrow_mut() = fails;
        }

        fn set_upload_chunks_received(&self, chunks: Vec<u32>) {
            *self.get_upload_chunks_received.borrow_mut() = chunks;
        }

        fn set_upload_chunk_count(&self, chunk_count: u64) {
            *self.get_upload_chunk_count.borrow_mut() = chunk_count;
        }

        fn set_upload_fail_id(&self, upload_id: String) {
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

    fn noop_progress() -> impl Fn(&str, u64, u64) -> Box<dyn Fn()> {
        |_: &str, _: u64, _: u64| -> Box<dyn Fn()> { Box::new(|| {}) }
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

        push_single_file_chunked(&db, &mock_client, temp_dir.path(), &file, &noop_progress())
            .await
            .unwrap();

        let record = db::get(&db, "file0").unwrap();
        assert!(record.is_some());
        assert!(record.unwrap().upload_id.is_none());
        assert_eq!(*mock_client.init_upload_count.borrow(), 1);
        assert_eq!(*mock_client.send_chunk_count.borrow(), 3);
        assert_eq!(*mock_client.finalize_upload_count.borrow(), 1);
    }

    #[tokio::test]
    async fn test_push_single_file_chunked_resumes() {
        let (db, mock_client, temp_dir) = setup();
        let file = temp_dir.path().join("file0");
        let bytes = b"hello world";
        std::fs::write(&file, &bytes).unwrap();
        let sync_record = SyncRecord {
            path: "file0".to_string(),
            local_hash: "something".to_string(),
            server_version: 0,
            upload_id: Some("test_upload".to_string()),
        };
        db::put(&db, &sync_record).unwrap();
        mock_client.set_upload_chunks_received(vec![1, 3, 4]);
        mock_client.set_upload_chunk_count(5);

        push_single_file_chunked(&db, &mock_client, temp_dir.path(), &file, &noop_progress())
            .await
            .unwrap();

        let record = db::get(&db, "file0").unwrap();
        assert!(record.is_some());
        assert!(record.unwrap().upload_id.is_none());
        assert_eq!(*mock_client.init_upload_count.borrow(), 0);
        assert_eq!(*mock_client.send_chunk_count.borrow(), 2);
        assert_eq!(*mock_client.finalize_upload_count.borrow(), 1);
    }

    #[tokio::test]
    async fn test_push_single_file_chunked_expired() {
        let (db, mock_client, temp_dir) = setup();
        let file = temp_dir.path().join("file0");
        let bytes = b"hello world";
        std::fs::write(&file, &bytes).unwrap();
        let sync_record = SyncRecord {
            path: "file0".to_string(),
            local_hash: "something".to_string(),
            server_version: 0,
            upload_id: Some("test_upload".to_string()),
        };
        db::put(&db, &sync_record).unwrap();
        mock_client.set_upload_fail_id("test_upload".to_string());

        push_single_file_chunked(&db, &mock_client, temp_dir.path(), &file, &noop_progress())
            .await
            .unwrap();

        let record = db::get(&db, "file0").unwrap();
        assert!(record.is_some());
        assert!(record.unwrap().upload_id.is_none());
        assert_eq!(*mock_client.init_upload_count.borrow(), 1);
        assert_eq!(*mock_client.send_chunk_count.borrow(), 1);
        assert_eq!(*mock_client.finalize_upload_count.borrow(), 1);
    }

    #[tokio::test]
    async fn test_push_single_file_chunked_crashes() {
        let (db, mock_client, temp_dir) = setup();
        let file = temp_dir.path().join("file0");
        let bytes = b"hello world";
        std::fs::write(&file, &bytes).unwrap();
        mock_client.set_send_chunk_fails(true);

        let result =
            push_single_file_chunked(&db, &mock_client, temp_dir.path(), &file, &noop_progress())
                .await;

        let record = db::get(&db, "file0").unwrap();
        assert!(result.is_err());
        assert!(record.is_some());
        assert!(record.unwrap().upload_id.is_some());
        assert_eq!(*mock_client.init_upload_count.borrow(), 1);
        assert_eq!(*mock_client.send_chunk_count.borrow(), 1);
        assert_eq!(*mock_client.finalize_upload_count.borrow(), 0);
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
            upload_id: None,
        };
        db::put(&db, &sync_record).unwrap();

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

        push(&db, &mock_client, temp_dir.path(), &noop_progress())
            .await
            .unwrap();

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
            upload_id: None,
        };
        db::put(&db, &sync_record).unwrap();

        push(&db, &mock_client, temp_dir.path(), &noop_progress())
            .await
            .unwrap();

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
            upload_id: None,
        };
        db::put(&db, &sync_record).unwrap();

        push(&db, &mock_client, temp_dir.path(), &noop_progress())
            .await
            .unwrap();

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
            upload_id: None,
        };
        db::put(&db, &sync_record).unwrap();

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
            upload_id: None,
        };
        db::put(&db, &sync_record).unwrap();

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
            upload_id: None,
        };
        db::put(&db, &sync_record).unwrap();

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
            content_hash: hash_bytes(&[]),
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
