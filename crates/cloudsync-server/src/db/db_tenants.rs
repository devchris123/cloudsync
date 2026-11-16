use super::tx::Transaction;
use redb::{ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

pub const TABLE_TENANTS: TableDefinition<&str, &[u8]> = TableDefinition::new("tenants");

#[derive(Serialize, Deserialize, Clone)]
pub struct TenantRecord {
    pub tenant_id: String,
}

pub fn exists_tx(tx: &Transaction, tenant_id: &str) -> anyhow::Result<bool> {
    match tx {
        Transaction::Read(tx) => exists_table(&tx.open_table(TABLE_TENANTS)?, tenant_id),
        Transaction::Write(tx) => exists_table(&tx.open_table(TABLE_TENANTS)?, tenant_id),
    }
}
fn exists_table(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    tenant_id: &str,
) -> anyhow::Result<bool> {
    Ok(table.get(tenant_id)?.is_some())
}
pub fn count_tx(tx: &Transaction) -> anyhow::Result<u64> {
    match tx {
        Transaction::Read(tx) => count_table(&tx.open_table(TABLE_TENANTS)?),
        Transaction::Write(tx) => count_table(&tx.open_table(TABLE_TENANTS)?),
    }
}
fn count_table(table: &impl ReadableTable<&'static str, &'static [u8]>) -> anyhow::Result<u64> {
    Ok(table.iter()?.count() as u64)
}
pub fn create_tx(tx: &redb::WriteTransaction, rec: TenantRecord) -> anyhow::Result<TenantRecord> {
    let mut table = tx.open_table(TABLE_TENANTS)?;
    let bytes = serde_json::to_vec(&rec)?;
    table.insert(rec.tenant_id.as_str(), bytes.as_slice())?;
    Ok(rec)
}
pub fn list_tx(tx: &Transaction) -> anyhow::Result<Vec<TenantRecord>> {
    match tx {
        Transaction::Read(tx) => list_table(&tx.open_table(TABLE_TENANTS)?),
        Transaction::Write(tx) => list_table(&tx.open_table(TABLE_TENANTS)?),
    }
}
fn list_table(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
) -> anyhow::Result<Vec<TenantRecord>> {
    let mut tenants = Vec::new();
    for entry in table.iter()? {
        let (_, value) = entry?;
        tenants.push(serde_json::from_slice(value.value())?);
    }
    Ok(tenants)
}
