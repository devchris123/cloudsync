//! OIDC web-login flow: Authorization Code + PKCE against Keycloak.
//!
//! The server's API path already validates OIDC access tokens (see [`crate::oidc`]).
//! This module adds the *login* side — the browser redirect, callback handler,
//! and user-session cookie — so a real Keycloak user can sign in to the web UI
//! as themselves and have their `sub` flow through to the existing tenancy code.
//!
//! ## Cookies
//!
//! - `cloudsync_oidc_state` (one per login attempt, ~10 min): carries the
//!   PKCE `code_verifier` and CSRF `state`. HMAC-signed. Cleared on callback.
//! - `cloudsync_user_session` (post-login, capped 8h): carries `sub|email|exp`.
//!   HMAC-signed. The auth layer reads it to populate [`UserContext`].
//!
//! Both cookies are HMAC-SHA256'd with `AppState::token` as the key — same
//! trust boundary as the legacy static-token cookie. No new secret to manage.
//!
//! ## Public vs internal URL
//!
//! The browser redirect goes to the **public** issuer (`AppState::oidc.issuer`),
//! reachable from the user's machine. The token exchange goes to the **internal**
//! discovery URL (`AppState::oidc.discovery_url`) — same hostname the JWKS fetch
//! already uses, often a Docker-internal name not reachable externally.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::app::AppState;

type HmacSha256 = Hmac<Sha256>;

pub const STATE_COOKIE: &str = "cloudsync_oidc_state";
pub const SESSION_COOKIE: &str = "cloudsync_user_session";

/// Hard cap on the state cookie lifetime regardless of what the IdP says.
/// A login attempt that's been sitting unused for 10 minutes is almost
/// certainly a forgotten tab; safer to ask the user to start over.
const STATE_COOKIE_MAX_AGE: Duration = Duration::from_secs(600);

/// Hard cap on the user-session lifetime. Even if Keycloak issues a token
/// good for 24h, we re-prompt after 8h. With Keycloak SSO still alive on the
/// IdP side this is a silent redirect, so the UX cost is small.
const SESSION_COOKIE_MAX_AGE: Duration = Duration::from_secs(8 * 3600);

// ---------- Cookie payloads ----------

/// Per-login-attempt material: PKCE verifier + CSRF state + where to return.
#[derive(Debug, PartialEq, Eq)]
pub struct StateCookie {
    pub state: String,
    pub verifier: String,
    pub return_to: String,
    pub issued_at: u64,
}

/// Authenticated user-session material. `exp` is unix-seconds, set to
/// `min(id_token.exp, now + SESSION_COOKIE_MAX_AGE)`.
#[derive(Debug, PartialEq, Eq)]
pub struct SessionCookie {
    pub sub: String,
    pub email: String,
    pub exp: u64,
}

impl StateCookie {
    fn encode(&self) -> String {
        // Pipe-separated and base64'd, simple and unambiguous. Components
        // never contain `|` (state/verifier are URL-safe-no-pad base64;
        // return_to is restricted at construction time — see make_state_cookie).
        let body = format!(
            "{}|{}|{}|{}",
            self.state, self.verifier, self.return_to, self.issued_at
        );
        URL_SAFE_NO_PAD.encode(body.as_bytes())
    }

    fn decode(raw: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(raw).ok()?;
        let body = String::from_utf8(bytes).ok()?;
        let mut parts = body.splitn(4, '|');
        let state = parts.next()?.to_string();
        let verifier = parts.next()?.to_string();
        let return_to = parts.next()?.to_string();
        let issued_at = parts.next()?.parse::<u64>().ok()?;
        Some(Self {
            state,
            verifier,
            return_to,
            issued_at,
        })
    }
}

impl SessionCookie {
    fn encode(&self) -> String {
        let body = format!("{}|{}|{}", self.sub, self.email, self.exp);
        URL_SAFE_NO_PAD.encode(body.as_bytes())
    }

