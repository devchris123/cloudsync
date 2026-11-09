use std::{collections::HashSet, path::Path, sync::Arc};

use anyhow::{Context, Result};
use redb::{AccessGuard, Database, MultimapValue, ReadableMultimapTable, WriteTransaction};

use crate::{
    auth::UserContext,
    db::{
        db_file::{INDEX_FILES_BY_TENANT_HASH, TenantFileMetaDb},
        db_pending_blob_deletions::{PendingBlobDeletionDb, PendingBlobDeletionRecord},
        db_tenants::TenantDb,
        db_upload::TenantUploadDb,
        db_usage::TenantUsageDb,
    },
    storage,
};

/// Committed outcomes; recovered deletions are separate from filesystem removals.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcReport {
    pub newly_marked_blobs: usize,
    pub deleted_blobs: usize,
    pub already_absent_blobs: usize,
    pub released_bytes: u64,
}

impl GcReport {
    pub fn has_work(&self) -> bool {
        self.newly_marked_blobs != 0 || self.deleted_blobs != 0 || self.already_absent_blobs != 0
    }

    fn merge(&mut self, other: Self) {
        self.newly_marked_blobs += other.newly_marked_blobs;
        self.deleted_blobs += other.deleted_blobs;
        self.already_absent_blobs += other.already_absent_blobs;
        self.released_bytes += other.released_bytes;
    }
}

pub fn garbage_collect_deleted_blobs(data_dir: &Path, db: Arc<Database>) -> Result<GcReport> {
    let tenant_db = TenantDb::new();
    let tenants = {
        let tx = db
            .begin_read()
            .context("opening tenant discovery transaction")?;
        tenant_db.list(&tx).context("listing tenants for GC")?
    };
    let mut report = GcReport::default();
    for tenant in tenants {
        report.merge(mark_and_delete_blobs_for_tenant(
            data_dir,
            db.clone(),
            &tenant.tenant_id,
        )?);
    }
    Ok(report)
}

fn mark_and_delete_blobs_for_tenant(
    data_dir: &Path,
    db: Arc<Database>,
    tenant_id: &str,
) -> Result<GcReport> {
    let started = std::time::Instant::now();
    let marked = mark_blobs_for_garbage_collection(data_dir, db.clone(), tenant_id)
        .with_context(|| format!("marking blobs for tenant {tenant_id}"))?;
    let mut report = delete_marked_blobs(data_dir, db.clone(), tenant_id)
        .with_context(|| format!("deleting pending blobs for tenant {tenant_id}"))?;
    report.newly_marked_blobs = marked.len();
    tracing::debug!(
        tenant_id,
        newly_marked_blobs = report.newly_marked_blobs,
        deleted_blobs = report.deleted_blobs,
        already_absent_blobs = report.already_absent_blobs,
        released_bytes = report.released_bytes,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "tenant garbage collection completed"
    );
    Ok(report)
}

pub(crate) fn mark_blobs_for_garbage_collection(
    data_dir: &Path,
    db: Arc<Database>,
    tenant_id: &str,
) -> Result<Vec<String>> {
    let mut marked_blobs: Vec<String> = vec![];
    let tx = db.begin_write()?;
    {
        let file_db = TenantFileMetaDb::new(
            db.clone(),
            UserContext {
                tenant_id: tenant_id.to_string(),
                user_id: tenant_id.to_string(),
            },
        );
        let upload_db = TenantUploadDb::new(
            db.clone(),
            UserContext {
                tenant_id: tenant_id.to_string(),
                user_id: tenant_id.to_string(),
            },
        );
        let active_upload_hashes: HashSet<String> = upload_db
            .list_tx(&tx)?
            .into_iter()
            .map(|upload| upload.total_hash)
            .collect();
        let table = tx.open_multimap_table(INDEX_FILES_BY_TENANT_HASH)?;
        let start = format!("{}\0", tenant_id);
        let end = format!("{}\u{1}", tenant_id);
        for index_entry in table.range(start.as_str()..end.as_str())? {
            let index_entry = index_entry.context("reading blob reference index")?;
            let (hash, marked) = mark_blob_for_garbage_collection(
                &tx,
                &file_db,
                &active_upload_hashes,
                data_dir,
                index_entry,
            )?;
            if marked {
                marked_blobs.push(hash.to_string());
            }
        }
    }
    tx.commit()?;
    Ok(marked_blobs)
}

