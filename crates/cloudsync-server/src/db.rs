use cloudsync_common::FileMeta;
use redb::{Database, ReadableTable, TableDefinition};

pub const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("files");

pub fn list(db: &Database) -> Result<Vec<FileMeta>, anyhow::Error> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE)?;

    let mut file_metas: Vec<FileMeta> = Vec::new();
    for entry in table.iter()? {
        let (_, val) = entry?;
        let bytes = val.value();
        let file_meta = serde_json::from_slice::<FileMeta>(bytes)?;
        file_metas.push(file_meta);
    }

    Ok(file_metas)
}

pub fn get(db: &Database, path: &str) -> Result<Option<FileMeta>, anyhow::Error> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE)?;
    let entry = table.get(path)?;
    let Some(entry) = entry else {
        return Ok(None);
    };
    let bytes = entry.value();
    let file_meta = serde_json::from_slice::<FileMeta>(bytes)?;
    Ok(Some(file_meta))
}

pub fn put(db: &Database, path: &str, size: u64, content_hash: String) -> anyhow::Result<FileMeta> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE)?;
    let entry = table.get(path)?;

    let mut file_meta = FileMeta {
        path: path.to_string(),
        size,
        content_hash,
        version: 1,
        is_deleted: false,
        created_at: chrono::Utc::now(),
        modified_at: chrono::Utc::now(),
    };

    if let Some(entry) = entry {
        let bytes = entry.value();
        let file_meta_raw = serde_json::from_slice::<FileMeta>(bytes)?;
        file_meta.version = file_meta_raw.version + 1;
        file_meta.created_at = file_meta_raw.created_at;
    }
    drop(table);

    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(TABLE)?;
        let bytes = serde_json::to_vec(&file_meta)?;
        table.insert(path, bytes.as_slice())?;
    }
    tx.commit()?;
    Ok(file_meta)
}

pub fn delete(db: &Database, path: &str) -> anyhow::Result<()> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE)?;
    let entry = table.get(path)?;
    let Some(entry) = entry else { return Ok(()) };
    let bytes = entry.value();
    let mut file_meta = serde_json::from_slice::<FileMeta>(bytes)?;
    file_meta.is_deleted = true;
    drop(table);

    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(TABLE)?;
        let bytes = serde_json::to_vec(&file_meta)?;
        table.insert(path, bytes.as_slice())?;
    }
    tx.commit()?;
    Ok(())
}
