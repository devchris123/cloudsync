use std::path::{Path};

use cloudsync_common::hash_bytes;
use redb::Database;
use serde::{Deserialize, Serialize};

use crate::{client, db};
use crate::scanner::scan_dir;

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

