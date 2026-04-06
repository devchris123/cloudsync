use std::path::Path;

use cloudsync_common::{
    CreateFileResponse, DeleteFileResponse, FileMeta, ListFilesResponse, hash_bytes, hash_file,
};
use redb::Database;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::scanner::{self, scan_dir};

#[allow(async_fn_in_trait)]
pub trait SyncApi {
    async fn list_files(&self) -> anyhow::Result<ListFilesResponse>;
    async fn create_file(&self, path: &str, content: Vec<u8>)
    -> anyhow::Result<CreateFileResponse>;
    async fn get_file(&self, path: &str) -> anyhow::Result<Vec<u8>>;
    async fn delete_file(&self, path: &str) -> anyhow::Result<DeleteFileResponse>;
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
    let files = scanner::scan_dir(sync_root, &ignored)?;

    for file in files.iter() {
        if let Err(e) = push_single_file(db, sync_client, sync_root, file).await {
            println!(
                "error pushing {}: {}",
                &file.to_str().unwrap().to_string(),
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
    file: &Path,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(file)?;
    let hash = hash_bytes(&bytes);
    let path = file.strip_prefix(sync_root)?.to_str().unwrap().to_string();
    let sync_record = db::get(db, &path)?;
    if let Some(sr) = sync_record
        && sr.local_hash == hash
    {
        return Ok(());
    }
    let resp = sync_client.create_file(&path, bytes).await?;
    let sync_record = SyncRecord {
        path: path.clone(),
        local_hash: hash,
        server_version: resp.file.version,
    };
    db::put(db, sync_record)?;
    println!("pushed: {}", &path);
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
    let files = sync_client.list_files().await?;
    for file in files.files {
        if let Err(e) = pull_single_file(db, sync_client, sync_root, &file).await {
            println!("error pulling {}: {}", &file.path, e);
            continue;
        }
    }
    Ok(())
}

async fn pull_single_file(
    db: &Database,
    sync_client: &impl SyncApi,
    sync_root: &Path,
    file: &FileMeta,
) -> anyhow::Result<()> {
    let record = db::get(db, &file.path)?;

    if let Some(record) = record {
        if record.server_version == file.version {
            return Ok(());
        }
        if record.server_version < file.version {
            let local_path = &sync_root.join(&file.path);
            if local_path.exists() {
                let hash = hash_file(local_path)?;
                if hash != record.local_hash {
                    println!("Conflict: {} changed locally and on server", file.path);
                    let server_content = sync_client.get_file(&file.path).await?;
                    let path = std::path::Path::new(&file.path);
                    let stem = path.file_stem().unwrap_or_default().to_str().unwrap();
                    let ext = path
                        .extension()
                        .map(|e| format!(".{}", e.to_str().unwrap()))
                        .unwrap_or_default();
                    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
                    let conflict_path = format!("{}.conflict.{}{}", stem, timestamp, ext);
                    let conflict_path = sync_root.join(&file.path).with_file_name(conflict_path);
                    std::fs::write(&conflict_path, server_content)?;
                    println!("Conflict: resolve conflict in {}", conflict_path.display());
                    return Ok(());
                }
            }
        }
    }
    let content = sync_client.get_file(&file.path).await?;
    let path = &sync_root.join(&file.path);
    let parent_dir = std::path::Path::new(path).parent();
    if let Some(parent_dir) = parent_dir {
        std::fs::create_dir_all(parent_dir)?;
    };
    std::fs::write(sync_root.join(&file.path), &content)?;
    let sync_record = SyncRecord {
        path: file.path.clone(),
        local_hash: hash_bytes(&content),
        server_version: file.version,
    };
    db::put(db, sync_record)?;
    println!("pulled: {}", &file.path);

    Ok(())
}

pub async fn status(
    db: &Database,
    sync_client: &impl SyncApi,
    sync_root: &Path,
) -> anyhow::Result<()> {
    let ignored = &scanner::get_ignored(sync_root);
    let files = scan_dir(sync_root, ignored)?;

    let server_files = sync_client.list_files().await?.files;
    for file in files.iter() {
        let path_str = file.strip_prefix(sync_root)?.to_str().unwrap().to_string();
        let sync_record = db::get(db, &path_str)?;

        let Some(sync_record) = sync_record else {
            println!("{} - new (local)", &path_str);
            continue;
        };
        let server_hash = server_files.iter().find(|f| f.path == path_str);
        let hash = hash_file(file)?;
        if hash != sync_record.local_hash {
            if let Some(sf) = server_hash
                && sf.version > sync_record.server_version
            {
                println!("{} - conflict", &path_str);
                continue;
            }
            println!("{} - update (local)", &path_str);
            continue;
        }
        if let Some(server_hash) = server_hash
            && server_hash.content_hash != sync_record.local_hash
        {
            println!("{} - update (server)", &path_str);
            continue;
        }
        println!("{} - no update", &path_str);
    }

    for server_file in server_files {
        if files.iter().any(|f| {
            let path_str = f.strip_prefix(sync_root).ok().and_then(|f| f.to_str());
            path_str == Some(&server_file.path)
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

    use tempfile::TempDir;

    use crate::db::open_db;

    use super::*;

    struct MockClient {
        files: RefCell<Vec<FileMeta>>,
        list_count: RefCell<u64>,
        create_count: RefCell<u64>,
        get_count: RefCell<u64>,
        delete_count: RefCell<u64>,
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

        async fn get_file(&self, path: &str) -> anyhow::Result<Vec<u8>> {
            if self.fail_path.borrow().as_deref() == Some(path) {
                anyhow::bail!("error");
            }
            *self.get_count.borrow_mut() += 1;
            Ok(Vec::new())
        }

        async fn delete_file(&self, _path: &str) -> anyhow::Result<DeleteFileResponse> {
            *self.delete_count.borrow_mut() += 1;
            Ok(DeleteFileResponse {})
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
