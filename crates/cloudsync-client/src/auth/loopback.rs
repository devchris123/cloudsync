//! Loopback Authorization Code + PKCE flow for `cloudsync login`.
//!
//! Best UX for desktop CLI use: opens the system browser to Keycloak, catches
//! the callback on a single-use localhost listener, exchanges the code, and
//! returns a freshly-minted [`OidcSession`] the caller persists to config.
//!
//! ## Security shape
//!
//! - **Public client + PKCE.** No client secret on disk. PKCE `code_verifier`
//!   stays in-process and never leaves the local machine; only the challenge
//!   (its SHA-256) goes to the IdP.
//! - **CSRF `state`.** Random 16-byte token; the callback handler refuses
//!   anything that doesn't byte-match the one we minted.
//! - **Random loopback port.** `127.0.0.1:0` → kernel picks. Wide
//!   `redirectUris: ["/*"]` on the Keycloak `cloudsync` client covers any
//!   port — no per-port realm config needed.
//! - **2-minute hard timeout** on the whole flow so a forgotten browser tab
//!   doesn't leave the CLI hanging.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use oauth2::{CsrfToken, PkceCodeChallenge};
use rand::RngCore;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{OidcSession, TokenResponse, fetch_discovery};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Unauthenticated /api/v1/auth/info payload — the server tells the CLI which
/// IdP to talk to and what client_id to claim. Keeps Keycloak coordinates out
/// of the client config schema.
#[derive(Deserialize)]
struct AuthInfo {
    issuer: Option<String>,
    client_id: Option<String>,
}

/// Run the full loopback login flow against `server_url`. Returns the
/// authenticated session, or an error if the user cancels, the IdP rejects,
/// or the 2-minute timeout expires.
pub async fn run(server_url: &str) -> anyhow::Result<OidcSession> {
    tokio::time::timeout(LOGIN_TIMEOUT, run_inner(server_url))
        .await
        .context("login timed out after 2 minutes — re-run `cloudsync login` to try again")?
}

async fn run_inner(server_url: &str) -> anyhow::Result<OidcSession> {
    // Step 1: ask the server what IdP it's expecting.
    let info = fetch_auth_info(server_url).await?;
    let (Some(issuer), Some(client_id)) = (info.issuer, info.client_id) else {
        anyhow::bail!(
            "the server at {server_url} has OIDC disabled. Use `cloudsync init --token <T>` instead."
        );
    };

    // Step 2: discover Keycloak's auth + token endpoints.
    let discovery = fetch_discovery(&issuer).await?;

    // Step 3: bind a random local port and learn it.
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .context("could not bind loopback listener for callback")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    tracing::debug!("loopback listener on {redirect_uri}");

    // Step 4: PKCE + CSRF.
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let csrf = CsrfToken::new_random();

    // Step 5: build authorize URL and open browser.
    let auth_url = format!(
        "{auth}?response_type=code&client_id={cid}&redirect_uri={ru}\
         &scope=openid+email&state={st}&code_challenge={cc}&code_challenge_method=S256",
        auth = discovery.authorization_endpoint,
        cid = urlencode(&client_id),
        ru = urlencode(&redirect_uri),
        st = urlencode(csrf.secret()),
        cc = urlencode(challenge.as_str()),
    );
    println!("Opening browser to sign in…");
    println!("If it doesn't open, visit: {auth_url}");
    // Failure to open the browser isn't fatal — print and continue waiting.
    let _ = webbrowser::open(&auth_url);

    // Step 6: accept the single callback request.
    let (code, state) = wait_for_callback(&listener).await?;

    // Step 7: CSRF check. Constant-time eq isn't strictly required since both
    // sides are equal-length random base64, but cheap to do correctly.
    if !constant_time_eq(state.as_bytes(), csrf.secret().as_bytes()) {
        anyhow::bail!("state mismatch on callback — possible CSRF attempt, aborting");
    }

    // Step 8: exchange code for tokens at the token endpoint.
    let token = exchange_code(
        &discovery.token_endpoint,
        &client_id,
        &code,
        verifier.secret(),
        &redirect_uri,
    )
    .await?;

    let expires_in = token.expires_in.unwrap_or(300);
    let email = token.id_token.as_deref().and_then(extract_email);

    Ok(OidcSession {
        issuer,
        client_id,
        access_token: token.access_token,
        // Keycloak always issues a refresh_token for confidential and public
        // clients with offline_access scope; for vanilla openid+email it's
        // still issued by default. If the IdP doesn't return one, log and
        // continue — the session just won't auto-refresh.
        refresh_token: token.refresh_token.unwrap_or_else(|| {
            tracing::warn!("IdP did not issue a refresh_token; session won't auto-refresh");
            String::new()
        }),
        expires_at: chrono::Utc::now().timestamp() + expires_in as i64,
        email,
    })
}

async fn fetch_auth_info(server_url: &str) -> anyhow::Result<AuthInfo> {
    let url = format!("{}/api/v1/auth/info", server_url.trim_end_matches('/'));
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let resp = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("server returned {} for {url}", resp.status());
    }
    Ok(resp.json::<AuthInfo>().await?)
}

