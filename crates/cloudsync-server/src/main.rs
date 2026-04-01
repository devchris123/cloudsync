use std::sync::Arc;

use axum::{
    Json, Router, debug_handler, extract::{Multipart, State}, http::StatusCode, response::IntoResponse, routing::{delete, get, post}
};
use cloudsync_common::{CreateFileResponse, FileMeta};
use redb::{Database, TableDefinition};

const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("files");

const DB_NAME: &str = "data.redb";

const DATA_DIR: &str = "cloudsync/data/files";

fn list_files() {}

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
async fn post_file(State(state): State<AppState>, mut multipart: Multipart) -> Result<Json<CreateFileResponse>, AppError> {
    let mut path = None;
    let mut content = None;
    while let Some( field) = multipart.next_field().await? {
        match field.name().unwrap() {
            "path" => path = Some(field.text().await?),
            "file" => content = Some(field.bytes().await?),
            _ => {}
        }
    }
    let path = path.unwrap();
    let content = content.unwrap();

    let db = state.db;
    let tx = db.begin_read()?;
    let table = tx.open_table(TABLE)?;
    let raw_meta_access_guard = table.get(path.as_str())?;
    tx.close()?;

    let mut file: Option<FileMeta> = None;
    if let Some(raw_meta_access_guard) = raw_meta_access_guard {
        let bytes = raw_meta_access_guard.value();
        file = Some(serde_json::from_slice::<FileMeta>(bytes)?);
    }

    let content_hash = cloudsync_common::hash_bytes(&content);
    let mut file_meta = FileMeta {
        path: path.clone(),
        size: content.len() as u64,
        content_hash: content_hash.clone(),
        version: 1,
        is_deleted: false,
        created_at: chrono::Utc::now(),
        modified_at: chrono::Utc::now(),
    };

    match file {
        Some(file) => {
            file_meta.created_at = file.created_at;
            file_meta.version = file.version + 1;
        },
        None => {}
    }

    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(TABLE)?;
        let bytes = serde_json::to_vec(&file_meta)?;
        table.insert(path.as_str(), bytes.as_slice())?;
    }
    tx.commit()?;

    let dir = std::path::Path::new(DATA_DIR).join(&content_hash[0..2]);
    std::fs::create_dir_all(&dir)?;
    let data_path = dir.join(content_hash);
    std::fs::write(data_path, content)?;

    Ok(Json(CreateFileResponse {file: file_meta}))
}

fn delete_file() {}

fn get_file() {}

#[derive(Clone)]
struct AppState {
    db: Arc<Database>
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db = Database::create(DB_NAME)?;
    let db = Arc::new(db);
    let state = AppState{db};
    let app = Router::<AppState>::new()
        .route("/api/v1/files", get(list_files()))
        .route("/api/v1/files", post(post_file))
        .route("/api/v1/files/{path}", get(get_file()))
        .route("/api/v1/files/{path}", delete(delete_file()))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();

    Ok(())
}
