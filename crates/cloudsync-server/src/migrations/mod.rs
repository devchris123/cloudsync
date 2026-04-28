use redb::Database;

use crate::migrations::meta::TABLE_META;

mod m001_tenant_namespace;
mod meta;

pub fn run_migrations(
    db: &Database,
    default_tenant_id: &str,
    default_user_id: &str,
) -> anyhow::Result<()> {
    let tx = db.begin_write()?;
    {
        tx.open_table(TABLE_META)?;
    }
    tx.commit()?;
    m001_tenant_namespace::migrate(db, default_tenant_id, default_user_id)?;
    Ok(())
}

#[cfg(test)]
mod test {
    use cloudsync_common::FileMeta;

    use crate::{db::TABLE_FILES, migrations::meta::TABLE_META};

    use super::*;

    use chrono::Utc;
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
    fn test_migration() {
        let (_dir, db) = test_db();
        let file_metas = ["file1.txt", "file2.txt"].map(|path| FileMeta {
            tenant_id: "".to_string(),
            user_id: "".to_string(),
            path: path.to_string(),
            version: 0,
            created_at: Utc::now(),
            is_deleted: false,
            modified_at: Utc::now(),
            size: 0,
            content_hash: "".to_string(),
        });

        file_metas.iter().for_each(|fm| {
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(TABLE_FILES).unwrap();
                let bytes = serde_json::to_vec(fm).unwrap();
                table.insert(fm.path.as_str(), bytes.as_slice()).unwrap();
            }
            tx.commit().unwrap();
        });

        // Execute
        run_migrations(&db, "tenant1", "user1").unwrap();

        test_migrated_path(&db, "file1.txt");
        test_migrated_path(&db, "file2.txt");

        let meta = meta::get(&db).unwrap();
        assert_eq!(meta.schema_version, 1);
    }

    fn test_migrated_path(db: &Database, old_path: &str) {
        let tx = db.begin_read().unwrap();
        let table = tx.open_table(TABLE_FILES).unwrap();
        let new_key = format!("tenant1\0{}", old_path);
        let entry = table.get(new_key.as_str()).unwrap();
        let Some(entry) = entry else {
            panic!("migrated file_meta not found");
        };
        let bytes = entry.value();
        let file_meta = serde_json::from_slice::<FileMeta>(bytes).unwrap();
        assert_eq!(file_meta.tenant_id, "tenant1");
        assert_eq!(file_meta.user_id, "user1");
        assert_eq!(file_meta.path, old_path);
        let entry = table.get(old_path).unwrap();
        assert!(entry.is_none());
    }
}
