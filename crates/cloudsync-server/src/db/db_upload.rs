use super::tx::Transaction;
use cloudsync_common::{InitUploadRequest, Upload};
use redb::{ReadableTable, TableDefinition};

pub const TABLE_UPLOADS: TableDefinition<&str, &[u8]> = TableDefinition::new("uploads");

pub fn get_tx(
    tx: &Transaction,
    tenant_id: &str,
    upload_id: &str,
) -> anyhow::Result<Option<Upload>> {
    match tx {
        Transaction::Read(tx) => {
            let table = tx.open_table(TABLE_UPLOADS)?;
            get_from_table(&table, tenant_id, upload_id)
        }
        Transaction::Write(tx) => {
            let table = tx.open_table(TABLE_UPLOADS)?;
            get_from_table(&table, tenant_id, upload_id)
        }
    }
}

fn get_from_table(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    tenant_id: &str,
    upload_id: &str,
) -> Result<Option<Upload>, anyhow::Error> {
    let entry = table.get(upload_id)?;
    let Some(entry) = entry else {
        return Ok(None);
    };
    let bytes = entry.value();
    let upload = serde_json::from_slice::<Upload>(bytes)?;
    if upload.tenant_id != tenant_id {
        return Ok(None);
    }
    Ok(Some(upload))
}

pub fn list_tx(tx: &Transaction, tenant_id: &str) -> anyhow::Result<Vec<Upload>> {
    match tx {
        Transaction::Read(tx) => list_table(&tx.open_table(TABLE_UPLOADS)?, tenant_id),
        Transaction::Write(tx) => list_table(&tx.open_table(TABLE_UPLOADS)?, tenant_id),
    }
}

fn list_table(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    tenant_id: &str,
) -> anyhow::Result<Vec<Upload>> {
    let mut uploads = vec![];

    for entry in table.iter()? {
        let (_key, val) = entry?;
        let bytes = val.value();
        let upload = serde_json::from_slice::<Upload>(bytes)?;
        if upload.tenant_id != tenant_id {
            continue;
        }
        uploads.push(upload);
    }

    Ok(uploads)
}

pub fn create_tx(
    tx: &redb::WriteTransaction,
    tenant_id: &str,
    user_id: &str,
    upload_request: InitUploadRequest,
) -> anyhow::Result<Upload> {
    let upload_id = nanoid::nanoid!();
    let upload = Upload {
        upload_id: upload_id.clone(),
        path: upload_request.path,
        total_size: upload_request.total_size,
        total_hash: upload_request.total_hash,
        chunk_count: upload_request.chunk_count,
        chunks_received: vec![],
        created_at: chrono::Utc::now(),
        modified_at: chrono::Utc::now(),
        tenant_id: tenant_id.to_owned(),
        user_id: user_id.to_owned(),
    };
    {
        let mut table = tx.open_table(TABLE_UPLOADS)?;
        let bytes = serde_json::to_vec(&upload)?;
        table.insert(upload_id.as_str(), bytes.as_slice())?;
    }

    Ok(upload)
}

pub fn delete_tx(
    tx: &redb::WriteTransaction,
    tenant_id: &str,
    upload_id: &str,
) -> anyhow::Result<()> {
    let mut table = tx.open_table(TABLE_UPLOADS)?;

    let is_owned = {
        let entry: Option<redb::AccessGuard<'_, &[u8]>> = table.get(upload_id)?;
        if let Some(entry) = entry {
            let bytes = entry.value();
            let file_meta = serde_json::from_slice::<Upload>(bytes)?;
            file_meta.tenant_id == tenant_id
        } else {
            return Ok(());
        }
    };
    if !is_owned {
        anyhow::bail!("upload not found: {}", upload_id);
    }
    table.remove(upload_id)?;
    Ok(())
}

