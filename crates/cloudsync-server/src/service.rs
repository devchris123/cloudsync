use crate::{
    auth::UserContext,
    db::TenantDb,
    db_upload::TenantUploadDb,
    db_user::{self, TenantUserDb},
    storage,
};
use cloudsync_common::{FileMeta, InitUploadRequest, hash_bytes};
use redb::{self};
use std::sync::Arc;

#[derive(Debug)]
pub enum ServiceError {
    //// The operation would exceed the tenant's storage quota.
    QuotaExceeded,

    //// The requested file or upload was not found.
    NotFound,

    //// The request was invalid, e.g., missing required fields or violating constraints.
    Validation(String),

    //// An error occurred while interacting with the storage backend.
    Storage(storage::StorageError),

    //// The content hash of the uploaded file does not match the expected hash.
    HashMismatch(storage::StorageError),

    //// An unexpected internal error occurred.
    Internal(anyhow::Error),
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceError::QuotaExceeded => write!(f, "quota exceeded"),
            ServiceError::NotFound => write!(f, "not found"),
            ServiceError::Validation(msg) => write!(f, "validation error: {}", msg),
            ServiceError::Storage(err) => write!(f, "storage error: {}", err),
            ServiceError::HashMismatch(err) => write!(f, "hash mismatch: {}", err),
            ServiceError::Internal(err) => write!(f, "internal error: {}", err),
        }
    }
}

impl From<anyhow::Error> for ServiceError {
    fn from(err: anyhow::Error) -> Self {
        ServiceError::Internal(err)
    }
}

impl From<redb::CommitError> for ServiceError {
    fn from(err: redb::CommitError) -> Self {
        ServiceError::Internal(err.into())
    }
}

impl From<std::io::Error> for ServiceError {
    fn from(err: std::io::Error) -> Self {
        ServiceError::Internal(err.into())
    }
}

impl From<storage::StorageError> for ServiceError {
    fn from(err: storage::StorageError) -> Self {
        match err {
            storage::StorageError::HashMismatch => ServiceError::HashMismatch(err),
            _ => ServiceError::Storage(err),
        }
    }
}

/// File operations scoped to one authenticated user context.
/// Construct a separate service for each request; the database remains shared.
pub struct TenantFileService {
    db: Arc<redb::Database>,
    ctx: UserContext,
    storage_dir: std::path::PathBuf,
    staging_dir: std::path::PathBuf,
    default_user_quota_bytes: u64,
}

impl TenantFileService {
    pub fn new(
        db: Arc<redb::Database>,
        ctx: UserContext,
        storage_dir: std::path::PathBuf,
        staging_dir: std::path::PathBuf,
        default_user_quota_bytes: u64,
    ) -> Self {
        Self {
            db,
            ctx,
            storage_dir,
            staging_dir,
            default_user_quota_bytes,
        }
    }

    /// Get the metadata for a file at the given path.
    ///
    /// # Errors
    /// Returns `ServiceError::NotFound` if the file does not exist.
    pub fn get_file(&self, path: &str) -> Result<FileMeta, ServiceError> {
        TenantDb::new(self.db.clone(), self.ctx.clone())
            .get(path)?
            .ok_or(ServiceError::NotFound)
    }

    /// List all files for the current tenant.
    pub fn list_files(&self) -> Result<Vec<FileMeta>, ServiceError> {
        Ok(TenantDb::new(self.db.clone(), self.ctx.clone()).list()?)
    }

    /// Marks a file as deleted without removing its stored blob.
    ///
    /// Succeeds if the path does not exist or is already deleted.
    /// Stored bytes remain charged against the tenant's quota.
    pub fn delete_file(&self, path: &str) -> Result<(), ServiceError> {
        TenantDb::new(self.db.clone(), self.ctx.clone()).delete(path)?;
        tracing::info!("file marked as deleted: {}", path);
        Ok(())
    }

