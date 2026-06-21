//! Authentication abstraction for the sync client.
//!
//! `SyncClient` consults [`TokenSource`] right before each request to get
//! a fresh bearer token. Two variants today:
//!
//! - [`TokenSource::Static`] — legacy shared bearer token, same value on every call.
//! - [`TokenSource::Oidc`] — Keycloak access token, refreshed via refresh_token
//!   when within 60s of expiry. Initial tokens are obtained by `cloudsync login`
//!   (loopback flow in [`loopback`], device flow lands in step 5).
//!
//! The enum has interior async because [`TokenSource::Oidc`] needs network I/O.
//! Callers go through `SyncClient::bearer` which holds a `Mutex` to make the
//! `&mut self` requirement work behind a `&self` API.

use serde::{Deserialize, Serialize};

pub mod device;
pub mod loopback;

/// Margin before expiry within which we proactively refresh. Picked to be
/// long enough that a token can't expire mid-request even on slow networks,
/// short enough that we don't refresh constantly.
const REFRESH_MARGIN_SECS: i64 = 60;

/// Source of bearer tokens for outbound API requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TokenSource {
    /// The legacy shared bearer token. Same value goes on every request.
    Static { token: String },
    /// OIDC-issued access token; auto-refreshes via refresh_token.
    Oidc(OidcSession),
}

/// Persisted OIDC session — the tokens the user got from `cloudsync login`,
/// plus the IdP coordinates needed to refresh them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcSession {
    /// Public issuer URL (what the IdP puts in `iss`). Used for refresh.
    pub issuer: String,
    /// OAuth client_id we registered as in Keycloak.
    pub client_id: String,
    /// Bearer token sent on every API call until `expires_at`.
    pub access_token: String,
    /// Refresh token. Long-lived; treated as a credential — config file
    /// permissions are tightened to 0600 when this is written.
    pub refresh_token: String,
    /// Unix seconds. Server-side `exp` of the access_token.
    pub expires_at: i64,
    /// Email or preferred_username from the id_token, for `cloudsync status`
    /// to show *who* is logged in. Display-only; never read for auth.
    pub email: Option<String>,
}

impl TokenSource {
    /// Resolve to a bearer token suitable for `Authorization: Bearer <T>`.
    ///
    /// For [`TokenSource::Static`] this is a pure clone. For [`TokenSource::Oidc`]
    /// it refreshes the access token if it's within [`REFRESH_MARGIN_SECS`] of
    /// expiry, mutating self to cache the new token + refresh_token + expiry.
    pub async fn access_token(&mut self) -> anyhow::Result<String> {
        match self {
            TokenSource::Static { token } => Ok(token.clone()),
            TokenSource::Oidc(session) => {
                if session.needs_refresh() {
                    session.refresh().await?;
                }
                Ok(session.access_token.clone())
            }
        }
    }
}

impl OidcSession {
    fn needs_refresh(&self) -> bool {
        let now = chrono::Utc::now().timestamp();
        now + REFRESH_MARGIN_SECS >= self.expires_at
    }

    /// Refresh against `${issuer}/.well-known/openid-configuration → token_endpoint`.
    /// Mutates `access_token`, `refresh_token` (Keycloak rotates them by default),
    /// and `expires_at`. On failure leaves state unchanged so the caller can
    /// surface a clear "re-run `cloudsync login`" error.
    pub async fn refresh(&mut self) -> anyhow::Result<()> {
        tracing::debug!("refreshing OIDC access token (issuer={})", self.issuer);
        let discovery = fetch_discovery(&self.issuer).await?;
        // Disable redirects to prevent SSRF; the IdP's token endpoint never legitimately 30x's.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let params = [
            ("grant_type", "refresh_token"),
            ("client_id", self.client_id.as_str()),
            ("refresh_token", self.refresh_token.as_str()),
        ];
        let resp = http
            .post(&discovery.token_endpoint)
            .form(&params)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "refresh failed ({status}): {body}. Run `cloudsync login` to re-authenticate.",
            );
        }
        let parsed = resp.json::<TokenResponse>().await?;
        let expires_in = parsed.expires_in.unwrap_or(300);
        self.access_token = parsed.access_token;
        // Keycloak rotates refresh tokens on every refresh by default. If the
        // server *doesn't* rotate (refresh_token absent in the response), keep the old one.
        if let Some(rt) = parsed.refresh_token {
            self.refresh_token = rt;
        }
        self.expires_at = chrono::Utc::now().timestamp() + expires_in as i64;
        Ok(())
    }
}

// ---------- Discovery + token responses (shared with loopback) ----------

#[derive(Deserialize, Clone)]
pub(crate) struct OidcDiscovery {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// Optional: only present if the IdP advertises the device-authorization
    /// grant. Keycloak gates this behind a per-client attribute; the device
    /// flow surfaces a clear "enable it on the client" error when absent.
    #[serde(default)]
    pub device_authorization_endpoint: Option<String>,
}

pub(crate) async fn fetch_discovery(issuer: &str) -> anyhow::Result<OidcDiscovery> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let resp = http.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("discovery fetch failed ({}) at {url}", resp.status());
    }
    Ok(resp.json::<OidcDiscovery>().await?)
}

#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
    pub id_token: Option<String>,
}

/// Extract `email` (falling back to `preferred_username`) from an *unverified*
/// JWT — used for display only. The server validates every access token on
/// every API call, so we don't redo signature verification on the client side.
pub(crate) fn extract_email_from_id_token(id_token: &str) -> Option<String> {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_token_returns_underlying_value() {
        let mut src = TokenSource::Static {
            token: "my-token".to_string(),
        };
        let token = src.access_token().await.unwrap();
        assert_eq!(token, "my-token");
    }

    #[test]
    fn oidc_session_needs_refresh_when_expired() {
        let s = OidcSession {
            issuer: "x".into(),
            client_id: "c".into(),
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: chrono::Utc::now().timestamp() - 1,
            email: None,
        };
        assert!(s.needs_refresh());
    }

    #[test]
    fn oidc_session_needs_refresh_within_margin() {
        let s = OidcSession {
            issuer: "x".into(),
            client_id: "c".into(),
            access_token: "a".into(),
            refresh_token: "r".into(),
            // Just inside the margin → refresh.
            expires_at: chrono::Utc::now().timestamp() + REFRESH_MARGIN_SECS - 5,
            email: None,
        };
        assert!(s.needs_refresh());
    }

    #[test]
    fn oidc_session_no_refresh_when_fresh() {
        let s = OidcSession {
            issuer: "x".into(),
            client_id: "c".into(),
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            email: None,
        };
        assert!(!s.needs_refresh());
    }

    #[test]
    fn token_source_serde_roundtrip_static() {
        let original = TokenSource::Static {
            token: "abc".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains("\"kind\":\"static\""));
        let parsed: TokenSource = serde_json::from_str(&json).unwrap();
        match parsed {
            TokenSource::Static { token } => assert_eq!(token, "abc"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn token_source_serde_roundtrip_oidc() {
        let original = TokenSource::Oidc(OidcSession {
            issuer: "https://auth.example/realms/x".into(),
            client_id: "cloudsync".into(),
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_at: 9999,
            email: Some("u@x".into()),
        });
        let json = serde_json::to_string(&original).unwrap();
        let parsed: TokenSource = serde_json::from_str(&json).unwrap();
        if let TokenSource::Oidc(s) = parsed {
            assert_eq!(s.access_token, "at");
            assert_eq!(s.refresh_token, "rt");
            assert_eq!(s.email.as_deref(), Some("u@x"));
        } else {
            panic!("wrong variant")
        }
    }
}
