use std::path::Path;

use cloudsync_common::{FileMeta, hash_bytes};
use redb::Database;
use serde::{Deserialize, Serialize};

use crate::scanner::{self, scan_dir};
use crate::{client, db};

#[derive(Serialize, Deserialize)]
pub struct SyncRecord {
    pub path: String,
    pub local_hash: String,
    pub server_version: u64,
}

pub async fn push(
    db: &Database,
    sync_client: &client::SyncClient,
    sync_root: &Path,
) -> anyhow::Result<()> {
    let ignored = scanner::get_ignored(sync_root);
    let files = scanner::scan_dir(&sync_root, &ignored)?;

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
        if !sync_root.join(&sr.path).exists() {
            if let Err(e) = delete_file(sync_client, db, &sr.path).await {
                println!("error deleting {}: {}", &sr.path, e);
                continue;
            }
        }
    }
    Ok(())
}

async fn push_single_file(
    db: &Database,
    sync_client: &client::SyncClient,
    sync_root: &Path,
    file: &Path,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(&file)?;
    let hash = hash_bytes(&bytes);
    let path = file.strip_prefix(sync_root)?.to_str().unwrap().to_string();
    let sync_record = db::get(db, &path)?;
    if let Some(sr) = sync_record {
        if sr.local_hash == hash {
            return Ok(());
        }
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
    sync_client: &client::SyncClient,
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
    sync_client: &client::SyncClient,
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
    sync_client: &client::SyncClient,
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
                let local_content = std::fs::read(&local_path)?;
                let hash = hash_bytes(&local_content);
                if hash != record.local_hash {
                    println!(
                        "Conflict: {} changed locally and on server, skipping",
                        file.path
                    );
                    return Ok(());
                }
            }
        }
    }
    let content = sync_client.get_file(&file.path).await?;
    let path = &sync_root.join(&file.path);
    let parent_dir = std::path::Path::new(path).parent();
    if let Some(parent_dir) = parent_dir {
        std::fs::create_dir_all(&parent_dir)?;
    };
    std::fs::write(&sync_root.join(&file.path), &content)?;
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
    sync_client: &client::SyncClient,
    sync_root: &Path,
) -> anyhow::Result<()> {
    let ignored = &scanner::get_ignored(sync_root);
    let files = scan_dir(&sync_root, &ignored)?;

    let server_files = sync_client.list_files().await?.files;
    for file in files.iter() {
        let content = std::fs::read(&file)?;
        let path_str = file.strip_prefix(&sync_root)?.to_str().unwrap().to_string();
        let sync_record = db::get(db, &path_str)?;

        let Some(sync_record) = sync_record else {
            println!("{} - {}", &path_str, "new (local)");
            continue;
        };
        let server_hash = server_files.iter().find(|f| f.path == path_str);
        let hash = hash_bytes(&content);
        if hash != sync_record.local_hash {
            if let Some(sf) = server_hash {
                if sf.version > sync_record.server_version {
                    println!("{} - {}", &path_str, "conflict");
                    continue;
                }
            }
            println!("{} - {}", &path_str, "update (local)");
            continue;
        }
        if let Some(server_hash) = server_hash {
            if server_hash.content_hash != sync_record.local_hash {
                println!("{} - {}", &path_str, "update (server)");
                continue;
            }
        }
        println!("{} - {}", &path_str, "no update");
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
            println!("{} - {}", server_file.path, "new (server)")
        } else if !path.exists() {
            println!("{} - {}", &server_file.path, "deleted (local)");
        }
    }
    Ok(())
}
