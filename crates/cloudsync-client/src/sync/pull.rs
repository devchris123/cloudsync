use std::path::{Path, PathBuf};

use cloudsync_common::{FileMeta, hash_file};
use redb::Database;

use crate::db;
use crate::sync::core::ProgressReader;
use crate::sync::{SyncApi, SyncRecord};

pub async fn pull(
    db: &Database,
    sync_client: &impl SyncApi,
    sync_root: &Path,
    on_file_start: &impl Fn(&str, u64, u64) -> Box<dyn Fn(u64)>,
) -> anyhow::Result<()> {
    let file_metas = sync_client.list_files().await?.files;
    for file_meta in file_metas {
        if let Err(e) =
            pull_single_file(db, sync_client, sync_root, &file_meta, on_file_start).await
        {
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
    on_file_start: &impl Fn(&str, u64, u64) -> Box<dyn Fn(u64)>,
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

                    let bytes_on_file = tokio::fs::metadata(&conflict_path_backup)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0);
                    let on_bytes = on_file_start(&file_meta.path, file_meta.size, bytes_on_file);
                    download_to_file(
                        sync_client,
                        &file_meta.path,
                        &conflict_path_backup,
                        on_bytes,
                    )
                    .await?;
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

    let bytes_on_file = tokio::fs::metadata(&local_path_backup)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let on_bytes = on_file_start(&file_meta.path, file_meta.size, bytes_on_file);
    download_to_file(sync_client, &file_meta.path, &local_path_backup, on_bytes).await?;
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

async fn download_to_file(
    sync_client: &impl SyncApi,
    src_path: &str,
    dest: &Path,
    on_bytes: Box<dyn Fn(u64)>,
) -> anyhow::Result<()> {
    let bytes_on_file = tokio::fs::metadata(&dest)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let resp = sync_client.get_file(src_path, bytes_on_file).await?;
    let mut file_opts = tokio::fs::OpenOptions::new();
    file_opts.create(true);
    let mut file = if resp.resumed {
        file_opts.append(true).open(&dest).await?
    } else {
        file_opts.truncate(true).write(true).open(&dest).await?
    };
    let mut progress_streamer = ProgressReader {
        inner: resp.stream,
        on_bytes,
    };
    tokio::io::copy(&mut progress_streamer, &mut file).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{SyncRecord, mock_client::setup_test_deps};
    use cloudsync_common::hash_bytes;

    fn noop_download_progress() -> impl Fn(&str, u64, u64) -> Box<dyn Fn(u64)> {
        |_: &str, _: u64, _: u64| -> Box<dyn Fn(u64)> { Box::new(|_| {}) }
    }

    #[tokio::test]
    async fn test_pull_downloads_and_saves() {
        let (db, mock_client, temp_dir) = setup_test_deps();

        let file_meta = make_file_meta("file0", 0);

        pull_single_file(
            &db,
            &mock_client,
            temp_dir.path(),
            &file_meta,
            &noop_download_progress(),
        )
        .await
        .unwrap();

        assert_eq!(*mock_client.get_count.borrow(), 1);
    }

    #[tokio::test]
    async fn test_pull_skips_file() {
        let (db, mock_client, temp_dir) = setup_test_deps();
        let file_meta = make_file_meta("file0", 2);
        let bytes: &[u8; 11] = b"hello world";
        let sync_record = SyncRecord {
            path: "file0".to_string(),
            local_hash: hash_bytes(bytes),
            server_version: 2,
            upload_id: None,
        };
        db::put(&db, &sync_record).unwrap();

        pull_single_file(
            &db,
            &mock_client,
            temp_dir.path(),
            &file_meta,
            &noop_download_progress(),
        )
        .await
        .unwrap();

        assert_eq!(*mock_client.get_count.borrow(), 0);
    }

    #[tokio::test]
    async fn test_pull_downloads_server_update() {
        let (db, mock_client, temp_dir) = setup_test_deps();
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

        pull_single_file(
            &db,
            &mock_client,
            temp_dir.path(),
            &file_meta,
            &noop_download_progress(),
        )
        .await
        .unwrap();

        assert_eq!(*mock_client.get_count.borrow(), 1);
    }

    #[tokio::test]
    async fn test_pull_records_conflict() {
        let (db, mock_client, temp_dir) = setup_test_deps();
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

        pull_single_file(
            &db,
            &mock_client,
            temp_dir.path(),
            &file_meta,
            &noop_download_progress(),
        )
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
        let (db, mock_client, temp_dir) = setup_test_deps();
        let file0 = temp_dir.path().join("file0");
        let bytes: &[u8; 11] = b"hello world";
        std::fs::write(file0, bytes).unwrap();

        let file_meta0 = make_file_meta("file0", 2);
        let file_meta1 = make_file_meta("file01", 2);
        mock_client.set_files(vec![file_meta0, file_meta1]);

        pull(
            &db,
            &mock_client,
            temp_dir.path(),
            &noop_download_progress(),
        )
        .await
        .unwrap();

        assert_eq!(*mock_client.list_count.borrow(), 1);
        assert_eq!(*mock_client.get_count.borrow(), 2)
    }

    #[tokio::test]
    async fn test_pull_continues_on_error() {
        let (db, mock_client, temp_dir) = setup_test_deps();
        let file0 = temp_dir.path().join("file0");
        let bytes: &[u8; 11] = b"hello world";
        std::fs::write(file0, bytes).unwrap();

        let file_meta0 = make_file_meta("file0", 2);
        let file_meta1 = make_file_meta("file1", 2);
        mock_client.set_files(vec![file_meta0, file_meta1]);
        mock_client.set_fail_path("file1".to_string());

        pull(
            &db,
            &mock_client,
            temp_dir.path(),
            &noop_download_progress(),
        )
        .await
        .unwrap();

        assert_eq!(*mock_client.list_count.borrow(), 1);
        assert_eq!(*mock_client.get_count.borrow(), 1)
    }
}