fn mark_blob_for_garbage_collection(
    tx: &WriteTransaction,
    file_db: &TenantFileMetaDb,
    active_upload_hashes: &HashSet<String>,
    data_dir: &Path,
    index_entry: (AccessGuard<'_, &str>, MultimapValue<'_, &[u8]>),
) -> anyhow::Result<(String, bool)> {
    let (index_key_guard, file_keys) = index_entry;
    let index_key = index_key_guard.value();
    let (tenant_id, hash) = index_key
        .split_once('\0')
        .ok_or_else(|| anyhow::anyhow!("invalid blob index key"))?;

    (|| -> Result<(String, bool)> {
        if active_upload_hashes.contains(hash) {
            tracing::trace!(
                tenant_id,
                blob_hash = hash,
                reason = "active_upload",
                "blob protected from GC"
            );
            return Ok((hash.to_owned(), false));
        }
        let mut is_referenced = false;
        for file_key in file_keys {
            let file_key_guard = file_key?;
            is_referenced = is_blob_referenced(tx, file_db, file_key_guard)
                .context("checking blob references")?;
            if is_referenced {
                break;
            }
        }
        if is_referenced {
            tracing::trace!(
                tenant_id,
                blob_hash = hash,
                reason = "live_reference",
                "blob protected from GC"
            );
            return Ok((hash.to_owned(), false));
        }
        let pending_deletion_db = PendingBlobDeletionDb::new(UserContext {
            tenant_id: tenant_id.to_string(),
            user_id: tenant_id.to_string(),
        });
        if pending_deletion_db.exists_tx(tx, hash)? {
            tracing::trace!(tenant_id, blob_hash = hash, "blob already pending deletion");
            return Ok((hash.to_owned(), false));
        }
        let metadata = std::fs::metadata(storage::get_storage_path(data_dir, tenant_id, hash));
        let metadata = match metadata {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::trace!(tenant_id, blob_hash = hash, "unmarked blob already absent");
                return Ok((hash.to_owned(), false));
            }
            Err(err) => return Err(anyhow::Error::from(err).context("reading blob metadata")),
        };
        if !metadata.is_file() {
            return Err(anyhow::anyhow!("blob storage entry is not a regular file")
                .context("checking blob type"));
        }
        pending_deletion_db
            .put_tx(
                tx,
                hash,
                PendingBlobDeletionRecord {
                    size_bytes: metadata.len(),
                    marked_at: chrono::Utc::now(),
                },
            )
            .context("persisting blob deletion intent")?;
        Ok((hash.to_owned(), true))
    })()
    .inspect_err(|err| {
        tracing::error!(tenant_id, blob_hash = hash, phase = "marking", error = ?err,
            "failed to mark blob for deletion");
    })
}

fn is_blob_referenced(
    tx: &WriteTransaction,
    file_db: &TenantFileMetaDb,
    file_key_guard: AccessGuard<'_, &[u8]>,
) -> anyhow::Result<bool> {
    let bytes = file_key_guard.value();
    let tenant_path = std::str::from_utf8(bytes)?;
    let (_tenant_id, path) = tenant_path
        .split_once('\0')
        .ok_or_else(|| anyhow::anyhow!("invalid file_meta key"))?;
    let file_meta = file_db.get_tx(tx, path)?;
    match file_meta {
        Some(file_meta) => Ok(!file_meta.is_deleted),
        None => Err(anyhow::anyhow!(
            "blob reference index points to missing file metadata"
        )),
    }
}

