use std::collections::BTreeSet;

use askama::Template;
use axum::{
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use cloudsync_common::FileMeta;
use hmac::{Hmac, Mac};
use rust_embed::RustEmbed;
use sha2::Sha256;

use crate::app::AppState;
use crate::db;

type HmacSha256 = Hmac<Sha256>;

const COOKIE_NAME: &str = "cloudsync_session";

#[derive(RustEmbed)]
#[folder = "static/"]
struct StaticAssets;

// --- Templates ---

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: String,
}

pub struct Breadcrumb {
    pub name: String,
    pub prefix: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub display_size: String,
    pub display_date: String,
}

#[derive(Template)]
#[template(path = "browser.html")]
struct BrowserTemplate {
    prefix: String,
    breadcrumbs: Vec<Breadcrumb>,
    directories: Vec<String>,
    files: Vec<FileEntry>,
}

// --- Cookie auth helpers ---

fn sign_token(token: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(token.as_bytes()).expect("HMAC can take key of any size");
    mac.update(b"cloudsync_session");
    hex::encode(mac.finalize().into_bytes())
}

fn make_session_cookie(token: &str) -> String {
    let sig = sign_token(token);
    format!("{COOKIE_NAME}={sig}; HttpOnly; SameSite=Strict; Path=/")
}

fn clear_session_cookie() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
}

pub fn verify_session_cookie(cookie_header: &str, token: &str) -> bool {
    let expected = sign_token(token);
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix(&format!("{COOKIE_NAME}=")) {
            return value == expected;
        }
    }
    false
}

// --- Helpers ---

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn build_breadcrumbs(prefix: &str) -> Vec<Breadcrumb> {
    let mut crumbs = Vec::new();
    let mut accumulated = String::new();
    for segment in prefix.split('/').filter(|s| !s.is_empty()) {
        accumulated.push_str(segment);
        accumulated.push('/');
        crumbs.push(Breadcrumb {
            name: segment.to_string(),
            prefix: accumulated.clone(),
        });
    }
    crumbs
}

fn has_session(headers: &axum::http::HeaderMap, token: &str) -> bool {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|cookie| verify_session_cookie(cookie, token))
}

// --- Handlers ---

#[derive(serde::Deserialize)]
pub struct BrowseQuery {
    pub prefix: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct LoginForm {
    pub token: String,
}

pub async fn index(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    if has_session(&headers, &state.token) {
        Redirect::to("/browse").into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

pub async fn login_page(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    if has_session(&headers, &state.token) {
        return Redirect::to("/browse").into_response();
    }
    let template = LoginTemplate {
        error: String::new(),
    };
    Html(template.render().unwrap()).into_response()
}

pub async fn login_submit(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<LoginForm>,
) -> Response {
    if form.token != state.token {
        let template = LoginTemplate {
            error: "Invalid token.".to_string(),
        };
        return (StatusCode::FORBIDDEN, Html(template.render().unwrap())).into_response();
    }
    let cookie = make_session_cookie(&state.token);
    let mut response = Redirect::to("/browse").into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    response
}

pub async fn logout() -> Response {
    let cookie = clear_session_cookie();
    let mut response = Redirect::to("/login").into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    response
}

pub async fn browse(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<BrowseQuery>,
) -> Response {
    if !has_session(&headers, &state.token) {
        return Redirect::to("/login").into_response();
    }

    let prefix = query.prefix.unwrap_or_default();
    let all_files = match db::list(&state.db) {
        Ok(f) => f,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to list files").into_response();
        }
    };

    let (files, directories) = files_and_dirs(all_files, &prefix);

    let template = BrowserTemplate {
        breadcrumbs: build_breadcrumbs(&prefix),
        prefix,
        directories: directories.into_iter().collect(),
        files,
    };

    Html(template.render().unwrap()).into_response()
}

fn files_and_dirs(all_files: Vec<FileMeta>, prefix: &str) -> (Vec<FileEntry>, BTreeSet<String>) {
    // Derive directories and files at the current prefix level
    let mut directories = BTreeSet::new();
    let mut files = Vec::new();

    for file_meta in &all_files {
        let Some(relative) = file_meta.path.strip_prefix(prefix) else {
            continue;
        };
        if let Some(slash_pos) = relative.find('/') {
            // This file is in a subdirectory
            let dir_name = &relative[..=slash_pos];
            directories.insert(dir_name.to_string());
        } else {
            // This file is at the current level
            files.push(FileEntry {
                path: file_meta.path.clone(),
                name: relative.to_string(),
                display_size: format_size(file_meta.size),
                display_date: file_meta.modified_at.format("%Y-%m-%d %H:%M").to_string(),
            });
        }
    }
    (files, directories)
}

pub async fn static_file(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    let Some(file) = StaticAssets::get(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    ([(header::CONTENT_TYPE, mime.as_ref())], file.data.to_vec()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ui::{sign_token, verify_session_cookie};

    #[test]
    fn test_sign_token_consistent() {
        let token = "token";

        assert_eq!(sign_token(token), sign_token(token));
    }

    #[test]
    fn test_verify_session_cookie_succeeds() {
        let signed_token = sign_token("sometoken");
        let cookie_header = format!("{COOKIE_NAME}={signed_token}");

        let result = verify_session_cookie(cookie_header.as_str(), "sometoken");

        assert!(result);
    }

    #[test]
    fn test_verify_session_cookie_fails() {
        let cookie_header = format!("{COOKIE_NAME}=falsetokenhash");

        let result = verify_session_cookie(cookie_header.as_str(), "sometoken");

        assert!(!result);
    }

    #[test]
    fn test_verify_session_cookie_missing_cookie_fails() {
        let cookie_header = "";

        let result = verify_session_cookie(cookie_header, "sometoken");

        assert!(!result);
    }

    #[test]
    fn test_browse() {
        let all_files = vec![
            create_file_meta("file0".to_string()),
            create_file_meta("file1".to_string()),
            create_file_meta("subdir/file0".to_string()),
            create_file_meta("subdir/file1".to_string()),
        ];
        let prefix = "";
        let expected_files = vec![
            FileEntry {
                path: "file0".to_string(),
                name: "file0".to_string(),
                display_size: format_size(0),
                display_date: all_files[0]
                    .modified_at
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
            },
            FileEntry {
                path: "file1".to_string(),
                name: "file1".to_string(),
                display_size: format_size(0),
                display_date: all_files[1]
                    .modified_at
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
            },
        ];
        let expected_dirs = BTreeSet::from(["subdir/".to_string()]);

        let (files, dirs) = files_and_dirs(all_files, prefix);

        assert_eq!(files, expected_files);
        assert_eq!(dirs, expected_dirs);
    }

    fn create_file_meta(path: String) -> FileMeta {
        FileMeta {
            path,
            size: 0,
            content_hash: "".to_string(),
            version: 0,
            is_deleted: false,
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
        }
    }
}
