use std::sync::Arc;

use cloudsync_common::FileMeta;
use redb::{Database, ReadableTable, TableDefinition};

use crate::{auth::UserContext, db_upload::TABLE_UPLOADS};

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

pub struct TenantDb {
    db: Arc<Database>,
    user_context: UserContext,
}

impl TenantDb {
    pub fn new(db: Arc<Database>, user_context: UserContext) -> Self {
        TenantDb { db, user_context }
    }

    pub fn list(&self) -> Result<Vec<FileMeta>, anyhow::Error> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(TABLE_FILES)?;

        let mut file_metas: Vec<FileMeta> = Vec::new();
        for entry in table.iter()? {
            let (key, val) = entry?;
            if !key
                .value()
                .starts_with(&format!("{}\0", self.user_context.tenant_id))
            {
                continue;
            }
            let bytes = val.value();
            let file_meta = serde_json::from_slice::<FileMeta>(bytes)?;
            if file_meta.is_deleted {
                continue;
            }
            file_metas.push(file_meta);
        }

        Ok(file_metas)
    }

    pub fn get(&self, path: &str) -> Result<Option<FileMeta>, anyhow::Error> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(TABLE_FILES)?;
        let keyed_path = self.make_key(path);
        let entry = table.get(keyed_path.as_str())?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let bytes = entry.value();
        let file_meta = serde_json::from_slice::<FileMeta>(bytes)?;
        Ok(Some(file_meta))
    }

    pub fn put(&self, path: &str, size: u64, content_hash: &str) -> anyhow::Result<FileMeta> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(TABLE_FILES)?;
        let keyed_path = self.make_key(path);
        let entry = table.get(keyed_path.as_str())?;

        let mut file_meta = FileMeta {
            path: path.to_string(),
            size,
            content_hash: content_hash.to_string(),
            version: 1,
            is_deleted: false,
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            tenant_id: self.user_context.tenant_id.clone(),
            user_id: self.user_context.user_id.clone(),
        };

        if let Some(entry) = entry {
            let bytes = entry.value();
            let file_meta_raw = serde_json::from_slice::<FileMeta>(bytes)?;
            file_meta.version = file_meta_raw.version + 1;
            file_meta.created_at = file_meta_raw.created_at;
        }
        drop(table);

        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(TABLE_FILES)?;
            let bytes = serde_json::to_vec(&file_meta)?;
            table.insert(keyed_path.as_str(), bytes.as_slice())?;
        }
        tx.commit()?;
        Ok(file_meta)
    }

    pub fn delete(&self, path: &str) -> anyhow::Result<()> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(TABLE_FILES)?;
        let keyed_path = self.make_key(path);
        let entry = table.get(keyed_path.as_str())?;
        let Some(entry) = entry else { return Ok(()) };
        let bytes = entry.value();
        let mut file_meta = serde_json::from_slice::<FileMeta>(bytes)?;
        file_meta.is_deleted = true;
        drop(table);

        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(TABLE_FILES)?;
            let bytes = serde_json::to_vec(&file_meta)?;
            table.insert(keyed_path.as_str(), bytes.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    fn make_key(&self, path: &str) -> String {
        format!("{}\0{}", self.user_context.tenant_id, path)
    }
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

    fn test_tenant(db: Arc<Database>, tenant_id: Option<String>) -> TenantDb {
        TenantDb {
            db,
            user_context: UserContext {
                tenant_id: tenant_id.unwrap_or("tenant1".to_string()),
                user_id: "user1".to_string(),
            },
        }
    }

    #[test]
    fn test_full_lifecycle() {
        let (_dir, db) = test_db();
        let db = test_tenant(Arc::new(db), None);

        let path = "somepath/test.txt";
        let bytes = b"hello world";
        let hash = hash_bytes(bytes);
        let size = bytes.len() as u64;
        let file_meta = db.put(path, size, &hash).unwrap();

        assert_eq!(file_meta.path, path);
        assert_eq!(file_meta.content_hash, hash);
        assert_eq!(file_meta.size, size);
        assert_eq!(file_meta.version, 1);
        assert_eq!(file_meta.is_deleted, false);

        let file_meta = db.get(path).unwrap().unwrap();
        assert_eq!(file_meta.path, path);
        assert_eq!(file_meta.content_hash, hash);
        assert_eq!(file_meta.size, size);
        assert_eq!(file_meta.version, 1);
        assert_eq!(file_meta.is_deleted, false);

        let file_meta = db.put(path, size, &hash).unwrap();
        assert_eq!(file_meta.version, 2);

        let path = "somepath/test2.txt";
        let bytes = b"hello world";
        let hash = hash_bytes(bytes);
        let size = bytes.len() as u64;
        db.put(path, size, &hash).unwrap();
        let file_metas = db.list().unwrap();
        assert_eq!(file_metas.len(), 2);

        db.delete(path).unwrap();

        let file_metas = db.list().unwrap();
        assert_eq!(file_metas.len(), 1);
    }

    #[test]
    fn test_get_not_exist() {
        let (_dir, db) = test_db();
        let db = test_tenant(Arc::new(db), None);

        let file_meta = db.get("notexist").unwrap();

        assert!(file_meta.is_none());
    }

    #[test]
    fn test_tenanta_cannot_see_tenantb() {
        let (_dir, db) = test_db();
        let tenanta_name = "tenanta".to_string();
        let tenantb_name = "tenantb".to_string();
        let arc_db = Arc::new(db);
        let tenanta = test_tenant(Arc::clone(&arc_db), Some(tenanta_name.clone()));
        let tenantb = test_tenant(arc_db, Some(tenantb_name.clone()));

        let path = "tenanta/test.txt";
        let bytes = b"hello world";
        let hash = hash_bytes(bytes);
        let size = bytes.len() as u64;
        let file_metaa = tenanta.put(path, size, &hash).unwrap();
        assert_eq!(tenanta_name, file_metaa.tenant_id);

        let path = "tenantb/test.txt";
        let bytes = b"hello world";
        let hash = hash_bytes(bytes);
        let size = bytes.len() as u64;
        let file_metab = tenantb.put(path, size, &hash).unwrap();
        assert_eq!(tenantb_name, file_metab.tenant_id);

        let patha = tenanta.get("tenanta/test.txt").unwrap();
        assert!(patha.is_some());
        let pathb = tenanta.get("tenantb/test.txt").unwrap();
        assert!(pathb.is_none());
        let patha = tenantb.get("tenanta/test.txt").unwrap();
        assert!(patha.is_none());
    }
}
