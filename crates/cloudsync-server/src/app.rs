use std::sync::Arc;

use axum::{
    Json, Router,
    body::{self},
    debug_handler,
    extract::{self, DefaultBodyLimit, FromRequestParts, Multipart, Path, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use tower_http::trace::TraceLayer;

use cloudsync_common::{
    CreateFileResponse, DeleteFileResponse, FinalizeUploadResponse, GetHealthResponse,
    GetUploadResponse, InitUploadResponse, ListFilesResponse, ReplaceChunkResponse, hash_file,
    upload::InitUploadRequest,
};
use redb::Database;

use crate::{
    auth::UserContext, config::ServerConfig, db::TenantDb, db_upload::TenantUploadDb, migrations,
};

use super::db;
use super::storage;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub storage_dir: String,
    pub staging_dir: String,
    pub token: String,
    pub default_tenant_id: String,
    pub default_user_id: String,
}

struct AppError(anyhow::Error, StatusCode);

impl AppError {}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        tracing::error!("{}", self.0);
        (self.1, self.0.to_string()).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(value: E) -> Self {
        AppError(value.into(), StatusCode::INTERNAL_SERVER_ERROR)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for UserContext {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _: &S,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<UserContext>()
            .cloned()
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

#[debug_handler]
async fn list_files(
    State(state): State<AppState>,
    ctx: UserContext,
) -> Result<Json<ListFilesResponse>, AppError> {
    let db = TenantDb::new(state.db, ctx);
    let files = db.list()?;
    Ok(Json(ListFilesResponse { files }))
}

#[debug_handler]
async fn post_file(
    State(state): State<AppState>,
    ctx: UserContext,
    mut multipart: Multipart,
) -> Result<Json<CreateFileResponse>, AppError> {
    let mut path = None;
    let mut content = None;
    while let Some(field) = multipart.next_field().await? {
        match field.name().unwrap() {
            "path" => path = Some(field.text().await?),
            "file" => content = Some(field.bytes().await?),
            _ => {}
        }
    }
    let path = path.unwrap();
    let content = content.unwrap();

    let content_hash: String = storage::write(&state.storage_dir, &content)?;
    tracing::info!("file stored: {} (hash: {})", path, content_hash);
    let db = TenantDb::new(state.db, ctx);
    let file_meta = db.put(&path, content.len() as u64, &content_hash)?;
    tracing::info!("metadata saved: {} (version: {})", path, file_meta.version);

    Ok(Json(CreateFileResponse { file: file_meta }))
}

#[debug_handler]
async fn delete_file(
    State(state): State<AppState>,
    Path(path): Path<String>,
    ctx: UserContext,
) -> Result<Json<DeleteFileResponse>, AppError> {
    let db = TenantDb::new(state.db, ctx);
    db.delete(&path)?;
    tracing::info!("file marked as deleted: {}", path);
    Ok(Json(DeleteFileResponse {}))
}

#[debug_handler]
async fn get_file(
    State(state): State<AppState>,
    Path(path): Path<String>,
    ctx: UserContext,
    request: Request,
) -> Result<impl IntoResponse, AppError> {
    let db = TenantDb::new(state.db, ctx);
    let file_meta = db.get(&path)?;
    let Some(file_meta) = file_meta else {
        tracing::warn!("metadata not found: {}", path);
        return Err(AppError(
            anyhow::anyhow!("not found"),
            StatusCode::NOT_FOUND,
        ));
    };
    tracing::debug!(
        "metadata retrieved: {} (version: {})",
        path,
        file_meta.version
    );
    let content_hash = file_meta.content_hash;
    let path = storage::get_storage_path(&state.storage_dir, &content_hash);
    let resp = ServeFile::new(path).oneshot(request).await?;
    Ok(resp.into_response())
}

async fn create_upload(
    State(state): State<AppState>,
    ctx: UserContext,
    extract::Json(body): extract::Json<InitUploadRequest>,
) -> Result<Json<InitUploadResponse>, AppError> {
    let db = TenantUploadDb::new(state.db, ctx);
    let upload = db.create(body)?;
    let staging_dir = std::path::Path::new(&state.staging_dir).join(&upload.upload_id);
    std::fs::create_dir_all(staging_dir)?;
    Ok(Json(InitUploadResponse {
        upload_id: upload.upload_id,
    }))
}

async fn replace_chunk(
    State(state): State<AppState>,
    extract::Path((upload_id, index)): Path<(String, u32)>,
    ctx: UserContext,
    body: body::Bytes,
) -> Result<Json<ReplaceChunkResponse>, AppError> {
    let db = TenantUploadDb::new(state.db, ctx);
    let upload = db.get(&upload_id)?;
    let Some(upload) = upload else {
        return Err(AppError(
            anyhow::anyhow!("upload not found"),
            StatusCode::NOT_FOUND,
        ));
    };
    if index >= upload.chunk_count as u32 {
        return Err(AppError(
            anyhow::anyhow!("index larger than upload chunk_count"),
            StatusCode::BAD_REQUEST,
        ));
    }
    let staging_dir = std::path::Path::new(&state.staging_dir).join(&upload_id);
    let chunk_path = staging_dir.join(index.to_string());
    std::fs::write(chunk_path, body)?;
    db.add_chunk(&upload_id, index)?;
    Ok(Json(ReplaceChunkResponse { chunk_index: index }))
}

async fn get_upload(
    State(state): State<AppState>,
    extract::Path(upload_id): Path<String>,
    ctx: UserContext,
) -> Result<Json<GetUploadResponse>, AppError> {
    let db = TenantUploadDb::new(state.db, ctx);
    let upload = db.get(&upload_id)?;
    let Some(upload) = upload else {
        return Err(AppError(
            anyhow::anyhow!("not found"),
            StatusCode::NOT_FOUND,
        ));
    };
    Ok(Json(GetUploadResponse { upload }))
}

async fn finalize_upload(
    State(state): State<AppState>,
    extract::Path(upload_id): Path<String>,
    ctx: UserContext,
) -> Result<Json<FinalizeUploadResponse>, AppError> {
    let upload_db: TenantUploadDb = TenantUploadDb::new(state.db.clone(), ctx.clone());
    let upload = upload_db.get(&upload_id)?;
    let Some(upload) = upload else {
        return Err(AppError(
            anyhow::anyhow!("not found"),
            StatusCode::NOT_FOUND,
        ));
    };
    if upload.chunks_received.len() != upload.chunk_count as usize {
        return Err(AppError(
            anyhow::anyhow!("bad request"),
            StatusCode::BAD_REQUEST,
        ));
    }
    let staging_dir = std::path::Path::new(&state.staging_dir).join(&upload_id);
    let storage_path = storage::get_storage_path(&state.storage_dir, &upload.total_hash);
    std::fs::create_dir_all(storage_path.parent().unwrap())?;
    let mut storage_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&storage_path)?;
    for chunk_index in 0..upload.chunk_count {
        let chunk_path = staging_dir.join(chunk_index.to_string());
        let mut chunk_file = std::fs::OpenOptions::new().read(true).open(chunk_path)?;
        std::io::copy(&mut chunk_file, &mut storage_file)?;
    }

    let total_hash = hash_file(&storage_path)?;
    if upload.total_hash != total_hash {
        return Err(AppError(
            anyhow::anyhow!("unexpected hash mismatch after writing"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    let db = TenantDb::new(state.db, ctx);
    let file = db.put(&upload.path, upload.total_size, &upload.total_hash)?;
    upload_db.delete(&upload_id)?;
    std::fs::remove_dir_all(staging_dir)?;
    Ok(Json(FinalizeUploadResponse { file }))
}

#[debug_handler]
async fn get_health() -> Result<Json<GetHealthResponse>, AppError> {
    Ok(Json(GetHealthResponse {
        status: "ok".to_string(),
    }))
}

async fn bearer_auth_layer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(auth_header) = request.headers().get("Authorization")
        && auth_header.to_str().unwrap() == format!("Bearer {}", state.token)
    {
        tracing::trace!("bearer token valid");
        request.extensions_mut().insert(UserContext {
            tenant_id: state.default_tenant_id,
            user_id: state.default_user_id,
        });
    }
    next.run(request).await
}

async fn cookie_auth_layer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(cookie_header) = request.headers().get(axum::http::header::COOKIE)
        && let Ok(cookie_str) = cookie_header.to_str()
        && crate::ui::verify_session_cookie(cookie_str, &state.token)
    {
        tracing::trace!("session cookie valid");
        request.extensions_mut().insert(UserContext {
            tenant_id: state.default_tenant_id,
            user_id: state.default_user_id,
        });
    }
    next.run(request).await
}

async fn require_auth_layer(request: Request, next: Next) -> Result<Response, StatusCode> {
    if request.extensions().get::<UserContext>().is_some() {
        tracing::trace!("access granted");
        Ok(next.run(request).await)
    } else {
        tracing::warn!("access denied: no valid authorization");
        Err(StatusCode::FORBIDDEN)
    }
}

pub fn create_app(state: AppState) -> Router {
    // API routes with Bearer token / cookie auth
    let auth_router = Router::<AppState>::new()
        .route("/api/v1/files", get(list_files))
        .route("/api/v1/files", post(post_file))
        .route("/api/v1/files/{*path}", get(get_file))
        .route("/api/v1/files/{*path}", delete(delete_file))
        .route("/api/v1/uploads", post(create_upload))
        .route("/api/v1/uploads/{upload_id}", get(get_upload))
        .route(
            "/api/v1/uploads/{upload_id}/chunks/{index}",
            put(replace_chunk),
        )
        .route(
            "/api/v1/uploads/{upload_id}/finalize",
            post(finalize_upload),
        )
        .route_layer(axum::middleware::from_fn(require_auth_layer))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            cookie_auth_layer,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            bearer_auth_layer,
        ))
        .layer(DefaultBodyLimit::max(5 * 1024 * 1024)); // 4MB + overhead

    // Web UI routes (auth handled per-handler via cookie check)
    let ui_router = Router::<AppState>::new()
        .route("/", get(crate::ui::index))
        .route(
            "/login",
            get(crate::ui::login_page).post(crate::ui::login_submit),
        )
        .route("/logout", post(crate::ui::logout))
        .route("/browse", get(crate::ui::browse))
        .route("/static/{*path}", get(crate::ui::static_file));

    Router::<AppState>::new()
        .route("/api/v1/health", get(get_health))
        .merge(auth_router)
        .merge(ui_router)
        .layer(TraceLayer::new_for_http())
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024)) // 50MB
        .with_state(state)
}

pub fn bootstrap_app(config: ServerConfig) -> anyhow::Result<Router> {
    let db = db::open_db(&config.dbname)?;

    // Run migrations
    migrations::run_migrations(&db, &config.default_tenant_id, &config.default_user_id)?;

    let db = Arc::new(db);
    let state = AppState {
        db,
        storage_dir: config.storage_dir,
        staging_dir: config.staging_dir,
        token: config.token,
        default_tenant_id: config.default_tenant_id,
        default_user_id: config.default_user_id,
    };
    let app = create_app(state);
    Ok(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health() {
        let result = get_health().await;
        assert!(result.is_ok());
    }
}
