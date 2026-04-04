use std::path::{Path, PathBuf};

use cloudsync_common::hash_bytes;
use redb::Database;
use serde::{Deserialize, Serialize};

use crate::{client, db};

use super::config;

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
    let files = scan_dir(&sync_root)?;

    for file in files.iter() {
        let bytes = std::fs::read(&file)?;
        let hash = hash_bytes(&bytes);
        let path = file.strip_prefix(sync_root)?.to_str().unwrap().to_string();
        let sync_record = db::get(db, &path)?;
        if let Some(sr) = sync_record {
            if sr.local_hash == hash {
                continue;
            }
        }
        let resp = sync_client.create_file(&path, bytes).await?;
        let sync_record = SyncRecord {
            path,
            local_hash: hash,
            server_version: resp.file.version,
        };
        db::put(db, sync_record)?;
    }

    let sync_records = db::list(db)?;
    for sr in sync_records {
        if !sync_root.join(&sr.path).exists() {
            sync_client.delete_file(&sr.path).await?;
            db::delete(db, &sr.path)?;
        }
    }
    Ok(())
}

pub fn scan_dir(sync_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let dirs_iter = std::fs::read_dir(sync_root)?;

    let mut changed_files: Vec<PathBuf> = Vec::new();
    for dir in dirs_iter {
        let dir = dir?;
        if dir.file_name() == config::CONFIG_DIR {
            continue;
        }
        if dir.file_type()?.is_dir() {
            let mut sub_files = scan_dir(&dir.path())?;
            changed_files.append(&mut sub_files);
            continue;
        }
        changed_files.push(dir.path());
    }
    Ok(changed_files)
}
