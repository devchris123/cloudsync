use super::tx::Transaction;
use cloudsync_common::FileMeta;
use redb::{Database, MultimapTableDefinition, ReadableTable, TableDefinition};

use crate::db::{db_tenants::TABLE_TENANTS, db_upload::TABLE_UPLOADS};

pub const TABLE_FILES: TableDefinition<&str, &[u8]> = TableDefinition::new("files");
pub const INDEX_FILES_BY_TENANT_HASH: MultimapTableDefinition<&str, &[u8]> =
    MultimapTableDefinition::new("files_by_tenant_hash");

pub fn open_db(db_path: &str) -> anyhow::Result<Database> {
    let db: Database = Database::create(db_path)?;
    let tx = db.begin_write()?;
    {
        tx.open_table(TABLE_FILES)?;
        tx.open_table(TABLE_UPLOADS)?;
        tx.open_table(TABLE_TENANTS)?;
        tx.open_multimap_table(INDEX_FILES_BY_TENANT_HASH)?;
    }
    tx.commit()?;
    Ok(db)
}

pub fn get_tx(tx: &Transaction, tenant_id: &str, path: &str) -> anyhow::Result<Option<FileMeta>> {
    match tx {
        Transaction::Read(tx) => {
            let table = tx.open_table(TABLE_FILES)?;
            get_table(&table, tenant_id, path)
        }
        Transaction::Write(tx) => {
            let table = tx.open_table(TABLE_FILES)?;
            get_table(&table, tenant_id, path)
        }
    }
}

fn get_table(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    tenant_id: &str,
    path: &str,
) -> anyhow::Result<Option<FileMeta>> {
    let keyed_path = make_key(tenant_id, path);
    let entry = table.get(keyed_path.as_str())?;
    let Some(entry) = entry else {
        return Ok(None);
    };
    let bytes = entry.value();
    let file_meta = serde_json::from_slice::<FileMeta>(bytes)?;
    Ok(Some(file_meta))
}

pub fn put_tx(
    tx: &redb::WriteTransaction,
    tenant_id: &str,
    user_id: &str,
    path: &str,
    size: u64,
    content_hash: &str,
) -> anyhow::Result<FileMeta> {
    let mut file_meta = FileMeta {
        path: path.to_string(),
        size,
        content_hash: content_hash.to_string(),
        version: 1,
        is_deleted: false,
        created_at: chrono::Utc::now(),
        modified_at: chrono::Utc::now(),
        tenant_id: tenant_id.to_owned(),
        user_id: user_id.to_owned(),
    };

    let keyed_path = make_key(tenant_id, path);
    let mut table = tx.open_table(TABLE_FILES)?;
    {
        let entry = table.get(keyed_path.as_str())?;
        if let Some(entry) = entry {
            let bytes = entry.value();
            let file_meta_old = serde_json::from_slice::<FileMeta>(bytes)?;
            file_meta.version = file_meta_old.version + 1;
            file_meta.created_at = file_meta_old.created_at;
            // If an entry exists we need to update the index
            let old_hash = file_meta_old.content_hash;
            let index_key = make_index_key(tenant_id, &old_hash);
            let mut table = tx.open_multimap_table(INDEX_FILES_BY_TENANT_HASH)?;
            table.remove(index_key.as_str(), keyed_path.as_bytes())?;
        }
    }

    {
        let bytes = serde_json::to_vec(&file_meta)?;
        table.insert(keyed_path.as_str(), bytes.as_slice())?;
    }

    drop(table);
    {
        let index_key = make_index_key(tenant_id, content_hash);
        let mut index = tx.open_multimap_table(INDEX_FILES_BY_TENANT_HASH)?;
        index.insert(index_key.as_str(), keyed_path.as_bytes())?;
    }
    Ok(file_meta)
}

pub fn make_key(tenant_id: &str, path: &str) -> String {
    format!("{}\0{}", tenant_id, path)
}

pub fn make_index_key(tenant_id: &str, hash: &str) -> String {
    format!("{}\0{}", tenant_id, hash)
}

#[cfg(test)]
pub fn decode_key(key: &str) -> anyhow::Result<(&str, &str)> {
    key.split_once('\0')
        .ok_or_else(|| anyhow::anyhow!("invalid key"))
}

/// Tombstone the current row in the caller's writer; retain attribution, version and index.
pub fn delete_tx(tx: &redb::WriteTransaction, tenant_id: &str, path: &str) -> anyhow::Result<()> {
    let mut table = tx.open_table(TABLE_FILES)?;
    let key = make_key(tenant_id, path);
    let Some(mut file) = get_table(&table, tenant_id, path)? else {
        return Ok(());
    };
    file.is_deleted = true;
    let bytes = serde_json::to_vec(&file)?;
    table.insert(key.as_str(), bytes.as_slice())?;
    Ok(())
}

