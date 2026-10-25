use axum::{body::Body, http::Request};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cloudsync_server::{app::bootstrap_app, config::ServerConfig};
use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use sha2::Sha256;
use tower::ServiceExt;

const TOKEN: &str = "static-login-token";
const SECRET: &str = "independent-server-only-session-secret";

fn config(dir: &tempfile::TempDir) -> ServerConfig {
    ServerConfig {
        storage_dir: dir.path().join("files").to_str().unwrap().into(),
        staging_dir: dir.path().join("staging").to_str().unwrap().into(),
        dbname: dir.path().join("server.redb").to_str().unwrap().into(),
        session_signing_secret: SECRET.into(),
        ..ServerConfig::new(TOKEN.into())
    }
}

fn static_cookie(key: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(b"cloudsync_session");
    format!(
        "cloudsync_session={}",
        hex::encode(mac.finalize().into_bytes())
    )
}

fn oidc_cookie(key: &str) -> String {
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let body = URL_SAFE_NO_PAD.encode(format!("tenant-a|user@example.com|{expiry}"));
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(b"session.");
    mac.update(body.as_bytes());
    format!(
        "cloudsync_user_session={body}.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}

#[tokio::test]
async fn cookies_require_the_session_secret_and_bearer_requires_the_login_token() {
    let dir = tempfile::tempdir().unwrap();
    let app = bootstrap_app(config(&dir)).unwrap();
    for (name, value, expected) in [
        ("Cookie", oidc_cookie(TOKEN), StatusCode::UNAUTHORIZED),
        ("Cookie", static_cookie(TOKEN), StatusCode::UNAUTHORIZED),
        ("Cookie", oidc_cookie(SECRET), StatusCode::OK),
        ("Cookie", static_cookie(SECRET), StatusCode::OK),
        ("Authorization", format!("Bearer {TOKEN}"), StatusCode::OK),
        (
            "Authorization",
            format!("Bearer {SECRET}"),
            StatusCode::UNAUTHORIZED,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/files")
                    .header(name, value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "credential case using {name}");
    }
}

#[tokio::test]
async fn static_login_issues_cookie_signed_with_session_secret() {
    let dir = tempfile::tempdir().unwrap();
    let app = bootstrap_app(config(&dir)).unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("token={TOKEN}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert_eq!(cookie, static_cookie(SECRET));

    // Both redirect handlers and protected routes must read the same secret.
    for path in ["/", "/login", "/browse", "/partials/files", "/api/v1/files"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("Cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        if matches!(path, "/" | "/login") {
            assert_eq!(response.status(), StatusCode::SEE_OTHER);
            assert_eq!(response.headers()["location"], "/browse");
        } else {
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
    }
}
