//! Server repositories expose only transaction-taking `_tx` operations.
//! Callers supply tenant IDs (and user IDs for attribution), own transaction
//! boundaries, and may compose repositories without hidden commits. Global
//! tenant enumeration/counting take no tenant. Read operations accept `Transaction`;
//! writes require `&WriteTransaction`. Use `tx` helpers for ordinary scoped work,
//! or explicit redb transactions for existing special commit/abort paths.

pub mod db_file;
pub mod db_pending_blob_deletions;
pub mod db_tenants;
pub mod db_upload;
pub mod db_usage;
pub mod tx;