    fn decode(raw: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(raw).ok()?;
        let body = String::from_utf8(bytes).ok()?;
        let mut parts = body.splitn(3, '|');
        let sub = parts.next()?.to_string();
        let email = parts.next()?.to_string();
        let exp = parts.next()?.parse::<u64>().ok()?;
        Some(Self { sub, email, exp })
    }
}

// ---------- Cookie sign / verify ----------

/// Sign `body` with HMAC-SHA256 keyed on the server's static token.
///
/// Reusing `state.token` keeps the trust boundary identical to the legacy
/// session cookie — anyone who knows the token can already authenticate, so
/// they could already mint cookies. No additional secret to manage.
fn sign(token: &str, label: &[u8], body: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(token.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(label);
    mac.update(b".");
    mac.update(body.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// Constant-time compare two HMAC tags. Hex/base64 strings of equal length are
/// safe to compare byte-by-byte under HMAC's collision resistance, but using
/// the `Mac::verify_slice` primitive routes through a constant-time impl.
fn verify_tag(token: &str, label: &[u8], body: &str, tag_b64: &str) -> bool {
    let Ok(tag) = URL_SAFE_NO_PAD.decode(tag_b64) else {
        return false;
    };
    let mut mac =
        HmacSha256::new_from_slice(token.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(label);
    mac.update(b".");
    mac.update(body.as_bytes());
    mac.verify_slice(&tag).is_ok()
}

fn split_signed(raw: &str) -> Option<(&str, &str)> {
    let (body, tag) = raw.split_once('.')?;
    Some((body, tag))
}

fn encode_signed(token: &str, label: &[u8], body: &str) -> String {
    let tag = sign(token, label, body);
    format!("{body}.{tag}")
}

fn decode_signed(token: &str, label: &[u8], raw: &str) -> Option<String> {
    let (body, tag) = split_signed(raw)?;
    if !verify_tag(token, label, body, tag) {
        return None;
    }
    Some(body.to_string())
}

// ---------- Cookie construction ----------

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn make_state_cookie(token: &str, payload: &StateCookie, secure: bool) -> String {
    let value = encode_signed(token, b"state", &payload.encode());
    let secure_attr = if secure { "; Secure" } else { "" };
    format!(
        "{STATE_COOKIE}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}{secure_attr}",
        STATE_COOKIE_MAX_AGE.as_secs()
    )
}

pub fn clear_state_cookie(secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!("{STATE_COOKIE}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0{secure_attr}")
}

pub fn make_session_cookie(token: &str, payload: &SessionCookie, secure: bool) -> String {
    let value = encode_signed(token, b"session", &payload.encode());
    let max_age = payload.exp.saturating_sub(now_secs());
    let secure_attr = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}{secure_attr}"
    )
}

pub fn clear_session_cookie(secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0{secure_attr}")
}

// ---------- Cookie reading ----------

fn read_cookie<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    cookie_header.split(';').find_map(|p| {
        let p = p.trim();
        p.strip_prefix(name)?.strip_prefix('=')
    })
}

/// Read and validate the state cookie. Returns the payload on success.
/// Returns `None` for any failure (missing, bad signature, malformed, expired).
pub fn read_state_cookie(token: &str, cookie_header: &str) -> Option<StateCookie> {
    let raw = read_cookie(cookie_header, STATE_COOKIE)?;
    let body = decode_signed(token, b"state", raw)?;
    let payload = StateCookie::decode(&body)?;
    let age = now_secs().saturating_sub(payload.issued_at);
    if age > STATE_COOKIE_MAX_AGE.as_secs() {
        return None;
    }
    Some(payload)
}

/// Read and validate the user-session cookie. Returns `None` on any failure
/// — the auth layer treats that as "no session" and falls through.
pub fn read_session_cookie(token: &str, cookie_header: &str) -> Option<SessionCookie> {
    let raw = read_cookie(cookie_header, SESSION_COOKIE)?;
    let body = decode_signed(token, b"session", raw)?;
    let payload = SessionCookie::decode(&body)?;
    if payload.exp <= now_secs() {
        return None;
    }
    Some(payload)
}

// ---------- Redirect-URI derivation ----------

/// Compute the absolute base URL of this server as seen by the user's browser,
/// for use as the OAuth `redirect_uri`. Order of preference:
///
/// 1. `CLOUDSYNC_PUBLIC_BASE_URL` env var (operator override).
/// 2. `X-Forwarded-Proto` + `X-Forwarded-Host` (Caddy/standard reverse-proxy headers).
/// 3. `Host` header + scheme inferred from `X-Forwarded-Proto` or defaulting to http.
///
/// Trailing slash is stripped so callers can `format!("{}/auth/callback", base)`.
pub fn derive_base_url(headers: &HeaderMap) -> String {
    if let Ok(env) = std::env::var("CLOUDSYNC_PUBLIC_BASE_URL") {
        return env.trim_end_matches('/').to_string();
    }
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim())
        .unwrap_or("http");
    let host = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        .or_else(|| {
            headers
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "localhost".to_string());
    format!("{scheme}://{host}")
}

// ---------- Handlers ----------

#[derive(serde::Deserialize)]
pub struct LoginParams {
    /// Optional relative path to bounce to after login (e.g. `/browse?prefix=foo/`).
    /// Absolute URLs and protocol-relative URLs are rejected to prevent open-redirect.
    pub return_to: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// Sanitize a `return_to` query param into a relative path-only redirect.
/// Anything absolute or protocol-relative collapses to `/browse`.
fn sanitize_return_to(input: Option<String>) -> String {
    match input {
        Some(s) if s.starts_with('/') && !s.starts_with("//") => s,
        _ => "/browse".to_string(),
    }
}

/// `GET /auth/login` — begin the Authorization Code + PKCE flow.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<LoginParams>,
) -> Response {
    let (Some(oidc), Some(oidc_client)) = (&state.oidc, &state.oidc_client) else {
        return (StatusCode::NOT_FOUND, "OIDC not configured").into_response();
    };

    // Discover endpoints; cache is shared with the validator so this is
    // almost always a no-op after the first call.
    let discovery = match oidc.discovery().await {
        Ok(d) => d,
        Err(err) => {
            tracing::error!("OIDC discovery failed at /auth/login: {err}");
            return (StatusCode::BAD_GATEWAY, "OIDC discovery failed").into_response();
        }
    };
    let Some(auth_endpoint) = discovery.authorization_endpoint.as_ref() else {
        tracing::error!("discovery doc missing authorization_endpoint");
        return (StatusCode::BAD_GATEWAY, "OIDC misconfigured").into_response();
    };

    let return_to = sanitize_return_to(params.return_to);
    let base_url = derive_base_url(&headers);
    let redirect_uri = format!("{base_url}/auth/callback");
    let secure = base_url.starts_with("https://");

    // Random state and PKCE verifier. CsrfToken::new_random gives a 128-bit
    // base64 random; PkceCodeChallenge::new_random_sha256 gives the spec-compliant
    // 32-byte verifier + SHA-256 challenge.
    let csrf = oauth2::CsrfToken::new_random();
    let (pkce_challenge, pkce_verifier) = oauth2::PkceCodeChallenge::new_random_sha256();

    let state_cookie_payload = StateCookie {
        state: csrf.secret().clone(),
        verifier: pkce_verifier.secret().clone(),
        return_to,
        issued_at: now_secs(),
    };
    let state_cookie_header = make_state_cookie(&state.token, &state_cookie_payload, secure);

    // Build the authorize URL by hand rather than via oauth2's builder because
    // we want the public `issuer` URL here, while the BasicClient we build
    // for /token will use the internal discovery URL. Mixing them inside the
    // same client risks accidentally exposing internal hostnames in redirects.
    let auth_url = format!(
        "{auth_endpoint}?response_type=code&client_id={cid}&redirect_uri={ru}\
         &scope=openid+email&state={st}&code_challenge={cc}&code_challenge_method=S256",
        cid = urlencode(&oidc_client.client_id),
        ru = urlencode(&redirect_uri),
        st = urlencode(csrf.secret()),
        cc = urlencode(pkce_challenge.as_str()),
    );

    let mut resp = Redirect::to(&auth_url).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&state_cookie_header).expect("cookie ascii"),
    );
    resp
}

