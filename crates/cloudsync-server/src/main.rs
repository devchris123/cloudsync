use std::sync::Arc;

use axum::{
    Json, Router, debug_handler,
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
};
use cloudsync_common::{
    CreateFileResponse, DeleteFileResponse, GetHealthResponse, ListFilesResponse,
};
use redb::Database;

mod db;
mod storage;

const DB_NAME: &str = "data.redb";

const DATA_DIR: &str = "cloudsync/data/files";

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

    let content_hash: String = storage::write(&content)?;
    let db = state.db;
    let file_meta = db::put(&db, &path, content.len() as u64, content_hash)?;

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
    let content = storage::read(&content_hash)?;
    Ok(content)
}

#[debug_handler]
async fn get_health() -> Result<Json<GetHealthResponse>, AppError> {
    Ok(Json(GetHealthResponse {
        status: "ok".to_string(),
    }))
}

#[derive(Clone)]
struct AppState {
    db: Arc<Database>,
}

fn create_app(state: AppState) -> Router {
    Router::<AppState>::new()
        .route("/api/v1/health", get(get_health))
        .route("/api/v1/files", get(list_files))
        .route("/api/v1/files", post(post_file))
        .route("/api/v1/files/{*path}", get(get_file))
        .route("/api/v1/files/{*path}", delete(delete_file))
        .with_state(state)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db = Database::create(DB_NAME)?;
    let tx = db.begin_write()?;
    tx.open_table(db::TABLE)?;
    tx.commit()?;
    let db = Arc::new(db);
    let state = AppState { db };
    let app = create_app(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3050").await.unwrap();
    axum::serve(listener, app).await.unwrap();

    Ok(())
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
