use std::io::Read;
use std::path::Path;

use cloudsync_common::{InitUploadRequest, hash_bytes, hash_file};
use redb::Database;

use crate::db;
use crate::scanner;
use crate::sync::SyncApi;
use crate::sync::SyncRecord;

pub const CHUNK_SIZE: u64 = 4 * 1024 * 1024;

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

async fn push_single_file_chunked(
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
async fn resume_upload(
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

async fn delete_file(sync_client: &impl SyncApi, db: &Database, path: &str) -> anyhow::Result<()> {
    sync_client.delete_file(path).await?;
    db::delete(db, path)?;
    println!("deleted (server): {}", path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{core::SyncRecord, mock_client::setup_test_deps};

    fn noop_progress() -> impl Fn(&str, u64, u64) -> Box<dyn Fn()> {
        |_: &str, _: u64, _: u64| -> Box<dyn Fn()> { Box::new(|| {}) }
    }

    #[tokio::test]
    async fn test_push_single_file() {
        let (db, mock_client, temp_dir) = setup_test_deps();
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
        let (db, mock_client, temp_dir) = setup_test_deps();
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
        let (db, mock_client, temp_dir) = setup_test_deps();
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
        let (db, mock_client, temp_dir) = setup_test_deps();
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
        let (db, mock_client, temp_dir) = setup_test_deps();
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
        let (db, mock_client, temp_dir) = setup_test_deps();
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
        let (db, mock_client, temp_dir) = setup_test_deps();
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
        let (db, mock_client, temp_dir) = setup_test_deps();
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
        let (db, mock_client, temp_dir) = setup_test_deps();
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
}