/// `GET /auth/callback` — finish the flow: verify state, exchange code, set session.
pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<CallbackParams>,
) -> Response {
    let (Some(oidc), Some(oidc_client)) = (&state.oidc, &state.oidc_client) else {
        return (StatusCode::NOT_FOUND, "OIDC not configured").into_response();
    };

    // Pull state cookie before anything else. Anything missing → restart.
    let cookie_header = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(state_payload) = read_state_cookie(&state.token, cookie_header) else {
        tracing::warn!("auth callback: missing or invalid state cookie");
        return Redirect::to("/login?err=session_expired").into_response();
    };

    let secure = derive_base_url(&headers).starts_with("https://");

    // Pass IdP errors through verbatim (they come back as ?error=...&error_description=...).
    if let Some(err) = params.error {
        tracing::warn!(
            "auth callback: IdP returned error: {err} ({:?})",
            params.error_description
        );
        let mut resp = Redirect::to("/login?err=idp_error").into_response();
        resp.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&clear_state_cookie(secure)).expect("cookie ascii"),
        );
        return resp;
    }

    let (Some(code), Some(state_param)) = (params.code, params.state) else {
        return (StatusCode::BAD_REQUEST, "missing code/state").into_response();
    };

    // CSRF check: constant-time compare via verify_slice would be ideal, but
    // both sides are random base64 of equal length so byte compare is fine.
    // Use a constant-time eq to stay defensive.
    if !constant_time_eq(state_param.as_bytes(), state_payload.state.as_bytes()) {
        tracing::warn!("auth callback: state mismatch");
        return (StatusCode::BAD_REQUEST, "state mismatch").into_response();
    }

    // Token endpoint: from discovery doc (internal URL host — same as JWKS).
    let discovery = match oidc.discovery().await {
        Ok(d) => d,
        Err(err) => {
            tracing::error!("OIDC discovery failed at /auth/callback: {err}");
            return (StatusCode::BAD_GATEWAY, "OIDC discovery failed").into_response();
        }
    };
    let Some(token_endpoint) = discovery.token_endpoint.as_ref() else {
        return (StatusCode::BAD_GATEWAY, "OIDC misconfigured").into_response();
    };

    // Redirect URI we hand the token endpoint MUST byte-match the one we
    // sent on /auth/login. We derive from the same headers, so same value.
    let base_url = derive_base_url(&headers);
    let redirect_uri = format!("{base_url}/auth/callback");

    // Exchange the code. Public client, no client_secret. PKCE proves possession.
    let token_response = match exchange_code(
        token_endpoint,
        &oidc_client.client_id,
        &code,
        &state_payload.verifier,
        &redirect_uri,
    )
    .await
    {
        Ok(t) => t,
        Err(err) => {
            tracing::error!("token exchange failed: {err}");
            return (StatusCode::BAD_GATEWAY, "token exchange failed").into_response();
        }
    };

    // Validate the id_token via the existing OidcValidator. Its claims must
    // match issuer/audience/exp/signature exactly the same as API requests.
    let id_token = match token_response.id_token {
        Some(t) => t,
        None => {
            tracing::error!("token response missing id_token (scope=openid?)");
            return (StatusCode::BAD_GATEWAY, "id_token missing").into_response();
        }
    };
    let claims = match oidc.validate(id_token).await {
        Ok(c) => c,
        Err(err) => {
            tracing::error!("id_token validation failed: {err}");
            return (StatusCode::FORBIDDEN, "id_token rejected").into_response();
        }
    };

    let session_exp = std::cmp::min(
        claims.exp as u64,
        now_secs() + SESSION_COOKIE_MAX_AGE.as_secs(),
    );
    let session = SessionCookie {
        sub: claims.sub,
        email: claims.email,
        exp: session_exp,
    };
    let session_header = make_session_cookie(&state.token, &session, secure);
    let clear_state = clear_state_cookie(secure);

    let mut resp = Redirect::to(&state_payload.return_to).into_response();
    let h = resp.headers_mut();
    h.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&session_header).expect("cookie ascii"),
    );
    h.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&clear_state).expect("cookie ascii"),
    );
    resp
}

