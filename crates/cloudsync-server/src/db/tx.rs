use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

/// Shared read access. Mutations require a concrete `&WriteTransaction`.
pub enum Transaction<'tx> {
    Read(&'tx redb::ReadTransaction),
    Write(&'tx redb::WriteTransaction),
}

/// Run synchronous reads against one snapshot. Repository results are owned records.
///
/// An owned record can leave a transaction. The hidden setup includes the actual
/// private modules so examples check their types without changing visibility.
/// ```no_run
/// # fn main() {}
/// # #[allow(dead_code)]
/// # mod db {
/// #     pub mod tx { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/tx.rs")); }
/// #     pub mod db_usage { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/db_usage.rs")); }
/// # }
/// use db::{db_usage, tx::{Transaction, with_read_transaction, with_write_transaction}};
/// fn owned_records(db: &redb::Database) -> anyhow::Result<()> {
///     let written = with_write_transaction(db, |tx| db_usage::put_tx(tx, "tenant", 1))?;
///     let read = with_read_transaction(db, |tx| {
///         db_usage::get_tx(&Transaction::Read(tx), "tenant")
///     })?;
///     assert_eq!(read.unwrap().usage, written.usage);
///     Ok(())
/// }
/// ```
///
/// The transaction itself cannot escape:
/// ```compile_fail
/// # fn main() {}
/// # #[allow(dead_code)]
/// # mod db {
/// #     pub mod tx { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/tx.rs")); }
/// #     pub mod db_usage { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/db_usage.rs")); }
/// # }
/// use db::tx::with_read_transaction;
/// fn escape(db: &redb::Database) {
///     let _ = with_read_transaction(db, |tx| Ok::<_, anyhow::Error>(tx));
/// }
/// ```
/// Reads cannot be used for writes:
/// ```compile_fail
/// # fn main() {}
/// # #[allow(dead_code)]
/// # mod db {
/// #     pub mod tx { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/tx.rs")); }
/// #     pub mod db_usage { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/db_usage.rs")); }
/// # }
/// use db::{db_usage, tx::with_read_transaction};
/// fn mutate(db: &redb::Database) {
///     let _ = with_read_transaction(db, |tx| db_usage::put_tx(tx, "tenant", 1));
/// }
/// ```
/// Borrowed guards cannot escape the closure; do not return redb snapshot handles.
pub fn with_read_transaction<T, E>(
    db: &redb::Database,
    f: impl FnOnce(&redb::ReadTransaction) -> Result<T, E>,
) -> Result<T, E>
where
    E: From<anyhow::Error>,
{
    let tx = db.begin_read().map_err(anyhow::Error::from)?;
    f(&tx)
}

/// Commit once on success; abort on error or unwind. Never retries the closure.
///
/// A table borrowed from the writer cannot escape:
/// ```compile_fail
/// # fn main() {}
/// # #[allow(dead_code)]
/// # mod db {
/// #     pub mod tx { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/tx.rs")); }
/// #     pub mod db_usage { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/db_usage.rs")); }
/// # }
/// use db::{db_usage::TABLE_USAGES, tx::with_write_transaction};
/// fn escape(db: &redb::Database) {
///     let _ = with_write_transaction(db, |tx| -> anyhow::Result<_> {
///         Ok(tx.open_table(TABLE_USAGES)?)
///     });
/// }
/// ```
///
/// Keep closures synchronous and return owned repository results. Do not open
/// nested writers or invoke transaction-owning helpers inside the closure.
/// Filesystem effects are not rolled back: callers retain responsibility for
/// publication/recovery ordering and writer duration. Use explicit transactions
/// when successful aborts or commit-before-error behavior are required.
pub fn with_write_transaction<T, E>(
    db: &redb::Database,
    f: impl FnOnce(&redb::WriteTransaction) -> Result<T, E>,
) -> Result<T, E>
where
    E: From<anyhow::Error>,
{
    let tx = db.begin_write().map_err(anyhow::Error::from)?;
    // redb skips ordinary abort cleanup while the thread is panicking. Catch
    // only to abort outside unwinding, then resume the original panic.
    match catch_unwind(AssertUnwindSafe(|| f(&tx))) {
        Ok(Ok(value)) => {
            tx.commit().map_err(anyhow::Error::from)?;
            Ok(value)
        }
        Ok(Err(error)) => {
            if let Err(abort_error) = tx.abort() {
                tracing::error!(error = ?abort_error, "transaction abort failed");
            }
            Err(error)
        }
        Err(panic) => {
            if let Err(abort_error) = tx.abort() {
                tracing::error!(error = ?abort_error, "transaction abort failed after unwind");
            }
            resume_unwind(panic)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::db::{
        db_file as files, db_pending_blob_deletions as pending, db_tenants as tenants,
        db_upload as uploads, db_usage as usage,
    };
    use cloudsync_common::{InitUploadRequest, Upload};
    use redb::Database;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tempfile::TempDir;

    fn test_db() -> (TempDir, Database) {
        let dir = TempDir::new().unwrap();
        let db = files::open_db(dir.path().join("test.redb").to_str().unwrap()).unwrap();
        with_write_transaction(&db, |tx| -> anyhow::Result<()> {
            tx.open_table(usage::TABLE_USAGES)?;
            tx.open_table(pending::TABLE_PENDING_DELETIONS)?;
            Ok(())
        })
        .unwrap();
        (dir, db)
    }

    fn seed(tx: &redb::WriteTransaction, tenant_id: &str) -> anyhow::Result<Upload> {
        tenants::create_tx(
            tx,
            tenants::TenantRecord {
                tenant_id: tenant_id.into(),
            },
        )?;
        files::put_tx(tx, tenant_id, "author", "same-path", 3, "same-hash")?;
        usage::put_tx(tx, tenant_id, 3)?;
        pending::put_tx(
            tx,
            tenant_id,
            "unused-hash",
            pending::PendingBlobDeletionRecord {
                size_bytes: 2,
                marked_at: chrono::Utc::now(),
            },
        )?;
        uploads::create_tx(
            tx,
            tenant_id,
            "uploader",
            InitUploadRequest {
                path: "same-path".into(),
                total_size: 3,
                total_hash: "same-hash".into(),
                chunk_count: 1,
            },
        )
    }

    fn assert_present(tx: &Transaction, tenant_id: &str, upload: &Upload) -> anyhow::Result<()> {
        assert!(tenants::exists_tx(tx, tenant_id)?);
        let file = files::get_tx(tx, tenant_id, "same-path")?.unwrap();
        assert_eq!(file.user_id, "author");
        assert_eq!(file.version, 1);
        assert_eq!(files::list_tx(tx, tenant_id)?.len(), 1);
        assert_eq!(usage::get_tx(tx, tenant_id)?.unwrap().usage, 3);
        assert!(pending::exists_tx(tx, tenant_id, "unused-hash")?);
        assert_eq!(pending::list_tx(tx, tenant_id)?.len(), 1);
        assert_eq!(
            uploads::get_tx(tx, tenant_id, &upload.upload_id)?
                .unwrap()
                .user_id,
            "uploader"
        );
        assert_eq!(uploads::list_tx(tx, tenant_id)?.len(), 1);
        Ok(())
    }

    #[test]
    fn cross_repository_commit_read_your_writes_and_tenant_isolation() {
        let (_dir, db) = test_db();
        let (a, b) = with_write_transaction(&db, |tx| -> anyhow::Result<_> {
            let a = seed(tx, "a")?;
            let b = seed(tx, "b")?;
            let read = Transaction::Write(tx);
            assert_present(&read, "a", &a)?;
            assert_present(&read, "b", &b)?;
            assert_eq!(tenants::count_tx(&read)?, 2);
            assert_eq!(tenants::list_tx(&read)?.len(), 2);
            assert!(uploads::get_tx(&read, "b", &a.upload_id)?.is_none());
            assert!(uploads::delete_tx(tx, "b", &a.upload_id).is_err());
            assert!(uploads::add_chunk_tx(tx, "b", &a.upload_id, 0).is_err());
            Ok((a, b))
        })
        .unwrap();
        with_read_transaction(&db, |tx| -> anyhow::Result<()> {
            assert_present(&Transaction::Read(tx), "a", &a)?;
            assert_present(&Transaction::Read(tx), "b", &b)?;
            Ok(())
        })
        .unwrap();
        with_write_transaction(&db, |tx| -> anyhow::Result<()> {
            files::delete_tx(tx, "a", "same-path")?;
            pending::delete_tx(tx, "a", "unused-hash")?;
            usage::decrement_tx(tx, "a", 2)?;
            uploads::delete_tx(tx, "a", &a.upload_id)?;
            assert_present(&Transaction::Write(tx), "b", &b)?;
            Ok(())
        })
        .unwrap();
        with_read_transaction(&db, |tx| -> anyhow::Result<()> {
            let read = Transaction::Read(tx);
            assert!(files::get_tx(&read, "a", "same-path")?.unwrap().is_deleted);
            assert!(files::list_tx(&read, "a")?.is_empty());
            assert!(!pending::exists_tx(&read, "a", "unused-hash")?);
            assert!(uploads::get_tx(&read, "a", &a.upload_id)?.is_none());
            assert_eq!(usage::get_tx(&read, "a")?.unwrap().usage, 1);
            assert_present(&read, "b", &b)
        })
        .unwrap();
    }

    fn assert_empty(db: &Database) {
        with_read_transaction(db, |tx| -> anyhow::Result<()> {
            let read = Transaction::Read(tx);
            assert!(files::get_tx(&read, "a", "same-path")?.is_none());
            assert!(files::list_tx(&read, "a")?.is_empty());
            assert!(usage::get_tx(&read, "a")?.is_none());
            assert!(pending::list_tx(&read, "a")?.is_empty());
            assert!(uploads::list_tx(&read, "a")?.is_empty());
            assert_eq!(tenants::count_tx(&read)?, 0);
            let index = tx.open_multimap_table(files::INDEX_FILES_BY_TENANT_HASH)?;
            assert_eq!(
                index
                    .get(files::make_index_key("a", "same-hash").as_str())?
                    .len(),
                0
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn closure_error_rolls_back_every_repository_and_preserves_error() {
        let (_dir, db) = test_db();
        let error = with_write_transaction(&db, |tx| -> Result<(), crate::service::ServiceError> {
            seed(tx, "a")?;
            Err(crate::service::ServiceError::QuotaExceeded)
        })
        .unwrap_err();
        assert!(matches!(error, crate::service::ServiceError::QuotaExceeded));
        assert_empty(&db);
        // A later writer can proceed after the abort.
        with_write_transaction(&db, |tx| seed(tx, "a")).unwrap();
    }

    #[test]
    fn read_helper_retains_one_snapshot_across_repository_changes() {
        let (_dir, db) = test_db();
        with_read_transaction(&db, |snapshot| -> anyhow::Result<()> {
            let upload = with_write_transaction(&db, |tx| seed(tx, "a"))?;
            let read = Transaction::Read(snapshot);
            assert!(files::list_tx(&read, "a")?.is_empty());
            assert!(usage::get_tx(&read, "a")?.is_none());
            assert!(pending::list_tx(&read, "a")?.is_empty());
            assert!(uploads::get_tx(&read, "a", &upload.upload_id)?.is_none());
            assert_eq!(tenants::count_tx(&read)?, 0);
            with_read_transaction(&db, |fresh| {
                assert_present(&Transaction::Read(fresh), "a", &upload)
            })?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn unwind_aborts_every_repository_and_resumes_original_panic() {
        let (_dir, db) = test_db();
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _: anyhow::Result<()> = with_write_transaction(&db, |tx| {
                seed(tx, "a")?;
                std::panic::panic_any(62_u32);
            });
        }))
        .unwrap_err();
        assert_eq!(*panic.downcast::<u32>().unwrap(), 62);
        assert_empty(&db);
        with_write_transaction(&db, |tx| seed(tx, "a")).unwrap();
    }

    #[derive(Debug)]
    struct FailSync {
        inner: redb::backends::FileBackend,
        fail: Arc<AtomicBool>,
    }
    impl redb::StorageBackend for FailSync {
        fn len(&self) -> std::io::Result<u64> {
            self.inner.len()
        }
        fn read(&self, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
            self.inner.read(offset, len)
        }
        fn set_len(&self, len: u64) -> std::io::Result<()> {
            self.inner.set_len(len)
        }
        fn write(&self, offset: u64, data: &[u8]) -> std::io::Result<()> {
            self.inner.write(offset, data)
        }
        fn sync_data(&self, eventual: bool) -> std::io::Result<()> {
            if self.fail.load(Ordering::SeqCst) {
                Err(std::io::Error::other("test commit sync failure"))
            } else {
                self.inner.sync_data(eventual)
            }
        }
    }

    #[test]
    fn commit_error_is_propagated_after_successful_closure_without_retry() {
        let file = tempfile::tempfile().unwrap();
        let fail = Arc::new(AtomicBool::new(false));
        let db = Database::builder()
            .create_with_backend(FailSync {
                inner: redb::backends::FileBackend::new(file).unwrap(),
                fail: fail.clone(),
            })
            .unwrap();
        let mut calls = 0;
        let error = with_write_transaction(&db, |tx| -> anyhow::Result<()> {
            calls += 1;
            usage::put_tx(tx, "a", 3)?;
            // Fail only after the operation has succeeded, forcing the helper's commit to fail.
            fail.store(true, Ordering::SeqCst);
            Ok(())
        })
        .unwrap_err();
        assert!(error.downcast_ref::<redb::CommitError>().is_some());
        assert_eq!(calls, 1);
    }
}