    /// Creates a new file with the given path and content, or replaces an existing file.
    ///
    /// # Errors
    /// Returns `ServiceError::QuotaExceeded` if the operation would exceed the tenant's storage quota.
    pub fn create_file(&self, path: String, content: &[u8]) -> Result<FileMeta, ServiceError> {
        let usage_db = db_user::TenantUserDb::new(self.db.clone(), self.ctx.clone());
        let tx = usage_db.start_write_tx()?;
        let usage = self.get_usage(&tx)?;

        let db_file = TenantDb::new(self.db.clone(), self.ctx.clone());

        let mut content_hash = hash_bytes(content);
        let content_size = content.len() as u64;
        let exist = self.check_quota(&tx, &content_hash, content_size, usage)?;

        if !exist {
            usage_db.put_tx(&tx, usage + content_size)?;
            content_hash = storage::write(&self.storage_dir, &self.ctx.tenant_id, content)?;
        }

        tracing::info!("file stored: {} (hash: {})", path, content_hash);
        let file_meta = db_file.put_tx(&tx, &path, content_size, &content_hash)?;
        tracing::info!("metadata saved: {} (version: {})", path, file_meta.version);
        tx.commit()?;

        Ok(file_meta)
    }

    /// Creates a new upload session for chunked uploads.
    ///
    /// A staging directory is created for the upload session,
    /// and the tenant's storage quota is checked against the total size of the upload.
    /// To complete the upload, all chunks must be uploaded and then `finalize_upload` must be called.
    /// The upload session is deleted after finalization, and the staging directory is cleaned up.
    ///
    /// # Errors
    /// Returns `ServiceError::QuotaExceeded` if the operation would exceed the tenant's storage quota.
    /// Returns `ServiceError::Validation` if the request is invalid.
    pub fn create_upload(&self, request: InitUploadRequest) -> Result<String, ServiceError> {
        let usage_db = TenantUserDb::new(self.db.clone(), self.ctx.clone());
        let tx = usage_db.start_write_tx()?;
        let usage = self.get_usage(&tx)?;

        self.check_quota(&tx, &request.total_hash, request.total_size, usage)?;

        let scoped_db = TenantUploadDb::new(self.db.clone(), self.ctx.clone());
        let upload = scoped_db.create_tx(&tx, request)?;
        tx.commit()?;
        let upload_staging_dir = self.staging_dir.join(&upload.upload_id);
        std::fs::create_dir_all(upload_staging_dir)?;
        Ok(upload.upload_id)
    }

    fn get_usage(&self, tx: &redb::WriteTransaction) -> anyhow::Result<u64> {
        let usage_db = TenantUserDb::new(self.db.clone(), self.ctx.clone());
        let usage = usage_db.get_write_tx(tx)?;
        let usage = match usage {
            Some(usage) => usage.usage,
            None => {
                tracing::info!("initialize storage quota for tenant {}", self.ctx.tenant_id);
                usage_db.put_tx(tx, 0)?.usage
            }
        };
        Ok(usage)
    }

    fn check_quota(
        &self,
        tx: &redb::WriteTransaction,
        content_hash: &str,
        content_size: u64,
        usage: u64,
    ) -> Result<bool, ServiceError> {
        let exist = storage::exist_storage_path(
            self.storage_dir.as_path(),
            &self.ctx.tenant_id,
            content_hash,
        );

        if exist {
            return Ok(exist);
        }

        // Check uploads for reserved usage
        let db_upload = TenantUploadDb::new(self.db.clone(), self.ctx.clone());
        let upload_usage: u64 = db_upload
            .list_tx(tx)?
            .iter()
            .map(|u| u.total_size)
            .try_fold(0_u64, |sum, size| -> Result<u64, ServiceError> {
                sum.checked_add(size).ok_or(ServiceError::QuotaExceeded)
            })?;

        let total = usage
            .checked_add(content_size)
            .and_then(|sum| sum.checked_add(upload_usage))
            .ok_or(ServiceError::QuotaExceeded)?;
        if self.default_user_quota_bytes < total {
            tracing::info!("storage quota exceeded for tenant {}", self.ctx.tenant_id);
            return Err(ServiceError::QuotaExceeded);
        }

        Ok(exist)
    }

