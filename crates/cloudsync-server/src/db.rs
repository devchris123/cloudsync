use cloudsync_common::FileMeta;
use redb::{Database, ReadableTable, TableDefinition};

use crate::db_upload::TABLE_UPLOADS;

pub const TABLE_FILES: TableDefinition<&str, &[u8]> = TableDefinition::new("files");

pub fn open_db(db_path: &str) -> anyhow::Result<Database> {
    let db: Database = Database::create(db_path)?;
    let tx = db.begin_write()?;
    {
        tx.open_table(TABLE_FILES)?;
    }
    tx.commit()?;
    let tx = db.begin_write()?;
    {
        tx.open_table(TABLE_UPLOADS)?;
    }
    tx.commit()?;
    Ok(db)
}

pub fn list(db: &Database) -> Result<Vec<FileMeta>, anyhow::Error> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE_FILES)?;

    let mut file_metas: Vec<FileMeta> = Vec::new();
    for entry in table.iter()? {
        let (_, val) = entry?;
        let bytes = val.value();
        let file_meta = serde_json::from_slice::<FileMeta>(bytes)?;
        if file_meta.is_deleted {
            continue;
        }
        file_metas.push(file_meta);
    }

    Ok(file_metas)
}

pub fn get(db: &Database, path: &str) -> Result<Option<FileMeta>, anyhow::Error> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE_FILES)?;
    let entry = table.get(path)?;
    let Some(entry) = entry else {
        return Ok(None);
    };
    let bytes = entry.value();
    let file_meta = serde_json::from_slice::<FileMeta>(bytes)?;
    Ok(Some(file_meta))
}

pub fn put(db: &Database, path: &str, size: u64, content_hash: &str) -> anyhow::Result<FileMeta> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE_FILES)?;
    let entry = table.get(path)?;

    let mut file_meta = FileMeta {
        path: path.to_string(),
        size,
        content_hash: content_hash.to_string(),
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
        let mut table = tx.open_table(TABLE_FILES)?;
        let bytes = serde_json::to_vec(&file_meta)?;
        table.insert(path, bytes.as_slice())?;
    }
    tx.commit()?;
    Ok(file_meta)
}

pub fn delete(db: &Database, path: &str) -> anyhow::Result<()> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE_FILES)?;
    let entry = table.get(path)?;
    let Some(entry) = entry else { return Ok(()) };
    let bytes = entry.value();
    let mut file_meta = serde_json::from_slice::<FileMeta>(bytes)?;
    file_meta.is_deleted = true;
    drop(table);

    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(TABLE_FILES)?;
        let bytes = serde_json::to_vec(&file_meta)?;
        table.insert(path, bytes.as_slice())?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    use cloudsync_common::hash_bytes;
    use tempfile::TempDir;

    fn test_db() -> (TempDir, Database) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.redb");
        let db = redb::Database::create(&path).unwrap();
        let tx = db.begin_write().unwrap();
        tx.open_table(TABLE_FILES).unwrap();
        tx.commit().unwrap();
        (dir, db)
    }

    #[test]
    fn test_full_lifecycle() {
        let (_dir, db) = test_db();

        let path = "somepath/test.txt";
        let bytes = b"hello world";
        let hash = hash_bytes(bytes);
        let size = bytes.len() as u64;
        let file_meta = put(&db, path, size, &hash).unwrap();

        assert_eq!(file_meta.path, path);
        assert_eq!(file_meta.content_hash, hash);
        assert_eq!(file_meta.size, size);
        assert_eq!(file_meta.version, 1);
        assert_eq!(file_meta.is_deleted, false);

        let file_meta = get(&db, path).unwrap().unwrap();
        assert_eq!(file_meta.path, path);
        assert_eq!(file_meta.content_hash, hash);
        assert_eq!(file_meta.size, size);
        assert_eq!(file_meta.version, 1);
        assert_eq!(file_meta.is_deleted, false);

        let file_meta = put(&db, path, size, &hash).unwrap();
        assert_eq!(file_meta.version, 2);

        let path = "somepath/test2.txt";
        let bytes = b"hello world";
        let hash = hash_bytes(bytes);
        let size = bytes.len() as u64;
        put(&db, path, size, &hash).unwrap();
        let file_metas = list(&db).unwrap();
        assert_eq!(file_metas.len(), 2);

        delete(&db, path).unwrap();

        let file_metas = list(&db).unwrap();
        assert_eq!(file_metas.len(), 1);
    }

    #[test]
    fn test_get_not_exist() {
        let (_dir, db) = test_db();

        let file_meta = get(&db, "notexist").unwrap();

        assert!(file_meta.is_none());
    }
}
