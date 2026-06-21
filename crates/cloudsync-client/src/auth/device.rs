//! Device Authorization Grant (RFC 8628) for `cloudsync login --mode device`.
//!
//! For headless / SSH / CI boxes where opening a browser isn't an option.
//! The CLI prints a short code + verification URL; the user completes the
//! login on another device (their phone, usually); the CLI polls the token
//! endpoint until Keycloak says the user approved.
//!
//! ## Security shape
//!
//! - **Public client.** No client secret — Keycloak's `cloudsync` client is
//!   `publicClient: true`. The `device_code` itself is the proof of possession;
//!   only the CLI process that initiated the flow has it.
//! - **No PKCE.** RFC 8628 doesn't use PKCE — the equivalent protection is
//!   that the `device_code` (returned over HTTPS, never displayed) acts as
//!   the verifier. The shorter `user_code` shown to the user is only useful
//!   to bind a browser session to this device flow, not to redeem tokens.
//! - **Polling interval respect.** We always sleep at least the IdP-supplied
//!   `interval`, and we bump it on `slow_down` per RFC.
//! - **Total timeout = `expires_in` from the IdP** (Keycloak default: 600s).

use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use tokio::time::sleep;

use super::{OidcSession, TokenResponse, extract_email_from_id_token, fetch_discovery};

/// Device authorization endpoint response (RFC 8628 §3.2).
#[derive(Deserialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    /// "complete" URI: same as verification_uri but with the user_code baked in
    /// as a query param. Keycloak provides it; not all IdPs do.
    verification_uri_complete: Option<String>,
    expires_in: u64,
    /// Polling interval in seconds. Default to 5 if absent.
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_interval() -> u64 {
    5
}

/// Body of a non-success token response. RFC 8628 §3.5 enumerates the
/// device-flow-specific error codes.
#[derive(Deserialize)]
struct TokenError {
    error: String,
}

/// Unauthenticated /api/v1/auth/info — identical to what the loopback flow uses.
#[derive(Deserialize)]
struct AuthInfo {
    issuer: Option<String>,
    client_id: Option<String>,
}

/// Run the device flow against the cloudsync server's configured IdP.
pub async fn run(server_url: &str) -> anyhow::Result<OidcSession> {
    let info = fetch_auth_info(server_url).await?;
    let (Some(issuer), Some(client_id)) = (info.issuer, info.client_id) else {
        anyhow::bail!(
            "the server at {server_url} has OIDC disabled. Use `cloudsync init --token <T>` instead."
        );
    };

    let discovery = fetch_discovery(&issuer).await?;
    let Some(device_endpoint) = discovery.device_authorization_endpoint else {
        anyhow::bail!(
            "IdP at {issuer} does not advertise a device_authorization_endpoint. \
             Either enable the device flow on the Keycloak client \
             (oauth2.device.authorization.grant.enabled=true) or use `--mode loopback`.",
        );
    };

    // Step 1: ask for a device code.
    let auth = request_device_code(&device_endpoint, &client_id).await?;

    // Step 2: tell the user where to go.
    println!();
    println!("To finish signing in:");
    println!("  1. Visit:  {}", auth.verification_uri);
    println!("  2. Enter:  {}", auth.user_code);
    if let Some(complete) = &auth.verification_uri_complete {
        println!("  Or open directly: {complete}");
    }
    println!();
    println!("Waiting for approval (this terminal will pick up once you confirm)...");

    // Step 3: poll until success, denial, or expiry.
    let token = poll_for_token(
        &discovery.token_endpoint,
        &client_id,
        &auth.device_code,
        Duration::from_secs(auth.interval),
        Duration::from_secs(auth.expires_in),
    )
    .await?;

    let expires_in = token.expires_in.unwrap_or(300);
    let email = token
        .id_token
        .as_deref()
        .and_then(extract_email_from_id_token);

    Ok(OidcSession {
        issuer,
        client_id,
        access_token: token.access_token,
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

async fn request_device_code(
    device_endpoint: &str,
    client_id: &str,
) -> anyhow::Result<DeviceAuthorization> {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let params = [("client_id", client_id), ("scope", "openid email")];
    let resp = http.post(device_endpoint).form(&params).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("device authorization endpoint returned {status}: {body}");
    }
    Ok(resp.json::<DeviceAuthorization>().await?)
}

async fn poll_for_token(
    token_endpoint: &str,
    client_id: &str,
    device_code: &str,
    initial_interval: Duration,
    overall_timeout: Duration,
) -> anyhow::Result<TokenResponse> {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let start = std::time::Instant::now();
    let mut interval = initial_interval;
    loop {
        sleep(interval).await;

        if start.elapsed() >= overall_timeout {
            anyhow::bail!("device flow timed out — re-run `cloudsync login --mode device`");
        }

        let params = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", client_id),
            ("device_code", device_code),
        ];
        let resp = http.post(token_endpoint).form(&params).send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.json::<TokenResponse>().await?);
        }

        // Error body: parse `error` field, react per RFC 8628 §3.5.
        let body_text = resp.text().await.unwrap_or_default();
        let err = serde_json::from_str::<TokenError>(&body_text)
            .map(|e| e.error)
            .unwrap_or_else(|_| body_text.clone());
        match err.as_str() {
            "authorization_pending" => {
                // Keep polling at the current interval.
                continue;
            }
            "slow_down" => {
                // RFC says: bump interval by 5 seconds and keep going.
                interval += Duration::from_secs(5);
                continue;
            }
            "access_denied" => {
                anyhow::bail!("login denied by the user");
            }
            "expired_token" => {
                anyhow::bail!("device code expired — re-run `cloudsync login --mode device`");
            }
            other => {
                anyhow::bail!("token endpoint returned error: {other}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_authorization_default_interval_when_absent() {
        // Keycloak always includes `interval`, but the field is optional in the
        // RFC; default to 5s rather than panic.
        let raw = r#"{
            "device_code":"DC","user_code":"UC","verification_uri":"https://x/device",
            "expires_in":600
        }"#;
        let parsed: DeviceAuthorization = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.interval, 5);
    }

    #[test]
    fn device_authorization_uses_explicit_interval() {
        let raw = r#"{
            "device_code":"DC","user_code":"UC","verification_uri":"https://x/device",
            "expires_in":600,"interval":10
        }"#;
        let parsed: DeviceAuthorization = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.interval, 10);
    }

    #[test]
    fn token_error_parses_known_codes() {
        for code in [
            "authorization_pending",
            "slow_down",
            "access_denied",
            "expired_token",
        ] {
            let body = format!(r#"{{"error":"{code}"}}"#);
            let parsed: TokenError = serde_json::from_str(&body).unwrap();
            assert_eq!(parsed.error, code);
        }
    }
}