fn delete_marked_blobs(data_dir: &Path, db: Arc<Database>, tenant_id: &str) -> Result<GcReport> {
    let ctx = UserContext {
        tenant_id: tenant_id.to_string(),
        user_id: tenant_id.to_string(),
    };
    let pending_deletion_db = PendingBlobDeletionDb::new(ctx.clone());
    let usage_db = TenantUsageDb::new(db.clone(), ctx);
    let pending_deletions = (|| -> Result<_> {
        let tx = db.begin_write()?;
        let pending = pending_deletion_db.list(&tx)?;
        tx.commit()?;
        Ok(pending)
    })()
    .with_context(|| format!("listing pending deletions for tenant {tenant_id}"))?;
    if !pending_deletions.is_empty() {
        tracing::debug!(
            tenant_id,
            pending_blobs = pending_deletions.len(),
            "processing pending blob deletions"
        );
    }
    let mut report = GcReport::default();
    for (hash, pending_deletion) in pending_deletions {
        let removed = (|| -> Result<bool> {
            let tx = db
                .begin_write()
                .context("opening blob deletion transaction")?;
            let removed = storage::remove_if_exists(data_dir, tenant_id, hash.as_str())
                .context("removing pending blob from storage")?;
            pending_deletion_db
                .delete_tx(&tx, hash.as_str())
                .context("removing pending intent")?;
            usage_db
                .decrement_tx(&tx, pending_deletion.size_bytes)
                .context("releasing blob usage charge")?;
            tx.commit().context("committing blob deletion accounting")?;
            Ok(removed)
        })()
        .inspect_err(|err| {
            tracing::error!(tenant_id, blob_hash = hash, phase = "deletion", error = ?err,
                committed_deleted_blobs = report.deleted_blobs,
                committed_already_absent_blobs = report.already_absent_blobs,
                committed_released_bytes = report.released_bytes,
                "failed to complete pending blob deletion");
        })?;
        // Only report completed outcomes after the accounting commit succeeds.
        if removed {
            report.deleted_blobs += 1;
        } else {
            report.already_absent_blobs += 1;
        }
        report.released_bytes += pending_deletion.size_bytes;
        tracing::debug!(
            tenant_id,
            blob_hash = hash,
            removed,
            released_bytes = pending_deletion.size_bytes,
            "blob deletion accounting committed"
        );
    }
    Ok(report)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::Arc;

    use crate::{
        auth::UserContext,
        db::{
            db_file::{INDEX_FILES_BY_TENANT_HASH, TABLE_FILES},
            db_tenants::{TABLE_TENANTS, TenantRecord},
            tx::Transaction,
        },
        service::TenantFileService,
    };
    use cloudsync_common::{InitUploadRequest, hash_bytes};
    use redb::Database;
    use tempfile::TempDir;

    fn test_db() -> (TempDir, Database) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.redb");
        let db = redb::Database::create(&path).unwrap();
        let tx = db.begin_write().unwrap();
        tx.open_table(TABLE_FILES).unwrap();
        tx.open_table(TABLE_TENANTS).unwrap();
        tx.open_multimap_table(INDEX_FILES_BY_TENANT_HASH).unwrap();
        tx.commit().unwrap();
        (dir, db)
    }

    #[test]
    fn garbage_collect_new_database_without_tenants_is_noop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.redb");
        let db = crate::db::db_file::open_db(path.to_str().unwrap()).unwrap();

        let report = garbage_collect_deleted_blobs(dir.path(), Arc::new(db)).unwrap();

        assert_eq!(report, GcReport::default());
    }

    fn create_tenant(
        tenant_id: &str,
        db: Arc<redb::Database>,
        storage_dir: std::path::PathBuf,
    ) -> TenantFileService {
        let user_context = UserContext {
            tenant_id: tenant_id.to_string(),
            user_id: tenant_id.to_string(),
        };
        let tenant_db = TenantDb::new();
        let tx = db.begin_write().unwrap();
        tenant_db
            .create(
                &tx,
                TenantRecord {
                    tenant_id: tenant_id.to_string(),
                },
            )
            .unwrap();
        tx.commit().unwrap();
        TenantFileService::new(
            db.clone(),
            user_context,
            storage_dir.clone(),
            storage_dir.join("staging"),
            1024 * 1024 * 1024, // default_user_quota_bytes
        )
    }
    #[test]
    fn garbage_collect_deletes_unreferenced_blobs() {
        let (storage_dir, db) = test_db();
        let tenant_id = "tenant1";
        let db = Arc::new(db);
        let file_svc = create_tenant(tenant_id, db.clone(), storage_dir.path().to_path_buf());

        // One FM, one blob, deleted -> should be deleted
        let mut fm0 = file_svc.create_file("testpath0", b"helloworld1").unwrap();
        // Two FMS, same blob, both deleted -> should be deleted
        let mut fm1 = file_svc.create_file("testpath1", b"helloworld2").unwrap();
        let mut fm2 = file_svc.create_file("testpath2", b"helloworld2").unwrap();
        // One FM, one blob, not deleted -> should not be deleted
        let fm3 = file_svc.create_file("testpath3", b"helloworld3").unwrap();
        // Two FMS, same blob, one deleted, one not deleted -> should not be deleted
        let mut fm4 = file_svc.create_file("testpath4", b"helloworld4").unwrap();
        let fm5 = file_svc.create_file("testpath5", b"helloworld4").unwrap();
        // Two FMS, same blob, one deleted, one not deleted in reverse order -> should not be deleted
        let fm6 = file_svc.create_file("testpath6", b"helloworld6").unwrap();
        let mut fm7 = file_svc.create_file("testpath7", b"helloworld6").unwrap();

        // One FM, blob already deleted -> GC should not fail
        let mut fm8 = file_svc.create_file("testpath8", b"helloworld8").unwrap();

        // One FM, one blob, deleted, one upload in progress -> should not be deleted
        let mut fm9 = file_svc.create_file("testpath9", b"helloworld9").unwrap();
        file_svc
            .create_upload(InitUploadRequest {
                path: "testpath9".to_string(),
                total_size: 11,
                total_hash: hash_bytes(b"helloworld9"),
                chunk_count: 1,
            })
            .unwrap();

        // usage now at 11*7 bytes

        file_svc.delete_file("testpath0").unwrap();
        fm0.is_deleted = true;
        file_svc.delete_file("testpath1").unwrap();
        fm1.is_deleted = true;
        file_svc.delete_file("testpath2").unwrap();
        fm2.is_deleted = true;
        file_svc.delete_file("testpath4").unwrap();
        fm4.is_deleted = true;
        file_svc.delete_file(&fm7.path).unwrap();
        fm7.is_deleted = true;
        file_svc.delete_file(&fm8.path).unwrap();
        fm8.is_deleted = true;
        storage::remove_if_exists(storage_dir.path(), "tenant1", &fm8.content_hash).unwrap();
        file_svc.delete_file("testpath9").unwrap();
        fm9.is_deleted = true;

        // usage after GC is expected to be at 11*7 - 11*2 = 11*5 = 55 bytes . The fm8 path was deleted not as part of GC and goes undetected.

        let file_metas_delete =
            mark_and_delete_blobs_for_tenant(storage_dir.path(), db.clone(), "tenant1").unwrap();
        let file_metas_delete_2 =
            mark_and_delete_blobs_for_tenant(storage_dir.path(), db.clone(), "tenant1").unwrap();

        assert_eq!(file_metas_delete.newly_marked_blobs, 2);
        assert_eq!(file_metas_delete_2.newly_marked_blobs, 0);

        let expected_files = vec![
            storage::get_storage_path(storage_dir.path(), "tenant1", &fm3.content_hash),
            storage::get_storage_path(storage_dir.path(), "tenant1", &fm5.content_hash),
            storage::get_storage_path(storage_dir.path(), "tenant1", &fm6.content_hash),
            storage::get_storage_path(storage_dir.path(), "tenant1", &fm9.content_hash),
        ];
        for expected_file in expected_files {
            assert!(std::path::Path::new(&expected_file).exists());
        }
        let expected_deleted_files = vec![
            storage::get_storage_path(storage_dir.path(), "tenant1", &fm0.content_hash),
            storage::get_storage_path(storage_dir.path(), "tenant1", &fm1.content_hash),
            storage::get_storage_path(storage_dir.path(), "tenant1", &fm2.content_hash),
        ];
        for expected_deleted_file in expected_deleted_files {
            assert!(!std::path::Path::new(&expected_deleted_file).exists());
        }

        let usage_db = TenantUsageDb::new(
            db.clone(),
            UserContext {
                tenant_id: tenant_id.to_owned(),
                user_id: tenant_id.to_owned(),
            },
        );
        let tx = db.begin_read().unwrap();
        let usage = usage_db.get_tx(&Transaction::Read(&tx)).unwrap().unwrap();
        assert_eq!(usage.usage, 55);
    }

    #[test]
    fn garbage_collect_empty_tenant_does_not_fail() {
        let (storage_dir, db) = test_db();
        let db = Arc::new(db);
        let tenant_db = TenantDb::new();
        let tx = db.begin_write().unwrap();
        tenant_db
            .create(
                &tx,
                TenantRecord {
                    tenant_id: "tenant1".to_string(),
                },
            )
            .unwrap();
        tx.commit().unwrap();

        let file_metas_delete =
            mark_and_delete_blobs_for_tenant(storage_dir.path(), db.clone(), "tenant1").unwrap();

        assert_eq!(file_metas_delete.newly_marked_blobs, 0);
    }

    #[test]
    fn garbage_collection_isolates_tenants() {
        let (storage_dir, db) = test_db();
        let db = Arc::new(db);
        let file_svc1 = create_tenant("tenant1", db.clone(), storage_dir.path().to_path_buf());
        let file_svc2 = create_tenant("tenant2", db.clone(), storage_dir.path().to_path_buf());

        let fm1 = file_svc1
            .create_file("testpath1", b"helloworld1and1")
            .unwrap();
        let fm2 = file_svc2.create_file("testpath2", b"helloworld2").unwrap();

        file_svc1.delete_file("testpath1").unwrap();
        file_svc2.delete_file("testpath2").unwrap();

        // mark tenant2
        mark_blobs_for_garbage_collection(storage_dir.path(), db.clone(), "tenant2").unwrap();
        // but only delete for tenant1 -> should leave tenant2 alone
        let file_metas_delete =
            mark_and_delete_blobs_for_tenant(storage_dir.path(), db.clone(), "tenant1").unwrap();

        assert_eq!(file_metas_delete.newly_marked_blobs, 1);
        let expected_file =
            storage::get_storage_path(storage_dir.path(), "tenant2", &fm2.content_hash);
        assert!(expected_file.exists());
        let expected_deleted_file =
            storage::get_storage_path(storage_dir.path(), "tenant1", &fm1.content_hash);
        assert!(!expected_deleted_file.exists());

        let usage_db1 = TenantUsageDb::new(
            db.clone(),
            UserContext {
                tenant_id: "tenant1".to_owned(),
                user_id: "tenant1".to_owned(),
            },
        );
        let tx = db.begin_read().unwrap();
        let usage = usage_db1.get_tx(&Transaction::Read(&tx)).unwrap().unwrap();
        assert_eq!(usage.usage, 0);

        let usage_db2 = TenantUsageDb::new(
            db.clone(),
            UserContext {
                tenant_id: "tenant2".to_owned(),
                user_id: "tenant2".to_owned(),
            },
        );
        let tx = db.begin_read().unwrap();
        let usage = usage_db2.get_tx(&Transaction::Read(&tx)).unwrap().unwrap();
        assert_eq!(usage.usage, 11);
    }

    #[test]
    fn garbage_collect_isolates_tenants_with_same_content() {
        let (storage_dir, db) = test_db();
        let db = Arc::new(db);
        let file_svc1 = create_tenant("tenant1", db.clone(), storage_dir.path().to_path_buf());
        let file_svc2 = create_tenant("tenant2", db.clone(), storage_dir.path().to_path_buf());

        let fm1 = file_svc1.create_file("testpath1", b"helloworld").unwrap();
        let fm2 = file_svc2.create_file("testpath2", b"helloworld").unwrap();

        file_svc1.delete_file("testpath1").unwrap();
        let file_metas_delete =
            mark_and_delete_blobs_for_tenant(storage_dir.path(), db.clone(), "tenant1").unwrap();

        assert_eq!(file_metas_delete.newly_marked_blobs, 1);
        let expected_file =
            storage::get_storage_path(storage_dir.path(), "tenant2", &fm2.content_hash);
        assert!(expected_file.exists());
        let expected_deleted_file =
            storage::get_storage_path(storage_dir.path(), "tenant1", &fm1.content_hash);
        assert!(!expected_deleted_file.exists());
    }

    fn cleanup_state(db: &Arc<Database>) -> (u64, Vec<(String, PendingBlobDeletionRecord)>) {
        let ctx = UserContext {
            tenant_id: "tenant1".to_owned(),
            user_id: "tenant1".to_owned(),
        };
        let pending_db = PendingBlobDeletionDb::new(ctx.clone());
        let usage_db = TenantUsageDb::new(db.clone(), ctx);
        let tx = db.begin_write().unwrap();
        let usage = usage_db
            .get_tx(&Transaction::Write(&tx))
            .unwrap()
            .unwrap()
            .usage;
        let pending = pending_db.list(&tx).unwrap();
        tx.commit().unwrap();
        (usage, pending)
    }

    #[test]
    fn deletion_recovers_when_file_was_removed_before_accounting_commit() {
        let (dir, db) = test_db();
        let db = Arc::new(db);
        let service = create_tenant("tenant1", db.clone(), dir.path().to_path_buf());
        let meta = service.create_file("file", b"recovery").unwrap();
        service.delete_file("file").unwrap();
        mark_blobs_for_garbage_collection(dir.path(), db.clone(), "tenant1").unwrap();

        let (usage, pending) = cleanup_state(&db);
        assert_eq!(usage, 8);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, meta.content_hash);
        assert_eq!(pending[0].1.size_bytes, 8);
        // Simulate a crash after unlinking, before committing accounting.
        let path = storage::get_storage_path(dir.path(), "tenant1", &meta.content_hash);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(cleanup_state(&db).0, 8);

        delete_marked_blobs(dir.path(), db.clone(), "tenant1").unwrap();
        let (usage, pending) = cleanup_state(&db);
        assert_eq!(usage, 0);
        assert!(pending.is_empty());
        assert!(!path.exists());
        assert!(service.get_file("file").unwrap().is_deleted);

        delete_marked_blobs(dir.path(), db.clone(), "tenant1").unwrap();
        let (usage, pending) = cleanup_state(&db);
        assert_eq!(usage, 0);
        assert!(pending.is_empty());
    }

    #[test]
    fn deletion_failure_preserves_intent_and_usage_until_retry() {
        let (dir, db) = test_db();
        let db = Arc::new(db);
        let service = create_tenant("tenant1", db.clone(), dir.path().to_path_buf());
        let meta = service.create_file("file", b"retry").unwrap();
        service.delete_file("file").unwrap();
        mark_blobs_for_garbage_collection(dir.path(), db.clone(), "tenant1").unwrap();
        let (_, original_pending) = cleanup_state(&db);
        let path = storage::get_storage_path(dir.path(), "tenant1", &meta.content_hash);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();

        let err = delete_marked_blobs(dir.path(), db.clone(), "tenant1").unwrap_err();
        let io_error = err.downcast_ref::<std::io::Error>().unwrap();
        assert_ne!(io_error.kind(), std::io::ErrorKind::NotFound);
        let (usage, pending) = cleanup_state(&db);
        assert_eq!(usage, 5);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, meta.content_hash);
        assert_eq!(pending[0].1.size_bytes, original_pending[0].1.size_bytes);
        assert_eq!(pending[0].1.marked_at, original_pending[0].1.marked_at);
        assert!(path.is_dir());
        assert!(service.get_file("file").unwrap().is_deleted);

        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, b"retry").unwrap();
        delete_marked_blobs(dir.path(), db.clone(), "tenant1").unwrap();
        let (usage, pending) = cleanup_state(&db);
        assert_eq!(usage, 0);
        assert!(pending.is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn partial_deletion_failure_keeps_prior_commit_and_retries_remaining_intent() {
        let (dir, db) = test_db();
        let db = Arc::new(db);
        let service = create_tenant("tenant1", db.clone(), dir.path().to_path_buf());
        let first = service.create_file("first", b"first content").unwrap();
        let second = service.create_file("second", b"second content").unwrap();
        service.delete_file("first").unwrap();
        service.delete_file("second").unwrap();
        mark_blobs_for_garbage_collection(dir.path(), db.clone(), "tenant1").unwrap();
        let (initial_usage, pending) = cleanup_state(&db);
        assert_eq!(initial_usage, first.size + second.size);
        assert_eq!(pending.len(), 2);
        // Pending records are processed in key order, not insertion order.
        let (first_hash, first_intent) = &pending[0];
        let (failed_hash, failed_intent) = &pending[1];
        let first_path = storage::get_storage_path(dir.path(), "tenant1", first_hash);
        let failed_path = storage::get_storage_path(dir.path(), "tenant1", failed_hash);
        let failed_content = std::fs::read(&failed_path).unwrap();
        std::fs::remove_file(&failed_path).unwrap();
        std::fs::create_dir(&failed_path).unwrap();

        assert!(delete_marked_blobs(dir.path(), db.clone(), "tenant1").is_err());
        let (usage, remaining) = cleanup_state(&db);
        assert!(!first_path.exists());
        assert!(failed_path.is_dir());
        assert_eq!(usage, initial_usage - first_intent.size_bytes);
        assert_eq!(remaining.len(), 1);
        assert_eq!(&remaining[0].0, failed_hash);
        assert_eq!(remaining[0].1.size_bytes, failed_intent.size_bytes);
        assert_eq!(remaining[0].1.marked_at, failed_intent.marked_at);

        std::fs::remove_dir(&failed_path).unwrap();
        std::fs::write(&failed_path, failed_content).unwrap();
        delete_marked_blobs(dir.path(), db.clone(), "tenant1").unwrap();
        let (usage, remaining) = cleanup_state(&db);
        assert_eq!(usage, 0);
        assert!(remaining.is_empty());
        assert!(!failed_path.exists());
        assert!(service.get_file("first").unwrap().is_deleted);
        assert!(service.get_file("second").unwrap().is_deleted);

        delete_marked_blobs(dir.path(), db.clone(), "tenant1").unwrap();
        assert_eq!(cleanup_state(&db).0, 0);
    }

    #[test]
    fn upload_retries_after_pending_deletion_recreates_and_charges_blob_once() {
        let (dir, db) = test_db();
        let db = Arc::new(db);
        let service = create_tenant("tenant1", db.clone(), dir.path().to_path_buf());
        let content = b"upload retry";
        let meta = service.create_file("old", content).unwrap();
        service.delete_file("old").unwrap();
        mark_blobs_for_garbage_collection(dir.path(), db.clone(), "tenant1").unwrap();
        let path = storage::get_storage_path(dir.path(), "tenant1", &meta.content_hash);

        assert!(matches!(
            service.create_file("new", content),
            Err(crate::service::ServiceError::Conflict(_))
        ));
        assert!(matches!(
            service.get_file("new"),
            Err(crate::service::ServiceError::NotFound)
        ));
        let (usage, pending) = cleanup_state(&db);
        assert_eq!(usage, content.len() as u64);
        assert_eq!(pending.len(), 1);
        assert!(path.is_file());

        delete_marked_blobs(dir.path(), db.clone(), "tenant1").unwrap();
        let (usage, pending) = cleanup_state(&db);
        assert_eq!(usage, 0);
        assert!(pending.is_empty());
        assert!(!path.exists());

        let uploaded = service.create_file("new", content).unwrap();
        assert!(!uploaded.is_deleted);
        assert_eq!(uploaded.content_hash, meta.content_hash);
        assert_eq!(std::fs::read(&path).unwrap(), content);
        service.create_file("duplicate", content).unwrap();
        let (usage, pending) = cleanup_state(&db);
        assert_eq!(usage, content.len() as u64);
        assert!(pending.is_empty());

        mark_and_delete_blobs_for_tenant(dir.path(), db.clone(), "tenant1").unwrap();
        assert!(path.is_file());
        assert_eq!(cleanup_state(&db).0, content.len() as u64);
        assert!(service.get_file("old").unwrap().is_deleted);
    }

    #[test]
    fn gc_report_counts_new_marks_removals_and_recovered_accounting_separately() {
        let (dir, db) = test_db();
        let db = Arc::new(db);
        let service = create_tenant("tenant1", db.clone(), dir.path().to_path_buf());
        service.create_file("first", b"abc").unwrap();
        service.create_file("shared", b"abc").unwrap();
        service.delete_file("first").unwrap();
        service.delete_file("shared").unwrap();

        let report = garbage_collect_deleted_blobs(dir.path(), db.clone()).unwrap();
        assert_eq!(
            report,
            GcReport {
                newly_marked_blobs: 1,
                deleted_blobs: 1,
                already_absent_blobs: 0,
                released_bytes: 3,
            }
        );

        let meta = service.create_file("recovered", b"recover").unwrap();
        service.delete_file("recovered").unwrap();
        mark_blobs_for_garbage_collection(dir.path(), db.clone(), "tenant1").unwrap();
        std::fs::remove_file(storage::get_storage_path(
            dir.path(),
            "tenant1",
            &meta.content_hash,
        ))
        .unwrap();
        let report = garbage_collect_deleted_blobs(dir.path(), db.clone()).unwrap();
        assert_eq!(
            report,
            GcReport {
                newly_marked_blobs: 0,
                deleted_blobs: 0,
                already_absent_blobs: 1,
                released_bytes: 7,
            }
        );
        assert_eq!(
            garbage_collect_deleted_blobs(dir.path(), db).unwrap(),
            GcReport::default()
        );
    }

    #[test]
    fn garbage_collect_propagates_errors_other_than_not_found() {
        let (storage_dir, db) = test_db();
        let db = Arc::new(db);
        let file_svc1 = create_tenant("tenant1", db.clone(), storage_dir.path().to_path_buf());
        let fm1 = file_svc1.create_file("testpath1", b"helloworld").unwrap();
        file_svc1.delete_file(&fm1.path).unwrap();

        let path = storage::get_storage_path(storage_dir.path(), "tenant1", &fm1.content_hash);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();

        let res = mark_and_delete_blobs_for_tenant(storage_dir.path(), db.clone(), "tenant1");

        assert!(res.is_err());
        assert!(path.is_dir());
    }
}
