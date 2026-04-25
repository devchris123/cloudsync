use std::path::Path;

use cloudsync_common::hash_file;
use redb::Database;

use crate::db;
use crate::scanner::{self, scan_dir};
use crate::sync::SyncApi;

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
