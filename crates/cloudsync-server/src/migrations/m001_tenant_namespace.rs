use cloudsync_common::FileMeta;
use redb::{Database, ReadableTable};

use crate::db::TABLE_FILES;

use super::meta;

pub fn migrate(
    db: &Database,
    default_tenant_id: &str,
    default_user_id: &str,
) -> anyhow::Result<()> {
    let meta = meta::get(db)?;
    if meta.schema_version >= 1 {
        return Ok(());
    }

    // Read all file_metas
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE_FILES)?;
    let mut file_metas: Vec<FileMeta> = Vec::new();
    for entry in table.iter()? {
        let (key, val) = entry?;
        let key = key.value().to_string();
        if key.contains("\0") {
            continue;
        }
        let bytes = val.value().to_vec();
        let file_meta = serde_json::from_slice::<FileMeta>(bytes.as_slice())?;
        file_metas.push(file_meta);
    }
    drop(tx);

    // Transform all file_metas
    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(TABLE_FILES)?;
        for mut file_meta in file_metas {
            let new_key = format!("{}\0{}", default_tenant_id, file_meta.path);
            file_meta.tenant_id = default_tenant_id.to_string();
            file_meta.user_id = default_user_id.to_string();
            let bytes = serde_json::to_vec(&file_meta)?;
            table.insert(new_key.as_str(), bytes.as_slice())?;
            table.remove(file_meta.path.as_str())?;
        }
    }
    tx.commit()?;

    meta::inc(db)?;

    Ok(())
}
