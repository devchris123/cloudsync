use std::sync::Arc;

use cloudsync_common::{InitUploadRequest, Upload};
use redb::{Database, ReadableTable, TableDefinition};

use crate::auth::UserContext;

pub const TABLE_UPLOADS: TableDefinition<&str, &[u8]> = TableDefinition::new("uploads");

pub struct TenantUploadDb {
    db: Arc<Database>,
    user_context: UserContext,
}

impl TenantUploadDb {
    pub fn new(db: Arc<Database>, user_context: UserContext) -> Self {
        TenantUploadDb { db, user_context }
    }

    pub fn get(&self, upload_id: &str) -> Result<Option<Upload>, anyhow::Error> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(TABLE_UPLOADS)?;
        let entry = table.get(upload_id)?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let bytes = entry.value();
        let file_meta = serde_json::from_slice::<Upload>(bytes)?;
        if file_meta.tenant_id != self.user_context.tenant_id {
            return Ok(None);
        }
        Ok(Some(file_meta))
    }

    pub fn create(&self, upload_request: InitUploadRequest) -> anyhow::Result<Upload> {
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
            tenant_id: self.user_context.tenant_id.clone(),
            user_id: self.user_context.user_id.clone(),
        };

        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(TABLE_UPLOADS)?;
            let bytes = serde_json::to_vec(&upload)?;
            table.insert(upload_id.as_str(), bytes.as_slice())?;
        }
        tx.commit()?;
        Ok(upload)
    }

    pub fn add_chunk(&self, upload_id: &str, chunk_index: u32) -> anyhow::Result<Upload> {
        // get upload
        let tx = self.db.begin_write()?;
        let mut upload = {
            let table = tx.open_table(TABLE_UPLOADS)?;
            let entry: Option<redb::AccessGuard<'_, &[u8]>> = table.get(upload_id)?;

            let Some(entry) = entry else {
                anyhow::bail!("upload not found: {}", upload_id);
            };

            let bytes = entry.value();
            serde_json::from_slice::<Upload>(bytes)?
        };

        if upload.tenant_id != self.user_context.tenant_id {
            anyhow::bail!("upload not found: {}", upload_id);
        }
        if upload.chunks_received.contains(&chunk_index) {
            return Ok(upload);
        }

        upload.chunks_received.push(chunk_index);
        upload.modified_at = chrono::Utc::now();

        {
            let mut table = tx.open_table(TABLE_UPLOADS)?;
            let bytes = serde_json::to_vec(&upload)?;
            table.insert(upload_id, bytes.as_slice())?;
        }
        tx.commit()?;

        Ok(upload)
    }

    pub fn delete(&self, upload_id: &str) -> anyhow::Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(TABLE_UPLOADS)?;

            let is_owned = {
                let entry: Option<redb::AccessGuard<'_, &[u8]>> = table.get(upload_id)?;
                if let Some(entry) = entry {
                    let bytes = entry.value();
                    let file_meta = serde_json::from_slice::<Upload>(bytes)?;
                    file_meta.tenant_id == self.user_context.tenant_id
                } else {
                    return Ok(());
                }
            };
            if !is_owned {
                anyhow::bail!("upload not found: {}", upload_id);
            }
            table.remove(upload_id)?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use tempfile::TempDir;

    fn test_db() -> (TempDir, TenantUploadDb) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.redb");
        let db = redb::Database::create(&path).unwrap();
        let tx = db.begin_write().unwrap();
        tx.open_table(TABLE_UPLOADS).unwrap();
        tx.commit().unwrap();
        let db = TenantUploadDb {
            db: Arc::new(db),
            user_context: UserContext {
                tenant_id: "tenant1".to_string(),
                user_id: "user1".to_string(),
            },
        };
        (dir, db)
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
