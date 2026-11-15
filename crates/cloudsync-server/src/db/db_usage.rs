use redb::{ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::db::tx::Transaction;

pub const TABLE_USAGES: TableDefinition<&str, &[u8]> = TableDefinition::new("usages");

#[derive(Serialize, Deserialize, Clone)]
pub struct UsageRecord {
    pub tenant_id: String,
    pub usage: u64,
}

pub fn get_tx(tx: &Transaction, tenant_id: &str) -> anyhow::Result<Option<UsageRecord>> {
    match tx {
        Transaction::Read(tx) => {
            let table = tx.open_table(TABLE_USAGES)?;
            get_from_table(&table, tenant_id)
        }
        Transaction::Write(tx) => {
            let table = tx.open_table(TABLE_USAGES)?;
            get_from_table(&table, tenant_id)
        }
    }
}

fn get_from_table(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    tenant_id: &str,
) -> Result<Option<UsageRecord>, anyhow::Error> {
    let entry = table.get(tenant_id)?;
    let Some(entry) = entry else {
        return Ok(None);
    };
    let bytes = entry.value();
    let user_record = serde_json::from_slice::<UsageRecord>(bytes)?;
    if user_record.tenant_id != tenant_id {
        return Ok(None);
    }
    Ok(Some(user_record))
}

pub fn put_tx(
    tx: &redb::WriteTransaction,
    tenant_id: &str,
    usage: u64,
) -> anyhow::Result<UsageRecord> {
    let user_record = UsageRecord {
        tenant_id: tenant_id.to_owned(),
        usage,
    };
    {
        let mut table = tx.open_table(TABLE_USAGES)?;
        let bytes = serde_json::to_vec(&user_record)?;
        table.insert(tenant_id, bytes.as_slice())?;
    }
    Ok(user_record)
}

pub fn decrement_tx(
    tx: &redb::WriteTransaction,
    tenant_id: &str,
    usage: u64,
) -> anyhow::Result<UsageRecord> {
    let mut user_record = {
        let table = tx.open_table(TABLE_USAGES)?;
        let entry = table.get(tenant_id)?;
        let Some(entry) = entry else {
            tracing::error!(
                tenant_id,
                decrement_bytes = usage,
                "usage record missing during deletion accounting"
            );
            anyhow::bail!("tenant usage record missing");
        };
        let bytes = entry.value();
        serde_json::from_slice::<UsageRecord>(bytes)?
    };
    user_record.usage = user_record.usage.checked_sub(usage).ok_or_else(|| {
        tracing::error!(
            tenant_id,
            current_usage_bytes = user_record.usage,
            decrement_bytes = usage,
            "usage accounting underflow"
        );
        anyhow::anyhow!("usage accounting underflow")
    })?;
    {
        let mut table = tx.open_table(TABLE_USAGES)?;
        let bytes = serde_json::to_vec(&user_record)?;
        table.insert(tenant_id, bytes.as_slice())?;
    }
    Ok(user_record)
}

#[cfg(test)]
mod test {
    use super::*;
    use redb::Database;

    use tempfile::TempDir;

    fn test_db(_tenant_id: &str) -> (TempDir, Database) {
        let dir = TempDir::new().unwrap();
        let db = Database::create(dir.path().join("test.redb")).unwrap();
        let tx = db.begin_write().unwrap();
        tx.open_table(TABLE_USAGES).unwrap();
        tx.commit().unwrap();
        (dir, db)
    }

    #[test]
    fn test_get_not_exist() {
        let (_dir, db) = test_db("tenant1");

        let tx = db.begin_write().unwrap();
        let usage_rec = get_tx(&Transaction::Write(&tx), "tenant1").unwrap();
        tx.commit().unwrap();

        assert!(usage_rec.is_none());
    }

    #[test]
    fn test_get() {
        let (_dir, db) = test_db("tenant1");

        let tx = db.begin_write().unwrap();
        put_tx(&tx, "tenant1", 100).unwrap();
        tx.commit().unwrap();

        let tx = db.begin_write().unwrap();
        let usage_rec = get_tx(&Transaction::Write(&tx), "tenant1")
            .unwrap()
            .unwrap();

        assert_eq!("tenant1", usage_rec.tenant_id);
        assert_eq!(100, usage_rec.usage);
    }

    #[test]
    fn test_tx_isolate_reads() {
        let (_dir, db) = test_db("tenant1");

        let tx1 = db.begin_write().unwrap();
        put_tx(&tx1, "tenant1", 0).unwrap();
        tx1.commit().unwrap();

        let tx1 = db.begin_write().unwrap();
        put_tx(&tx1, "tenant1", 100).unwrap();

        let tx2 = db.begin_read().unwrap();
        let user_record = get_tx(&Transaction::Read(&tx2), "tenant1")
            .unwrap()
            .unwrap();

        assert_eq!("tenant1", user_record.tenant_id);
        assert_eq!(0, user_record.usage);

        tx1.commit().unwrap();

        // The existing read transaction retains its original snapshot.
        assert_eq!(
            0,
            get_tx(&Transaction::Read(&tx2), "tenant1")
                .unwrap()
                .unwrap()
                .usage
        );

        // A new read transaction sees the committed update.
        let tx3 = db.begin_read().unwrap();
        assert_eq!(
            100,
            get_tx(&Transaction::Read(&tx3), "tenant1")
                .unwrap()
                .unwrap()
                .usage
        );
    }
}
