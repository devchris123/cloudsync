use redb::{Database, TableDefinition};
use serde::{Deserialize, Serialize};

pub const TABLE_META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

const META_KEY: &str = "SCHEMA_VERSION";

#[derive(Serialize, Deserialize, Clone)]
pub struct Meta {
    pub schema_version: u32,
}

pub fn get(db: &Database) -> Result<Meta, anyhow::Error> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE_META)?;
    let entry = table.get(META_KEY)?;
    let Some(entry) = entry else {
        return Ok(Meta { schema_version: 0 });
    };
    let bytes = entry.value();
    let meta = serde_json::from_slice::<Meta>(bytes)?;
    Ok(meta)
}

pub fn inc(db: &Database) -> anyhow::Result<()> {
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE_META)?;
    let entry = table.get(META_KEY)?;
    let mut meta = match entry {
        Some(entry) => serde_json::from_slice::<Meta>(entry.value())?,
        None => Meta { schema_version: 0 },
    };

    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(TABLE_META)?;
        meta.schema_version += 1;
        let bytes = serde_json::to_vec(&meta)?;
        table.insert(META_KEY, bytes.as_slice())?;
    }
    tx.commit()?;

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    use tempfile::TempDir;

    fn test_db() -> (TempDir, Database) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.redb");
        let db = redb::Database::create(&path).unwrap();
        let tx = db.begin_write().unwrap();
        tx.open_table(TABLE_META).unwrap();
        tx.commit().unwrap();
        (dir, db)
    }

    #[test]
    fn test_full_lifecycle() {
        let (_dir, db) = test_db();

        let meta = get(&db).unwrap();
        assert_eq!(0, meta.schema_version);

        inc(&db).unwrap();
        inc(&db).unwrap();
        let meta = get(&db).unwrap();
        assert_eq!(2, meta.schema_version);
    }
}