    /// Replaces a chunk of an existing upload session.
    ///
    /// # Errors
    /// Returns `ServiceError::NotFound` if the upload session does not exist.
    /// Returns `ServiceError::Validation` if the index is out of bounds.
    pub fn replace_chunk(
        &self,
        upload_id: &str,
        index: u64,
        body: &[u8],
    ) -> Result<(), ServiceError> {
        let scoped_db = TenantUploadDb::new(self.db.clone(), self.ctx.clone());
        let upload = scoped_db.get(&upload_id)?;

        let Some(upload) = upload else {
            return Err(ServiceError::NotFound);
        };

        if index >= upload.chunk_count {
            return Err(ServiceError::Validation(
                "index larger than upload chunk_count".to_string(),
            ));
        }

        if index >= upload.chunk_count {
            return Err(ServiceError::Validation(
                "index larger than upload chunk_count".to_string(),
            ));
        }
        let upload_staging_dir = self.staging_dir.join(&upload_id);
        let chunk_path = upload_staging_dir.join(index.to_string());
        std::fs::write(chunk_path, body)?;
        scoped_db.add_chunk(&upload_id, index)?;
        Ok(())
    }

    /// Finalizes an upload session, reassembling the chunks into a complete file and storing it.
    ///
    /// The upload session is deleted after finalization, and the staging directory is cleaned up.
    ///
    /// # Errors
    /// Returns `ServiceError::NotFound` if the upload session does not exist.
    /// Returns `ServiceError::Validation` if not all chunks have been uploaded.
    /// Returns `ServiceError::QuotaExceeded` if the operation would exceed the tenant's storage quota.
    /// Returns `ServiceError::HashMismatch` if the reassembled file's hash does not match the expected hash.
    pub fn finalize_upload(&self, upload_id: &str) -> Result<FileMeta, ServiceError> {
        let upload_db: TenantUploadDb = TenantUploadDb::new(self.db.clone(), self.ctx.clone());
        let usage_db = TenantUserDb::new(self.db.clone(), self.ctx.clone());

        let tx = usage_db.start_write_tx()?;

        let upload = upload_db.get_write_tx(&tx, upload_id)?;

        let Some(upload) = upload else {
            tx.commit()?;
            return Err(ServiceError::NotFound);
        };
        if upload.chunks_received.len() != upload.chunk_count as usize {
            tx.commit()?;
            return Err(ServiceError::Validation(format!(
                "{} / {} chunks received, must receive upload all chunks before finalizing",
                upload.chunks_received.len(),
                upload.chunk_count
            )));
        }

        let upload_staging_dir = self.staging_dir.join(&upload_id);

        let storage_path = storage::get_storage_path(
            self.storage_dir.as_path(),
            self.ctx.tenant_id.as_str(),
            &upload.total_hash,
        );
        if !storage_path.exists() {
            let (_storage_path, seen_bytes) = storage::reassemble_chunks(
                self.storage_dir.as_path(),
                self.ctx.tenant_id.as_str(),
                &upload_staging_dir,
                upload.chunk_count,
                &upload.total_hash,
                upload.total_size,
            )?;
            let usage = usage_db.get_write_tx(&tx)?.unwrap();
            usage_db.put_tx(&tx, usage.usage + seen_bytes)?;
        } else {
            let meta = std::fs::metadata(storage_path)?;
            let size = meta.len();
            if size != upload.total_size {
                return Err(ServiceError::Validation(format!(
                    "upload total_size must match content size"
                )));
            }
        }

        // `db.put_tx` overwrites any prior row at this path — including a soft-deleted
        // one — bumping `version` and resetting `is_deleted`, so the re-upload of a
        // previously-deleted file lands as a fresh row without special-casing here.
        let scoped_db = TenantDb::new(self.db.clone(), self.ctx.clone());
        let file = scoped_db.put_tx(&tx, &upload.path, upload.total_size, &upload.total_hash)?;
        upload_db.delete_tx(&tx, &upload_id)?;
        tx.commit()?;
        if let Err(err) = std::fs::remove_dir_all(upload_staging_dir) {
            tracing::warn!(upload_id, error= %err, "upload finalized but staging cleanup failed");
        }
        Ok(file)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::auth::UserContext;
    use crate::db_user::TABLE_USAGES;
    use crate::service::ServiceError::QuotaExceeded;
    use redb::Database;
    use tempfile::TempDir;

    fn test_service(quota: u64) -> (TempDir, Arc<Database>, TenantFileService) {
        let dir = TempDir::new().unwrap();
        let db =
            Arc::new(crate::db::open_db(dir.path().join("test.redb").to_str().unwrap()).unwrap());
        let tx = db.begin_write().unwrap();
        tx.open_table(TABLE_USAGES).unwrap();
        tx.commit().unwrap();
        let service = TenantFileService::new(
            db.clone(),
            UserContext {
                tenant_id: "tenantid".to_string(),
                user_id: "userid".to_string(),
            },
            dir.path().join("storage"),
            dir.path().join("staging"),
            quota,
        );
        (dir, db, service)
    }

    fn finalize_state(db: &Arc<Database>, ctx: &UserContext, upload_id: &str) -> serde_json::Value {
        let usage_db = TenantUserDb::new(db.clone(), ctx.clone());
        let tx = usage_db.start_read_tx().unwrap();
        serde_json::json!({
            "usage": usage_db.get_read_tx(&tx).unwrap(),
            "files": TenantDb::new(db.clone(), ctx.clone()).list().unwrap(),
            "upload": TenantUploadDb::new(db.clone(), ctx.clone()).get(upload_id).unwrap(),
        })
    }

    #[test]
    fn test_get_file() {
        let (_dir, _db, service) = test_service(10);
        assert!(matches!(
            service.get_file("missing"),
            Err(ServiceError::NotFound)
        ));

        let created = service.create_file("file".into(), b"abc").unwrap();
        let found = service.get_file("file").unwrap();
        assert_eq!(
            serde_json::to_value(found).unwrap(),
            serde_json::to_value(created).unwrap()
        );
    }

    #[test]
    fn test_list_files_isolates_tenants() {
        let (_dir, db, service) = test_service(10);
        assert!(service.list_files().unwrap().is_empty());
        service.create_file("file".into(), b"abc").unwrap();

        let other = TenantFileService::new(
            db,
            UserContext {
                tenant_id: "other-tenant".into(),
                user_id: "other-user".into(),
            },
            service.storage_dir.clone(),
            service.staging_dir.clone(),
            10,
        );
        assert!(other.list_files().unwrap().is_empty());
        assert!(matches!(
            other.get_file("file"),
            Err(ServiceError::NotFound)
        ));
        other.create_file("other-file".into(), b"def").unwrap();

        let files = service.list_files().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "file");
        let files = other.list_files().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "other-file");
    }

    #[test]
    fn test_delete_file_soft_deletes_and_is_idempotent() {
        let (_dir, _db, service) = test_service(10);
        service.create_file("file".into(), b"abc").unwrap();

        service.delete_file("file").unwrap();
        assert!(service.list_files().unwrap().is_empty());
        // Preserve the existing metadata lookup behavior for soft-deleted files.
        let deleted = service.get_file("file").unwrap();
        assert!(deleted.is_deleted);
        let blob = storage::get_storage_path(
            &service.storage_dir,
            &service.ctx.tenant_id,
            &deleted.content_hash,
        );
        assert_eq!(std::fs::read(blob).unwrap(), b"abc");

        service.delete_file("file").unwrap();
        service.delete_file("missing").unwrap();
        assert!(service.list_files().unwrap().is_empty());
    }

    #[test]
    fn test_check_quota_rejects_arithmetic_overflow() {
        // Cover overflow both while summing reservations and while adding the
        // new file's size to an otherwise representable reservation total.
        for reservation_sizes in [vec![u64::MAX, 1], vec![u64::MAX]] {
            let (_dir, db, service) = test_service(u64::MAX);
            let ctx = service.ctx.clone();

            let upload_db = TenantUploadDb::new(db.clone(), ctx.clone());

            // Seed extreme reservations directly to exercise the service's
            // arithmetic independently of HTTP input-size validation.
            for total_size in reservation_sizes {
                upload_db
                    .create(InitUploadRequest {
                        path: "pending".to_string(),
                        total_size,
                        total_hash: hash_bytes(b"pending"),
                        chunk_count: 1,
                    })
                    .unwrap();
            }

            let tx = db.begin_write().unwrap();

            let result = service.check_quota(&tx, &hash_bytes(b"x"), 1, 0);

            assert!(matches!(result, Err(ServiceError::QuotaExceeded)));
        }
    }

    #[test]
    fn test_create_file() {
        let (_dir, db, mut service) = test_service(11);
        let ctx = service.ctx.clone();

        let usage_db = db_user::TenantUserDb::new(db.clone(), ctx.clone());
        let tx = usage_db.start_read_tx().unwrap();
        let usage = usage_db.get_read_tx(&tx).unwrap();
        assert!(usage.is_none());

        // New path, new content -> succeed and increase usage
        let file_meta = service.create_file("testpath".to_string(), b"testcontent");

        // New path, new content -> succeed and increase usage
        assert!(file_meta.is_ok());
        let tx = usage_db.start_read_tx().unwrap();
        let usage = usage_db.get_read_tx(&tx).unwrap().unwrap();
        assert_eq!(usage.usage, 11);

        service.default_user_quota_bytes = 22;

        let file_meta = service.create_file("testpath2".to_string(), b"tsetcontent");
        assert!(file_meta.is_ok());
        let tx = usage_db.start_read_tx().unwrap();
        let usage = usage_db.get_read_tx(&tx).unwrap().unwrap();
        assert_eq!(usage.usage, 22);

        // new path, old content -> succeed and keep usage the same
        let file_meta = service.create_file("testpath3".to_string(), b"tsetcontent");
        assert!(file_meta.is_ok());
        let tx = usage_db.start_read_tx().unwrap();
        let usage = usage_db.get_read_tx(&tx).unwrap().unwrap();
        assert_eq!(usage.usage, 22);

        // existing path, new content, limited quota -> fails with QuotaExceeded
        let file_meta = service.create_file("testpath3".to_string(), b"newcontent");
        assert!(file_meta.is_err());
        assert!(matches!(
            file_meta.err().unwrap(),
            ServiceError::QuotaExceeded
        ));
        let tx = usage_db.start_read_tx().unwrap();
        let usage = usage_db.get_read_tx(&tx).unwrap().unwrap();
        assert_eq!(22, usage.usage);
        assert!(storage::exist_storage_path(
            service.storage_dir.as_path(),
            &ctx.tenant_id,
            &hash_bytes(b"tsetcontent")
        ));
        assert!(!storage::exist_storage_path(
            service.storage_dir.as_path(),
            &ctx.tenant_id,
            &hash_bytes(b"newcontent")
        ));
        // existing path, new content -> succeed and increase usage
        service.default_user_quota_bytes = 32;

        let file_meta = service.create_file("testpath3".to_string(), b"newcontent");
        assert!(file_meta.is_ok());
        let tx = usage_db.start_read_tx().unwrap();
        let usage = usage_db.get_read_tx(&tx).unwrap().unwrap();
        assert_eq!(usage.usage, 32);
    }

    #[test]
    fn test_create_file_rejects_exceeded_quota() {
        let (_dir, _db, service) = test_service(2);

        let file_meta = service.create_file("testpath".to_string(), b"123");

        assert!(file_meta.is_err());
        assert!(matches!(
            file_meta.err().unwrap(),
            ServiceError::QuotaExceeded
        ));
    }

    #[test]
    fn test_create_file_rejects_exceeded_quota_due_to_pending_uploads() {
        let (_dir, _db, mut service) = test_service(101);

        // Initial upload
        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 100,
            total_hash: "abc123".to_string(),
            chunk_count: 5,
        };

        service.create_upload(upload_req).unwrap();

        service.default_user_quota_bytes = 100;

        let file_meta = service.create_file("testpath".to_string(), b"123");

        assert!(file_meta.is_err());
        assert!(matches!(
            file_meta.err().unwrap(),
            ServiceError::QuotaExceeded
        ));
    }

    #[test]
    fn test_create_file_rejects_exceeded_quota_due_to_both_pending_uploads_and_stored_files() {
        let (_dir, _db, service) = test_service(10);

        // Direct upload
        service
            .create_file("testpath".to_string(), b"abcd")
            .unwrap();

        // Chunked pending upload
        let upload_req = InitUploadRequest {
            path: "pending".to_string(),
            total_size: 4, // 4 reserved bytes
            total_hash: hash_bytes(b"efgh"),
            chunk_count: 5,
        };

        service.create_upload(upload_req).unwrap();

        // Reject additional chunked upload and direct upload based on given quota
        let upload_req = InitUploadRequest {
            path: "testpath2".to_string(),
            total_size: 3,
            total_hash: hash_bytes(b"ijk"),
            chunk_count: 5,
        };

        let upload = service.create_upload(upload_req);
        assert!(upload.is_err());
        assert!(matches!(upload.err().unwrap(), ServiceError::QuotaExceeded));

        let file_meta = service.create_file("testpath3".to_string(), b"lmn");
        assert!(file_meta.is_err());
        assert!(matches!(
            file_meta.err().unwrap(),
            ServiceError::QuotaExceeded
        ));
    }

    #[test]
    fn test_create_upload() {
        let (_dir, db, mut service) = test_service(101);
        let ctx = service.ctx.clone();

        let usage_db = db_user::TenantUserDb::new(db.clone(), ctx.clone());

        // Initial upload
        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 100,
            total_hash: "abc123".to_string(),
            chunk_count: 5,
        };

        let upload = service.create_upload(upload_req);
        assert!(upload.is_ok());
        let tx = usage_db.start_read_tx().unwrap();
        assert_eq!(usage_db.get_read_tx(&tx).unwrap().unwrap().usage, 0);

        // Additional upload accumulates usage
        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 100,
            total_hash: "abc123other".to_string(),
            chunk_count: 5,
        };
        service.default_user_quota_bytes = 201;

        let upload = service.create_upload(upload_req.clone());
        assert!(upload.is_ok());
        let tx = usage_db.start_read_tx().unwrap();
        assert_eq!(usage_db.get_read_tx(&tx).unwrap().unwrap().usage, 0);

        // Duplicate uploads accumulates usage
        service.default_user_quota_bytes = 301;

        let upload = service.create_upload(upload_req);
        assert!(upload.is_ok());
        let tx = usage_db.start_read_tx().unwrap();
        assert_eq!(usage_db.get_read_tx(&tx).unwrap().unwrap().usage, 0);
    }

    #[test]
    fn test_create_upload_rejects_exceeded_quota() {
        let (_dir, _db, service) = test_service(10);

        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 100,
            total_hash: "abc123".to_string(),
            chunk_count: 5,
        };

        let upload = service.create_upload(upload_req);

        assert!(upload.is_err());
        assert!(matches!(upload.err().unwrap(), QuotaExceeded))
    }

    #[test]
    fn test_create_upload_rejects_exceeded_quota_after_uploads() {
        let (_dir, _db, service) = test_service(101);

        // First upload inside quota limits
        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 100,
            total_hash: "abc123".to_string(),
            chunk_count: 5,
        };

        let upload = service.create_upload(upload_req);
        assert!(upload.is_ok());

        // Additional upload exceeds quota
        let upload_req = InitUploadRequest {
            path: "testpath2".to_string(),
            total_size: 100,
            total_hash: "abc123new".to_string(),
            chunk_count: 5,
        };
        let upload = service.create_upload(upload_req);
        assert!(upload.is_err());
        assert!(matches!(upload.err().unwrap(), QuotaExceeded))
    }

    #[test]
    fn test_create_uploads_isolates_tenant_usage() {
        let (_dir, db, service) = test_service(101);
        let mut ctx2 = service.ctx.clone();
        ctx2.tenant_id = "tenant2".to_string();
        ctx2.user_id = "user2".to_string();

        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 100,
            total_hash: "abc123".to_string(),
            chunk_count: 5,
        };

        let upload = service.create_upload(upload_req);
        assert!(upload.is_ok());

        // Additional upload exceeds quota
        let upload_req = InitUploadRequest {
            path: "testpath2".to_string(),
            total_size: 100,
            total_hash: "abc123new".to_string(),
            chunk_count: 5,
        };
        let service_2 = TenantFileService::new(
            db.clone(),
            ctx2.clone(),
            service.storage_dir.clone(),
            service.staging_dir.clone(),
            101,
        );

        let upload = service_2.create_upload(upload_req);
        assert!(upload.is_ok());
    }

    #[test]
    fn test_finalize_upload() {
        let (_dir, db, service) = test_service(101);
        let ctx = service.ctx.clone();

        // Initial upload
        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 6,
            total_hash: hash_bytes(b"abc123"),
            chunk_count: 2,
        };

        let upload_id = service.create_upload(upload_req).unwrap();

        service.replace_chunk(&upload_id, 0, b"abc").unwrap();
        service.replace_chunk(&upload_id, 1, b"123").unwrap();

        let res = service.finalize_upload(upload_id.as_str());
        assert!(res.is_ok(), "finalize_upload failed: {:?}", res.err());

        let tx = TenantUserDb::new(db.clone(), ctx.clone())
            .start_read_tx()
            .unwrap();
        let usage = TenantUserDb::new(db.clone(), ctx.clone())
            .get_read_tx(&tx)
            .unwrap()
            .unwrap();
        assert_eq!(usage.usage, 6);

        let file_meta = res.unwrap();
        assert_eq!(file_meta.path, "testpath");
        assert_eq!(file_meta.size, 6);
        assert_eq!(file_meta.content_hash, hash_bytes(b"abc123"));
        assert_eq!(file_meta.version, 1);
        assert_eq!(file_meta.is_deleted, false);
        assert_eq!(file_meta.tenant_id, ctx.tenant_id);
        assert_eq!(file_meta.user_id, ctx.user_id);

        let persisted = TenantDb::new(db.clone(), ctx.clone())
            .get("testpath")
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&persisted).unwrap(),
            serde_json::to_value(&file_meta).unwrap()
        );
        let blob_path = storage::get_storage_path(
            service.storage_dir.as_path(),
            &ctx.tenant_id,
            &persisted.content_hash,
        );
        assert_eq!(std::fs::read(blob_path).unwrap(), b"abc123");

        // Upload is deleted after finalization
        let upload = TenantUploadDb::new(db.clone(), ctx.clone()).get(&upload_id);
        assert!(
            upload.unwrap().is_none(),
            "upload should be deleted after finalization"
        );

        // Staging directory is deleted after finalization
        let staging_dir = std::path::Path::new(service.staging_dir.as_path()).join(&upload_id);
        assert!(
            !staging_dir.exists(),
            "staging directory should be deleted after finalization"
        );
    }

    #[test]
    fn test_finalize_upload_rejects_with_missing_chunks() {
        let (_dir, db, service) = test_service(101);
        let ctx = service.ctx.clone();

        // Initial upload
        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 6,
            total_hash: hash_bytes(b"abc123"),
            chunk_count: 2,
        };

        let upload_id = service.create_upload(upload_req).unwrap();

        service.replace_chunk(&upload_id, 0, b"abc").unwrap();

        let before = finalize_state(&db, &ctx, &upload_id);
        let res = service.finalize_upload(upload_id.as_str());
        assert!(
            matches!(res, Err(ServiceError::Validation(_))),
            "unexpected error: {:?}",
            res.as_ref().err()
        );
        assert_eq!(finalize_state(&db, &ctx, &upload_id), before);
        assert!(
            !storage::get_storage_path(
                service.storage_dir.as_path(),
                &ctx.tenant_id,
                &hash_bytes(b"abc123")
            )
            .exists()
        );
    }

    #[test]
    fn test_finalize_upload_rejects_with_unmatched_hash() {
        let (_dir, db, service) = test_service(101);
        let ctx = service.ctx.clone();

        // Initial upload
        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 6,
            total_hash: hash_bytes(b"someotherstring"),
            chunk_count: 2,
        };

        let upload_id = service.create_upload(upload_req).unwrap();

        service.replace_chunk(&upload_id, 0, b"abc").unwrap();
        service.replace_chunk(&upload_id, 1, b"123").unwrap();

        let before = finalize_state(&db, &ctx, &upload_id);
        let res = service.finalize_upload(upload_id.as_str());
        assert!(
            matches!(
                res,
                Err(ServiceError::HashMismatch(
                    storage::StorageError::HashMismatch
                ))
            ),
            "unexpected error: {:?}",
            res.as_ref().err()
        );
        assert_eq!(finalize_state(&db, &ctx, &upload_id), before);
        assert!(
            !storage::get_storage_path(
                service.storage_dir.as_path(),
                &ctx.tenant_id,
                &hash_bytes(b"someotherstring")
            )
            .exists()
        );
    }

    #[test]
    fn test_finalize_upload_rejects_with_missing_upload() {
        let (_dir, db, service) = test_service(0);
        let ctx = service.ctx.clone();

        let before = finalize_state(&db, &ctx, "nonexistent_upload_id");

        let res = service.finalize_upload("nonexistent_upload_id");
        assert!(
            matches!(res, Err(ServiceError::NotFound)),
            "unexpected error: {:?}",
            res.as_ref().err()
        );
        assert_eq!(finalize_state(&db, &ctx, "nonexistent_upload_id"), before);
    }

    #[test]
    fn test_finalize_does_not_increase_usage_if_file_exists() {
        let (_dir, db, mut service) = test_service(100);
        let ctx = service.ctx.clone();

        // Write file to storage to simulate existing file
        service
            .create_file("existing-path".to_string(), b"abc123")
            .unwrap();

        // Initial upload
        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 6,
            total_hash: hash_bytes(b"abc123"),
            chunk_count: 2,
        };
        service.default_user_quota_bytes = 101;

        let upload_id = service.create_upload(upload_req).unwrap();

        service.replace_chunk(&upload_id, 0, b"abc").unwrap();
        service.replace_chunk(&upload_id, 1, b"123").unwrap();

        let res = service.finalize_upload(upload_id.as_str());
        assert!(res.is_ok(), "finalize_upload failed: {:?}", res.err());

        let tx = TenantUserDb::new(db.clone(), ctx.clone())
            .start_read_tx()
            .unwrap();
        let usage = TenantUserDb::new(db.clone(), ctx.clone())
            .get_read_tx(&tx)
            .unwrap()
            .unwrap();
        assert_eq!(usage.usage, 6);
    }

    #[test]
    fn test_finalize_rejects_unmatched_content_size() {
        let (_dir, db, service) = test_service(101);
        let ctx = service.ctx.clone();

        // Initial upload
        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 1,
            total_hash: hash_bytes(b"abc123"),
            chunk_count: 2,
        };

        let upload_id = service.create_upload(upload_req).unwrap();

        service.replace_chunk(&upload_id, 0, b"abc").unwrap();
        service.replace_chunk(&upload_id, 1, b"123").unwrap();

        let before = finalize_state(&db, &ctx, &upload_id);
        let res = service.finalize_upload(upload_id.as_str());
        assert!(
            matches!(
                res,
                Err(ServiceError::Storage(storage::StorageError::SizeMismatch))
            ),
            "unexpected error: {:?}",
            res.as_ref().err()
        );
        assert_eq!(finalize_state(&db, &ctx, &upload_id), before);
        assert!(
            !storage::get_storage_path(
                service.storage_dir.as_path(),
                &ctx.tenant_id,
                &hash_bytes(b"abc123")
            )
            .exists()
        );
    }

    #[test]
    fn test_finalize_rejects_unmatched_content_size_for_existing_blobs() {
        let (_dir, db, service) = test_service(101);
        let ctx = service.ctx.clone();

        // Initial upload
        let upload_req = InitUploadRequest {
            path: "testpath".to_string(),
            total_size: 6,
            total_hash: hash_bytes(b"abc123"),
            chunk_count: 2,
        };

        let upload_id = service.create_upload(upload_req).unwrap();

        service.replace_chunk(&upload_id, 0, b"abc").unwrap();
        service.replace_chunk(&upload_id, 1, b"123").unwrap();

        service.finalize_upload(upload_id.as_str()).unwrap();

        // Second upload, same content, different path
        let upload_req = InitUploadRequest {
            path: "testpath2".to_string(),
            total_size: 1,
            total_hash: hash_bytes(b"abc123"),
            chunk_count: 2,
        };
        let upload_id = service.create_upload(upload_req).unwrap();

        service.replace_chunk(&upload_id, 0, b"abc").unwrap();
        service.replace_chunk(&upload_id, 1, b"123").unwrap();

        let before = finalize_state(&db, &ctx, &upload_id);
        let res = service.finalize_upload(upload_id.as_str());
        assert!(
            matches!(res, Err(ServiceError::Validation(_))),
            "unexpected error: {:?}",
            res.as_ref().err()
        );
        assert_eq!(finalize_state(&db, &ctx, &upload_id), before);
    }
}
