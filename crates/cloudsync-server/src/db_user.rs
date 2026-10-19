use std::sync::Arc;

use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::auth::UserContext;

pub const TABLE_USAGES: TableDefinition<&str, &[u8]> = TableDefinition::new("usages");

pub struct TenantUserDb {
    db: Arc<Database>,
    user_context: UserContext,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct UsageRecord {
    pub tenant_id: String,
    pub usage: u64,
}

impl TenantUserDb {
    pub fn new(db: Arc<Database>, user_context: UserContext) -> Self {
        TenantUserDb { db, user_context }
    }
    pub fn start_read_tx(&self) -> Result<redb::ReadTransaction, anyhow::Error> {
        Ok(self.db.begin_read()?)
    }

    pub fn start_write_tx(&self) -> Result<redb::WriteTransaction, anyhow::Error> {
        Ok(self.db.begin_write()?)
    }

    pub fn get_read_tx(
        &self,
        tx: &redb::ReadTransaction,
    ) -> Result<Option<UsageRecord>, anyhow::Error> {
        let table = tx.open_table(TABLE_USAGES)?;
        self.get_from_table(&table)
    }

    pub fn get_write_tx(
        &self,
        tx: &redb::WriteTransaction,
    ) -> Result<Option<UsageRecord>, anyhow::Error> {
        let table = tx.open_table(TABLE_USAGES)?;
        self.get_from_table(&table)
    }

    fn get_from_table(
        &self,
        table: &impl ReadableTable<&'static str, &'static [u8]>,
    ) -> Result<Option<UsageRecord>, anyhow::Error> {
        let tenant_id = self.user_context.tenant_id.as_str();
        let entry = table.get(tenant_id)?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let bytes = entry.value();
        let user_record = serde_json::from_slice::<UsageRecord>(bytes)?;
        if user_record.tenant_id != self.user_context.tenant_id {
            return Ok(None);
        }
        Ok(Some(user_record))
    }

    pub fn put_tx(&self, tx: &redb::WriteTransaction, usage: u64) -> anyhow::Result<UsageRecord> {
        let tenant_id = self.user_context.tenant_id.clone();
        let user_record = UsageRecord {
            tenant_id: tenant_id.clone(),
            usage,
        };
        {
            let mut table = tx.open_table(TABLE_USAGES)?;
            let bytes = serde_json::to_vec(&user_record)?;
            table.insert(tenant_id.as_str(), bytes.as_slice())?;
        }
        Ok(user_record)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use tempfile::TempDir;

    fn test_db(tenant_id: &str) -> (TempDir, TenantUserDb) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.redb");
        let db = redb::Database::create(&path).unwrap();
        let tx = db.begin_write().unwrap();
        tx.open_table(TABLE_USAGES).unwrap();
        tx.commit().unwrap();
        let db = TenantUserDb {
            db: Arc::new(db),
            user_context: UserContext {
                tenant_id: tenant_id.to_string(),
                user_id: "user1".to_string(),
            },
        };
        (dir, db)
    }

    #[test]
    fn test_get_not_exist() {
        let (_dir, db) = test_db("tenant1");

        let tx = db.start_write_tx().unwrap();
        let usage_rec = db.get_write_tx(&tx).unwrap();
        tx.commit().unwrap();

        assert!(usage_rec.is_none());
    }

    #[test]
    fn test_get() {
        let (_dir, db) = test_db("tenant1");

        let tx = db.start_write_tx().unwrap();
        db.put_tx(&tx, 100).unwrap();
        tx.commit().unwrap();

        let tx = db.start_write_tx().unwrap();
        let usage_rec = db.get_write_tx(&tx).unwrap().unwrap();

        assert_eq!("tenant1", usage_rec.tenant_id);
        assert_eq!(100, usage_rec.usage);
    }

    #[test]
    fn test_tx_isolate_reads() {
        let (_dir, db) = test_db("tenant1");

        let tx1 = db.start_write_tx().unwrap();
        db.put_tx(&tx1, 0).unwrap();
        tx1.commit().unwrap();

        let tx1 = db.start_write_tx().unwrap();
        db.put_tx(&tx1, 100).unwrap();

        let tx2 = db.start_read_tx().unwrap();
        let user_record = db.get_read_tx(&tx2).unwrap().unwrap();

        assert_eq!("tenant1", user_record.tenant_id);
        assert_eq!(0, user_record.usage);

        tx1.commit().unwrap();

        // The existing read transaction retains its original snapshot.
        assert_eq!(0, db.get_read_tx(&tx2).unwrap().unwrap().usage);

        // A new read transaction sees the committed update.
        let tx3 = db.start_read_tx().unwrap();
        assert_eq!(100, db.get_read_tx(&tx3).unwrap().unwrap().usage);
    }
}