pub fn add_chunk_tx(
    tx: &redb::WriteTransaction,
    tenant_id: &str,
    upload_id: &str,
    chunk_index: u64,
) -> anyhow::Result<Upload> {
    let mut upload = get_tx(&Transaction::Write(tx), tenant_id, upload_id)?
        .ok_or_else(|| anyhow::anyhow!("upload not found: {}", upload_id))?;
    if upload.chunks_received.contains(&chunk_index) {
        return Ok(upload);
    }
    upload.chunks_received.push(chunk_index);
    upload.modified_at = chrono::Utc::now();
    let mut table = tx.open_table(TABLE_UPLOADS)?;
    let bytes = serde_json::to_vec(&upload)?;
    table.insert(upload_id, bytes.as_slice())?;
    Ok(upload)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::db::tx::{with_read_transaction, with_write_transaction};
    use redb::Database;
    use std::sync::Arc;

    use tempfile::TempDir;

    struct TestUploadDb {
        db: Arc<Database>,
        tenant_id: &'static str,
    }
    impl TestUploadDb {
        fn create(&self, req: InitUploadRequest) -> anyhow::Result<Upload> {
            with_write_transaction(&self.db, |tx| create_tx(tx, self.tenant_id, "user1", req))
        }
        fn get(&self, id: &str) -> anyhow::Result<Option<Upload>> {
            with_read_transaction(&self.db, |tx| {
                get_tx(&Transaction::Read(tx), self.tenant_id, id)
            })
        }
        fn add_chunk(&self, id: &str, index: u64) -> anyhow::Result<Upload> {
            with_write_transaction(&self.db, |tx| add_chunk_tx(tx, self.tenant_id, id, index))
        }
        fn delete(&self, id: &str) -> anyhow::Result<()> {
            with_write_transaction(&self.db, |tx| delete_tx(tx, self.tenant_id, id))
        }
    }
    fn test_db() -> (TempDir, TestUploadDb) {
        let dir = TempDir::new().unwrap();
        let db = Database::create(dir.path().join("test.redb")).unwrap();
        with_write_transaction(&db, |tx| -> anyhow::Result<()> {
            tx.open_table(TABLE_UPLOADS)?;
            Ok(())
        })
        .unwrap();
        (
            dir,
            TestUploadDb {
                db: Arc::new(db),
                tenant_id: "tenant1",
            },
        )
    }

    #[test]
    fn ownership_and_duplicate_chunks_preserve_records() {
        let (_dir, db) = test_db();
        let upload = db
            .create(InitUploadRequest {
                path: "file".into(),
                total_size: 3,
                total_hash: "hash".into(),
                chunk_count: 1,
            })
            .unwrap();
        assert_eq!(upload.user_id, "user1");
        let other = TestUploadDb {
            db: db.db.clone(),
            tenant_id: "other",
        };
        assert!(other.get(&upload.upload_id).unwrap().is_none());
        assert!(other.add_chunk(&upload.upload_id, 0).is_err());
        assert!(other.delete(&upload.upload_id).is_err());
        let once = db.add_chunk(&upload.upload_id, 0).unwrap();
        let twice = db.add_chunk(&upload.upload_id, 0).unwrap();
        assert_eq!(
            serde_json::to_value(&once).unwrap(),
            serde_json::to_value(twice).unwrap()
        );
        assert_eq!(once.created_at, upload.created_at);
        assert_eq!(once.user_id, upload.user_id);
    }

    #[test]
    fn test_full_lifecycle() {
        let (_dir, db) = test_db();

        let upload = db
            .create(InitUploadRequest {
                path: "file0".to_string(),
                total_size: 10,
                total_hash: "testhash".to_string(),
                chunk_count: 100,
            })
            .unwrap();

        assert_eq!(upload.chunks_received.len(), 0);

        let upload = db.add_chunk(&upload.upload_id, 0).unwrap();
        let upload = db.add_chunk(&upload.upload_id, 3).unwrap();

        assert_eq!(upload.chunks_received.len(), 2);
        assert!(upload.chunks_received.iter().any(|ch| *ch == 0));
        assert!(upload.chunks_received.iter().any(|ch| *ch == 3));

        let upload_get = db.get(&upload.upload_id).unwrap();
        assert!(upload_get.is_some());

        db.delete(&upload.upload_id).unwrap();

        let upload_get = db.get(&upload.upload_id).unwrap();
        assert!(upload_get.is_none());
    }

    #[test]
    fn test_get_not_exist() {
        let (_dir, db) = test_db();

        let file_meta = db.get("notexist").unwrap();

        assert!(file_meta.is_none());
    }
}
