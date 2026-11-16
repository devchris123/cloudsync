use super::tx::Transaction;
use chrono::{DateTime, Utc};
use redb::{ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

pub const TABLE_PENDING_DELETIONS: TableDefinition<&str, &[u8]> =
    TableDefinition::new("pending_deletions");

#[derive(Serialize, Deserialize, Clone)]
pub struct PendingBlobDeletionRecord {
    pub size_bytes: u64,
    pub marked_at: DateTime<Utc>,
}

pub fn exists_tx(tx: &Transaction, tenant_id: &str, hash: &str) -> anyhow::Result<bool> {
    match tx {
        Transaction::Read(tx) => {
            let table = tx.open_table(TABLE_PENDING_DELETIONS)?;
            exists_table(&table, tenant_id, hash)
        }
        Transaction::Write(tx) => {
            let table = tx.open_table(TABLE_PENDING_DELETIONS)?;
            exists_table(&table, tenant_id, hash)
        }
    }
}

pub fn put_tx(
    tx: &redb::WriteTransaction,
    tenant_id: &str,
    hash: &str,
    rec: PendingBlobDeletionRecord,
) -> anyhow::Result<PendingBlobDeletionRecord> {
    {
        let mut table = tx.open_table(TABLE_PENDING_DELETIONS)?;
        let bytes = serde_json::to_vec(&rec)?;
        let key = make_key(tenant_id, hash);
        table.insert(key.as_str(), bytes.as_slice())?;
    }
    Ok(rec)
}

pub fn delete_tx(tx: &redb::WriteTransaction, tenant_id: &str, hash: &str) -> anyhow::Result<()> {
    {
        let mut table = tx.open_table(TABLE_PENDING_DELETIONS)?;
        let key = make_key(tenant_id, hash);
        table.remove(key.as_str())?;
    }
    Ok(())
}

pub fn list_tx(
    tx: &Transaction,
    tenant_id: &str,
) -> anyhow::Result<Vec<(String, PendingBlobDeletionRecord)>> {
    match tx {
        Transaction::Read(tx) => list_table(&tx.open_table(TABLE_PENDING_DELETIONS)?, tenant_id),
        Transaction::Write(tx) => list_table(&tx.open_table(TABLE_PENDING_DELETIONS)?, tenant_id),
    }
}

fn list_table(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    tenant_id: &str,
) -> anyhow::Result<Vec<(String, PendingBlobDeletionRecord)>> {
    let mut records: Vec<(String, PendingBlobDeletionRecord)> = Vec::new();
    let start = format!("{}\0", tenant_id);
    let end = format!("{}\u{1}", tenant_id);
    for entry in table.range(start.as_str()..end.as_str())? {
        let (key, val) = entry?;
        let bytes = val.value();
        let record = serde_json::from_slice::<PendingBlobDeletionRecord>(bytes)?;
        let (_tenant_id, hash) = decode_key(key.value())?;
        records.push((hash, record));
    }
    Ok(records)
}

pub fn make_key(tenant_id: &str, hash: &str) -> String {
    format!("{}\0{}", tenant_id, hash)
}

pub fn decode_key(key: &str) -> anyhow::Result<(String, String)> {
    let (tenant_id, hash) = key
        .split_once('\0')
        .ok_or_else(|| anyhow::anyhow!("invalid blob index key"))?;
    Ok((tenant_id.to_string(), hash.to_string()))
}

fn exists_table(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    tenant_id: &str,
    hash: &str,
) -> anyhow::Result<bool> {
    let key = make_key(tenant_id, hash);
    Ok(table.get(key.as_str())?.is_some())
}