pub fn list_tx(tx: &Transaction, tenant_id: &str) -> anyhow::Result<Vec<FileMeta>> {
    match tx {
        Transaction::Read(tx) => list_table(&tx.open_table(TABLE_FILES)?, tenant_id),
        Transaction::Write(tx) => list_table(&tx.open_table(TABLE_FILES)?, tenant_id),
    }
}

fn list_table(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    tenant_id: &str,
) -> anyhow::Result<Vec<FileMeta>> {
    let mut files = Vec::new();
    let start = format!("{tenant_id}\0");
    let end = format!("{tenant_id}\u{1}");
    for entry in table.range(start.as_str()..end.as_str())? {
        let (_, value) = entry?;
        let file: FileMeta = serde_json::from_slice(value.value())?;
        if !file.is_deleted {
            files.push(file);
        }
    }
    Ok(files)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::db::tx::{with_read_transaction, with_write_transaction};
    use std::sync::Arc;

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

    struct TestTenant {
        db: Arc<Database>,
        tenant_id: String,
        user_id: String,
    }
    impl TestTenant {
        fn put(&self, path: &str, size: u64, hash: &str) -> anyhow::Result<FileMeta> {
            with_write_transaction(&self.db, |tx| {
                put_tx(tx, &self.tenant_id, &self.user_id, path, size, hash)
            })
        }
        fn put_tx(
            &self,
            tx: &redb::WriteTransaction,
            path: &str,
            size: u64,
            hash: &str,
        ) -> anyhow::Result<FileMeta> {
            put_tx(tx, &self.tenant_id, &self.user_id, path, size, hash)
        }
        fn get(&self, path: &str) -> anyhow::Result<Option<FileMeta>> {
            with_read_transaction(&self.db, |tx| {
                get_tx(&Transaction::Read(tx), &self.tenant_id, path)
            })
        }
        fn list(&self) -> anyhow::Result<Vec<FileMeta>> {
            with_read_transaction(&self.db, |tx| {
                list_tx(&Transaction::Read(tx), &self.tenant_id)
            })
        }
        fn delete(&self, path: &str) -> anyhow::Result<()> {
            with_write_transaction(&self.db, |tx| delete_tx(tx, &self.tenant_id, path))
        }
    }
    fn test_tenant(db: Arc<Database>, tenant_id: Option<String>) -> TestTenant {
        TestTenant {
            db,
            tenant_id: tenant_id.unwrap_or("tenant1".into()),
            user_id: "user1".into(),
        }
    }

    #[test]
    fn replacement_preserves_creation_and_updates_attribution() {
        let (_dir, db) = test_db();
        let db = Arc::new(db);
        let first_user = test_tenant(db.clone(), None);
        let second_user = TestTenant {
            db: db.clone(),
            tenant_id: "tenant1".into(),
            user_id: "user2".into(),
        };
        let first = first_user.put("file", 3, "old").unwrap();
        first_user.delete("file").unwrap();
        let deleted = first_user.get("file").unwrap().unwrap();
        let mut expected = serde_json::to_value(&first).unwrap();
        expected["is_deleted"] = true.into();
        assert_eq!(serde_json::to_value(deleted).unwrap(), expected);
        let replaced = second_user.put("file", 4, "new").unwrap();
        assert_eq!(replaced.created_at, first.created_at);
        assert!(replaced.modified_at >= first.modified_at);
        assert_eq!(replaced.version, 2);
        assert_eq!(replaced.user_id, "user2");
        assert!(!replaced.is_deleted);
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

    #[test]
    fn put_tx_adds_index() {
        let (_dir, db) = test_db();
        let tenanta_name = "tenanta".to_string();
        let arc_db = Arc::new(db);
        let tenanta = test_tenant(Arc::clone(&arc_db), Some(tenanta_name.clone()));

        let path = "tenantb/test.txt";
        let bytes = b"hello world";
        let hash = hash_bytes(bytes);
        let tx = arc_db.begin_write().unwrap();
        tenanta
            .put_tx(&tx, path, bytes.len() as u64, &hash)
            .unwrap();

        let path2 = "tenantb/test2.txt";
        tenanta
            .put_tx(&tx, path2, bytes.len() as u64, &hash)
            .unwrap();

        tx.commit().unwrap();

        let tx = arc_db.begin_read().unwrap();
        let table = tx.open_multimap_table(INDEX_FILES_BY_TENANT_HASH).unwrap();
        let index_key = make_index_key(&tenanta_name, &hash);
        let mut value_map = table.get(index_key.as_str()).unwrap();

        assert_eq!(value_map.len(), 2);
        let value = value_map.next().unwrap().unwrap();
        let value = std::str::from_utf8(value.value()).unwrap();
        let (dec_tenant_id, dec_path) = decode_key(value).unwrap();
        assert_eq!(dec_tenant_id, tenanta_name);
        assert_eq!(dec_path, path);

        let value = value_map.next().unwrap().unwrap();
        let value = std::str::from_utf8(value.value()).unwrap();
        let (dec_tenant_id, dec_path) = decode_key(value).unwrap();
        assert_eq!(dec_tenant_id, tenanta_name);
        assert_eq!(dec_path, path2);
    }

    #[test]
    fn put_tx_updates_index() {
        let (_dir, db) = test_db();
        let tenanta_name = "tenanta".to_string();
        let arc_db = Arc::new(db);
        let tenanta = test_tenant(Arc::clone(&arc_db), Some(tenanta_name.clone()));

        let path = "tenantb/test.txt";
        let bytes = b"hello world";
        let hash = hash_bytes(bytes);
        let tx = arc_db.begin_write().unwrap();
        tenanta
            .put_tx(&tx, path, bytes.len() as u64, &hash)
            .unwrap();
        let updated_bytes = b"hello world2";
        let updated_hash = hash_bytes(updated_bytes);
        tenanta
            .put_tx(&tx, path, updated_bytes.len() as u64, &updated_hash)
            .unwrap();
        tx.commit().unwrap();

        let tx = arc_db.begin_read().unwrap();
        let table = tx.open_multimap_table(INDEX_FILES_BY_TENANT_HASH).unwrap();

        let index_key = make_index_key(&tenanta_name, &hash);
        let value_map = table.get(index_key.as_str()).unwrap();
        assert_eq!(value_map.len(), 0);

        let index_key = make_index_key(&tenanta_name, &updated_hash);
        let mut value_map = table.get(index_key.as_str()).unwrap();
        assert_eq!(value_map.len(), 1);

        let value = value_map.next().unwrap().unwrap();
        let value = std::str::from_utf8(value.value()).unwrap();
        let (dec_tenant_id, dec_path) = decode_key(value).unwrap();
        assert_eq!(dec_tenant_id, tenanta_name);
        assert_eq!(dec_path, path);
    }

    #[test]
    fn delete_tx_keeps_index() {
        let (_dir, db) = test_db();
        let tenanta_name = "tenanta".to_string();
        let arc_db = Arc::new(db);
        let tenanta = test_tenant(Arc::clone(&arc_db), Some(tenanta_name.clone()));

        let path = "tenantb/test.txt";
        let bytes = b"hello world";
        let hash = hash_bytes(bytes);
        let tx = arc_db.begin_write().unwrap();
        tenanta
            .put_tx(&tx, path, bytes.len() as u64, &hash)
            .unwrap();
        tx.commit().unwrap();

        tenanta.delete(path).unwrap();

        assert!(tenanta.get(path).unwrap().unwrap().is_deleted);
        let tx = arc_db.begin_read().unwrap();
        let table = tx.open_multimap_table(INDEX_FILES_BY_TENANT_HASH).unwrap();
        let index_key = make_index_key(&tenanta_name, &hash);
        let mut value_map = table.get(index_key.as_str()).unwrap();
        assert_eq!(value_map.len(), 1);
        let value = value_map.next().unwrap().unwrap();
        let value = std::str::from_utf8(value.value()).unwrap();
        let (dec_tenant_id, dec_path) = decode_key(value).unwrap();
        assert_eq!(dec_tenant_id, tenanta_name);
        assert_eq!(dec_path, path);
    }
    #[test]
    fn deletion_preserves_replacement_at_writer_boundary() {
        let (_dir, db) = test_db();
        let db = Arc::new(db);
        let tenant = test_tenant(db.clone(), None);
        tenant.put("file", 3, "old").unwrap();
        let old_snapshot = db.begin_read().unwrap();
        let tx = db.begin_write().unwrap();
        let replacement = put_tx(&tx, "tenant1", "replacer", "file", 7, "replacement").unwrap();
        // A deletion composed with an uncommitted replacement must read that writer,
        // never capture the old metadata from a separate snapshot.
        delete_tx(&tx, "tenant1", "file").unwrap();
        tx.commit().unwrap();
        assert_eq!(
            get_tx(&Transaction::Read(&old_snapshot), "tenant1", "file")
                .unwrap()
                .unwrap()
                .content_hash,
            "old"
        );
        let deleted = tenant.get("file").unwrap().unwrap();
        let mut expected = serde_json::to_value(replacement).unwrap();
        expected["is_deleted"] = true.into();
        assert_eq!(serde_json::to_value(deleted).unwrap(), expected);
        let tx = db.begin_read().unwrap();
        let index = tx.open_multimap_table(INDEX_FILES_BY_TENANT_HASH).unwrap();
        assert_eq!(
            index
                .get(make_index_key("tenant1", "replacement").as_str())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            index
                .get(make_index_key("tenant1", "old").as_str())
                .unwrap()
                .len(),
            0
        );
    }
}