/// Accept exactly one HTTP/1.1 GET on `/callback`, parse `code` and `state`
/// from the query string, write a small "you can close this tab" page, and
/// drop the socket.
async fn wait_for_callback(listener: &TcpListener) -> anyhow::Result<(String, String)> {
    let (mut socket, _peer) = listener.accept().await?;

    // Read until we have at least the request line + headers. Browsers send
    // tiny GETs (a few hundred bytes), so a single read is usually enough,
    // but loop until \r\n\r\n appears.
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 1024];
    loop {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            anyhow::bail!("loopback callback: connection closed before request was complete");
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 8192 {
            // Browsers never send 8KB on a callback. If we see this it's malformed.
            anyhow::bail!("loopback callback: request too large");
        }
    }

    let request = String::from_utf8_lossy(&buf);
    let request_line = request
        .lines()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty request"))?;
    // "GET /callback?code=X&state=Y HTTP/1.1"
    let path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("malformed request line"))?;

    let response_body = b"<!doctype html><html><body style='font-family:sans-serif;text-align:center;padding-top:4rem;'><h2>You can close this tab.</h2><p>Return to your terminal - cloudsync is signing you in.</p></body></html>";
    let response_header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response_body.len()
    );
    socket.write_all(response_header.as_bytes()).await?;
    socket.write_all(response_body).await?;
    socket.flush().await?;
    socket.shutdown().await.ok();

    // Parse ?code=...&state=... from the path.
    let query = path
        .split_once('?')
        .map(|(_, q)| q)
        .ok_or_else(|| anyhow::anyhow!("callback URL missing query string"))?;

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let v = urldecode(v);
        match k {
            "code" => code = Some(v),
            "state" => state = Some(v),
            "error" => error = Some(v),
            _ => {}
        }
    }

    if let Some(err) = error {
        anyhow::bail!("identity provider returned error: {err}");
    }
    let (Some(code), Some(state)) = (code, state) else {
        anyhow::bail!("callback URL missing code or state");
    };
    Ok((code, state))
}

async fn exchange_code(
    token_endpoint: &str,
    client_id: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> anyhow::Result<TokenResponse> {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    let resp = http.post(token_endpoint).form(&params).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("token endpoint returned {status}: {body}");
    }
    Ok(resp.json::<TokenResponse>().await?)
}

/// Extract `email` (falling back to `preferred_username`) from an unverified
/// JWT for *display only*. We don't validate the signature here — the server
/// will validate every access token on every API call, which is where auth
/// integrity actually lives. This value is only used in `cloudsync status`.
fn extract_email(id_token: &str) -> Option<String> {
    let mut parts = id_token.split('.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    let payload = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    v.get("email")
        .or_else(|| v.get("preferred_username"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
}

fn urlencode(s: &str) -> String {
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

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// Unused at the moment — kept around because `rand` is in `Cargo.toml`
/// for the loopback flow's randomness needs (oauth2's PKCE + CsrfToken cover
/// us today, but the device flow in step 5 will use this for the polling
/// jitter). Documented to dodge dead-code lints without an attribute.
#[allow(dead_code)]
pub(crate) fn random_b64_32() -> String {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_preserves_unreserved() {
        assert_eq!(urlencode("a.b-c_d~e"), "a.b-c_d~e");
    }

    #[test]
    fn urlencode_escapes_reserved() {
        assert_eq!(urlencode("a&b=c d"), "a%26b%3Dc%20d");
    }

    #[test]
    fn urldecode_handles_percent_and_plus() {
        assert_eq!(urldecode("a%26b%3Dc%20d"), "a&b=c d");
        assert_eq!(urldecode("hello+world"), "hello world");
    }

    #[test]
    fn urldecode_passes_through_garbage() {
        // `%g1` is not a valid escape — leave it alone rather than panic.
        assert_eq!(urldecode("%g1"), "%g1");
    }

    #[test]
    fn constant_time_eq_correct() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[test]
    fn extract_email_reads_email_claim() {
        // Hand-crafted JWT: header.payload.signature.
        // header  = {"alg":"RS256"}
        // payload = {"email":"u@example.com","sub":"abc"}
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"u@example.com","sub":"abc"}"#);
        let token = format!("xxx.{payload}.yyy");
        assert_eq!(extract_email(&token).as_deref(), Some("u@example.com"));
    }

    #[test]
    fn extract_email_falls_back_to_preferred_username() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"preferred_username":"alice","sub":"abc"}"#);
        let token = format!("xxx.{payload}.yyy");
        assert_eq!(extract_email(&token).as_deref(), Some("alice"));
    }

    #[test]
    fn extract_email_none_when_absent() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"abc"}"#);
        let token = format!("xxx.{payload}.yyy");
        assert_eq!(extract_email(&token), None);
    }

    #[test]
    fn extract_email_handles_garbage() {
        assert_eq!(extract_email("not.a.jwt"), None);
        assert_eq!(extract_email(""), None);
        assert_eq!(extract_email("only-one-part"), None);
    }
}
