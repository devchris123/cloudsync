use redb::{Database, ReadableTable, TableDefinition};

use crate::sync::SyncRecord;

const DB_NAME: &str = "sync.redb";

const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("sync_records");

pub fn init_db() -> anyhow::Result<()> {
    let db = Database::create(DB_NAME)?;
    let tx = db.begin_write()?;
    tx.commit()?;
    Ok(())
}

pub fn list(db: &Database) -> anyhow::Result<Vec<SyncRecord>> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE)?;

    let mut records: Vec<SyncRecord> = Vec::new();
    for entry in table.iter()? {
        let (_, val) = entry?;
        let val = val.value();
        let record = serde_json::from_slice::<SyncRecord>(val)?;
        records.push(record);
    }

    Ok(records)
}

pub fn put(db: &Database, sync_record: SyncRecord) -> anyhow::Result<()> {
    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(TABLE)?;
        let bytes = serde_json::to_vec(&sync_record)?;
        table.insert(sync_record.path.as_str(), bytes.as_slice())?;
    }
    tx.commit()?;
    Ok(())
}

pub fn get(db: &Database, path: &str) -> anyhow::Result<Option<SyncRecord>> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE)?;
    let raw = table.get(path)?;
    let Some(entry) = raw else {
        return Ok(None);
    };
    let bytes = entry.value();
    let sync_record = serde_json::from_slice::<SyncRecord>(bytes)?;
    Ok(Some(sync_record))
}

pub fn delete(db: &Database, path: &str) -> anyhow::Result<()> {
    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(TABLE)?;
        table.remove(path)?;
    }
    tx.commit()?;
    Ok(())
}
