use std::sync::Arc;

use axum::{
    Json, Router, debug_handler,
    extract::{Multipart, Path, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use cloudsync_common::{
    CreateFileResponse, DeleteFileResponse, GetHealthResponse, ListFilesResponse,
};
use redb::Database;

use super::db;
use super::storage;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub storage_dir: String,
    pub token: String,
}

struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(value: E) -> Self {
        AppError(value.into())
    }
}

#[debug_handler]
async fn list_files(State(state): State<AppState>) -> Result<Json<ListFilesResponse>, AppError> {
    let db = state.db;
    let files = db::list(&db)?;
    Ok(Json(ListFilesResponse { files }))
}

#[debug_handler]
async fn post_file(
    State(state): State<AppState>,
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
    let db = state.db;
    let file_meta = db::put(&db, &path, content.len() as u64, &content_hash)?;

    Ok(Json(CreateFileResponse { file: file_meta }))
}

#[debug_handler]
async fn delete_file(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Result<Json<DeleteFileResponse>, AppError> {
    let db = state.db;
    db::delete(&db, &path)?;
    Ok(Json(DeleteFileResponse {}))
}

#[debug_handler]
async fn get_file(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Result<Vec<u8>, AppError> {
    let db: Arc<Database> = state.db;
    let file_meta = db::get(&db, &path)?;
    let Some(file_meta) = file_meta else {
        return Err(AppError(anyhow::anyhow!("not found")));
    };
    let content_hash = file_meta.content_hash;
    let content = storage::read(&state.storage_dir, &content_hash)?;
    Ok(content)
}

#[debug_handler]
async fn get_health() -> Result<Json<GetHealthResponse>, AppError> {
    Ok(Json(GetHealthResponse {
        status: "ok".to_string(),
    }))
}

async fn auth_layer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let headers = request.headers();
    let auth_header = headers.get("Authorization");
    let Some(auth_header) = auth_header else {
        return Err(StatusCode::FORBIDDEN);
    };
    if auth_header.to_str().unwrap() != format!("Bearer {}", state.token) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}

pub fn create_app(state: AppState) -> Router {
    let auth_router = Router::<AppState>::new()
        .route("/api/v1/files", get(list_files))
        .route("/api/v1/files", post(post_file))
        .route("/api/v1/files/{*path}", get(get_file))
        .route("/api/v1/files/{*path}", delete(delete_file))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_layer,
        ));
    Router::<AppState>::new()
        .route("/api/v1/health", get(get_health))
        .merge(auth_router)
        .with_state(state)
}

pub fn bootstrap_app(storage_dir: String, token: String, dbname: String) -> anyhow::Result<Router> {
    let db = Database::create(dbname)?;
    let tx = db.begin_write()?;
    tx.open_table(db::TABLE)?;
    tx.commit()?;
    let db = Arc::new(db);
    let state = AppState {
        db,
        storage_dir,
        token,
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
