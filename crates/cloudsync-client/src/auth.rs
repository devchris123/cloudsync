//! Authentication abstraction for the sync client.
//!
//! `SyncClient` previously held a single `token: String` and sent it as
//! `Authorization: Bearer <T>` on every request. Once OIDC lands the bearer
//! token is no longer static — it has to be resolved (and possibly refreshed)
//! before each call. This module introduces [`TokenSource`], an enum the
//! client consults right before constructing each request.
//!
//! For now the only variant is [`TokenSource::Static`] which wraps today's
//! shared bearer token; step 4 of the Keycloak-login work adds an `Oidc`
//! variant that does refresh-token roundtrips against Keycloak.
//!
//! The enum has interior async because future variants need network I/O.
//! Callers go through [`SyncClient::bearer`] which holds a `Mutex` to make
//! the `&mut self` requirement work behind a `&self` API.

/// Source of bearer tokens for outbound API requests.
#[derive(Debug, Clone)]
pub enum TokenSource {
    /// The legacy shared bearer token. Same value goes on every request.
    Static(String),
    // Oidc variant lands in step 4 of the keycloak-login plan.
}

impl TokenSource {
    /// Resolve to a bearer token suitable for `Authorization: Bearer <T>`.
    ///
    /// `async` because future variants will need to refresh against the IdP.
    /// Today's `Static` variant is a pure clone.
    pub async fn access_token(&mut self) -> anyhow::Result<String> {
        match self {
            TokenSource::Static(t) => Ok(t.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_token_returns_underlying_value() {
        let mut src = TokenSource::Static("my-token".to_string());
        let token = src.access_token().await.unwrap();
        assert_eq!(token, "my-token");
    }

    #[tokio::test]
    async fn static_token_idempotent() {
        let mut src = TokenSource::Static("t".to_string());
        let a = src.access_token().await.unwrap();
        let b = src.access_token().await.unwrap();
        assert_eq!(a, b);
    }
}