// ---------- Token exchange ----------

#[derive(serde::Deserialize)]
struct TokenResponse {
    #[allow(dead_code)]
    access_token: String,
    id_token: Option<String>,
    #[allow(dead_code)]
    refresh_token: Option<String>,
    #[allow(dead_code)]
    expires_in: Option<u64>,
}

async fn exchange_code(
    token_endpoint: &str,
    client_id: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> anyhow::Result<TokenResponse> {
    // Disable redirects on the HTTP client to prevent SSRF — a malicious IdP
    // response that 30x'd into our internal network would otherwise be followed.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    let resp = client.post(token_endpoint).form(&params).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("token endpoint returned {status}: {body}");
    }
    let parsed = resp.json::<TokenResponse>().await?;
    Ok(parsed)
}

// ---------- Helpers ----------

fn urlencode(s: &str) -> String {
    // Minimal RFC3986 encode. Reserved-but-allowed-in-query (`?`, `&`, `=`,
    // `#`, `+`, `/`, `:`) are escaped. Unreserved letters/digits/`-._~` pass through.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "test-token";

    #[test]
    fn state_cookie_roundtrip() {
        let original = StateCookie {
            state: "abc123".into(),
            verifier: "verifier-blob".into(),
            return_to: "/browse?prefix=foo/".into(),
            issued_at: now_secs(),
        };
        let cookie_header = make_state_cookie(TOKEN, &original, false);
        // Extract value from "<name>=<v>; ..."
        let raw_value = cookie_header
            .split(';')
            .next()
            .unwrap()
            .strip_prefix(&format!("{STATE_COOKIE}="))
            .unwrap();
        // Reconstruct the full Cookie header the way a browser would send it.
        let cookie_header = format!("{STATE_COOKIE}={raw_value}");
        let parsed = read_state_cookie(TOKEN, &cookie_header).expect("decode");
        assert_eq!(parsed, original);
    }

    #[test]
    fn state_cookie_tampered_signature_rejected() {
        let original = StateCookie {
            state: "abc".into(),
            verifier: "v".into(),
            return_to: "/".into(),
            issued_at: now_secs(),
        };
        let raw = make_state_cookie(TOKEN, &original, false);
        let value = raw.split(';').next().unwrap();
        // Flip the last char of the tag
        let mut bytes = value.as_bytes().to_vec();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(read_state_cookie(TOKEN, &tampered).is_none());
    }

    #[test]
    fn state_cookie_wrong_key_rejected() {
        let original = StateCookie {
            state: "abc".into(),
            verifier: "v".into(),
            return_to: "/".into(),
            issued_at: now_secs(),
        };
        let raw = make_state_cookie(TOKEN, &original, false);
        let value = raw.split(';').next().unwrap();
        let cookie_header = value.to_string();
        assert!(read_state_cookie("different-token", &cookie_header).is_none());
    }

    #[test]
    fn state_cookie_expired_rejected() {
        let payload = StateCookie {
            state: "abc".into(),
            verifier: "v".into(),
            return_to: "/".into(),
            issued_at: now_secs() - STATE_COOKIE_MAX_AGE.as_secs() - 5,
        };
        let raw = make_state_cookie(TOKEN, &payload, false);
        let value = raw.split(';').next().unwrap();
        assert!(read_state_cookie(TOKEN, value).is_none());
    }

    #[test]
    fn session_cookie_roundtrip() {
        let session = SessionCookie {
            sub: "user-abc-123".into(),
            email: "user@example.com".into(),
            exp: now_secs() + 3600,
        };
        let cookie_header = make_session_cookie(TOKEN, &session, true);
        let value = cookie_header.split(';').next().unwrap();
        let parsed = read_session_cookie(TOKEN, value).expect("decode");
        assert_eq!(parsed, session);
    }

    #[test]
    fn session_cookie_secure_flag_when_https() {
        let s = SessionCookie {
            sub: "u".into(),
            email: "e".into(),
            exp: now_secs() + 60,
        };
        let secure_cookie = make_session_cookie(TOKEN, &s, true);
        let insecure_cookie = make_session_cookie(TOKEN, &s, false);
        assert!(secure_cookie.contains("; Secure"));
        assert!(!insecure_cookie.contains("Secure"));
    }

    #[test]
    fn session_cookie_expired_rejected() {
        let session = SessionCookie {
            sub: "u".into(),
            email: "e".into(),
            exp: now_secs() - 1,
        };
        let cookie_header = make_session_cookie(TOKEN, &session, false);
        let value = cookie_header.split(';').next().unwrap();
        assert!(read_session_cookie(TOKEN, value).is_none());
    }

    #[test]
    fn read_cookie_finds_named_value_among_others() {
        // Browsers send "Cookie: a=1; b=2; cloudsync_user_session=xyz"
        let raw = format!("a=1; b=2; {SESSION_COOKIE}=just_a_value; c=3");
        let found = read_cookie(&raw, SESSION_COOKIE);
        assert_eq!(found, Some("just_a_value"));
    }

    #[test]
    fn read_cookie_missing_returns_none() {
        assert!(read_cookie("a=1; b=2", SESSION_COOKIE).is_none());
    }

    #[test]
    fn derive_base_url_prefers_env_var() {
        // SAFETY: tests run in one process; env mutation is sequential within a
        // single #[test] but global. This test does not run in parallel with
        // anything else that reads CLOUDSYNC_PUBLIC_BASE_URL.
        unsafe {
            std::env::set_var("CLOUDSYNC_PUBLIC_BASE_URL", "https://override.example/");
        }
        let h = HeaderMap::new();
        assert_eq!(derive_base_url(&h), "https://override.example");
        unsafe {
            std::env::remove_var("CLOUDSYNC_PUBLIC_BASE_URL");
        }
    }

    #[test]
    fn derive_base_url_uses_forwarded_headers() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        h.insert(
            "x-forwarded-host",
            HeaderValue::from_static("cloud.example"),
        );
        assert_eq!(derive_base_url(&h), "https://cloud.example");
    }

    #[test]
    fn derive_base_url_falls_back_to_host_and_http() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("localhost:3050"));
        assert_eq!(derive_base_url(&h), "http://localhost:3050");
    }

    #[test]
    fn sanitize_return_to_blocks_open_redirect() {
        assert_eq!(sanitize_return_to(None), "/browse");
        assert_eq!(
            sanitize_return_to(Some("/browse?prefix=x".into())),
            "/browse?prefix=x"
        );
        // Absolute URL → rewritten
        assert_eq!(
            sanitize_return_to(Some("https://attacker.example/".into())),
            "/browse"
        );
        // Protocol-relative → rewritten
        assert_eq!(sanitize_return_to(Some("//evil/".into())), "/browse");
    }

    #[test]
    fn urlencode_escapes_reserved() {
        assert_eq!(urlencode("hello world"), "hello%20world");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencode("https://x/y?z"), "https%3A%2F%2Fx%2Fy%3Fz");
        // Unreserved passes through
        assert_eq!(urlencode("a.b-c_d~e"), "a.b-c_d~e");
    }

    #[test]
    fn constant_time_eq_correct() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }
}
