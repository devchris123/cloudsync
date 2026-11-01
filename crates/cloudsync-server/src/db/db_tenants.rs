use redb::{ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

pub const TABLE_TENANTS: TableDefinition<&str, &[u8]> = TableDefinition::new("tenants");

pub struct TenantDb {}

#[derive(Serialize, Deserialize, Clone)]
pub struct TenantRecord {
    pub tenant_id: String,
}

impl TenantDb {
    pub fn new() -> Self {
        TenantDb {}
    }

    pub fn exists(&self, tx: &redb::WriteTransaction, tenant_id: &str) -> anyhow::Result<bool> {
        let table = tx.open_table(TABLE_TENANTS)?;
        let result = table.get(tenant_id)?;
        match result {
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }

    pub fn count(&self, tx: &redb::WriteTransaction) -> anyhow::Result<u64> {
        let table = tx.open_table(TABLE_TENANTS)?;
        Ok(table.iter()?.count() as u64)
    }

    pub fn create(
        &self,
        tx: &redb::WriteTransaction,
        rec: TenantRecord,
    ) -> anyhow::Result<TenantRecord> {
        {
            let mut table = tx.open_table(TABLE_TENANTS)?;
            let bytes = serde_json::to_vec(&rec)?;
            table.insert(rec.tenant_id.as_str(), bytes.as_slice())?;
        }
        Ok(rec)
    }

    pub fn list(&self, tx: &redb::ReadTransaction) -> anyhow::Result<Vec<TenantRecord>> {
        let table = tx.open_table(TABLE_TENANTS)?;
        let mut tenants: Vec<TenantRecord> = Vec::new();
        for entry in table.iter()? {
            let (_key, val) = entry?;
            let bytes = val.value();
            let tenant_record = serde_json::from_slice::<TenantRecord>(bytes)?;
            tenants.push(tenant_record);
        }
        Ok(tenants)
    }
}
