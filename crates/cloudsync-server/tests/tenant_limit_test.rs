use axum::{body::Body, http::Request};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cloudsync_server::{app::bootstrap_app, config::ServerConfig};
use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use sha2::Sha256;
use tower::ServiceExt;

const TOKEN: &str = "tenant-limit-test-token";
const SESSION_SECRET: &str = "tenant-limit-test-session-secret";

#[tokio::test]
async fn unauthenticated_requests_keep_existing_auth_behavior() {
    let (_dir, app) = test_app(0);
    for cookie in [None, Some("cloudsync_user_session=invalid")] {
        for path in ["/browse", "/api/v1/files", "/partials/files"] {
            let mut request = Request::builder().uri(path);
            if let Some(cookie) = cookie {
                request = request.header("Cookie", cookie);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            if path == "/browse" {
                assert_eq!(response.status(), StatusCode::SEE_OTHER);
                assert_eq!(response.headers()["location"], "/login");
            } else {
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
            }
        }
    }
}

fn test_app(max_tenant_count: u64) -> (tempfile::TempDir, axum::Router) {
    let dir = tempfile::tempdir().unwrap();
    let app = bootstrap_app(ServerConfig {
        storage_dir: dir.path().join("files").to_str().unwrap().into(),
        staging_dir: dir.path().join("staging").to_str().unwrap().into(),
        dbname: dir.path().join("server.redb").to_str().unwrap().into(),
        max_tenant_count,
        session_signing_secret: SESSION_SECRET.into(),
        ..ServerConfig::new(TOKEN.into())
    })
    .unwrap();
    (dir, app)
}

// Issue a valid OIDC session directly so these tests exercise admission through
// cookie authentication without depending on an external identity provider.
fn session_cookie(sub: &str) -> String {
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let body = URL_SAFE_NO_PAD.encode(format!("{sub}|{sub}@example.com|{expiry}"));
    let mut mac = Hmac::<Sha256>::new_from_slice(SESSION_SECRET.as_bytes()).unwrap();
    mac.update(b"session.");
    mac.update(body.as_bytes());
    let tag = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("cloudsync_user_session={body}.{tag}")
}

#[tokio::test]
async fn zero_tenant_limit_rejects_authenticated_bearer() {
    let (_dir, app) = test_app(0);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/files")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn tenant_limit_admits_existing_tenant_and_rejects_new_tenants() {
    // Each route must admit the first tenant itself, then enforce the same cap
    // for both cookie and bearer credentials on subsequent requests.
    for path in ["/api/v1/files", "/browse", "/partials/files"] {
        let (_dir, app) = test_app(1);
        let existing_cookie = session_cookie("first-tenant");
        let new_cookie = session_cookie("second-tenant");

        for (cookie, expected) in [
            (&existing_cookie, StatusCode::OK),
            (&new_cookie, StatusCode::FORBIDDEN),
            (&existing_cookie, StatusCode::OK),
        ] {
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
            assert_eq!(response.status(), expected, "cookie admission on {path}");
        }

        // The static token resolves to a different (default) tenant and must
        // share the cap already filled through cookie authentication.
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/files")
                    .header("Authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "after {path}");
    }
}
