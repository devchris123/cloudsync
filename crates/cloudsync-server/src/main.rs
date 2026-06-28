use clap::Parser;

mod cli;

use cloudsync_server::app;
use cloudsync_server::config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let config = create_config(args);
    log_auth_posture(&config);
    log_base_url_posture(&config);

    // Start server
    let host = config.host.clone();
    let port = config.port;
    let app = app::bootstrap_app(config).unwrap();
    let listener = tokio::net::TcpListener::bind(format!("{}:{}", host, port))
        .await
        .unwrap();
    tracing::info!("server listening on {}:{}", host, port);
    axum::serve(listener, app).await.unwrap();
    Ok(())
}

/// Log which auth mechanisms are active so the deployed posture is visible
/// in logs. The OIDC issuer/audience are integrity-critical config; surfacing
/// them on startup makes a misconfigured deploy easy to spot.
fn log_auth_posture(config: &config::ServerConfig) {
    match &config.oidc_config {
        Some(oidc) => tracing::info!(
            issuer = %oidc.issuer,
            discovery_url = %oidc.discovery_url,
            audience = %oidc.audience,
            client_id = %oidc.client_id,
            "auth: OIDC enabled (static token also accepted)"
        ),
        None => tracing::warn!("auth: OIDC disabled, only static token accepted"),
    }
}

/// Warn if the deployment looks like it's serving over plain HTTP to a
/// non-loopback host. Without `CLOUDSYNC_PUBLIC_BASE_URL` pinned to https,
/// the cookie `Secure` flag is derived from `X-Forwarded-Proto` headers —
/// a misconfigured reverse proxy can silently downgrade session cookies to
/// non-Secure, leaving them sniffable on any plain-HTTP hop. Setting the
/// env var to the real https URL makes the decision deterministic and
/// immune to header weirdness.
fn log_base_url_posture(config: &config::ServerConfig) {
    let public = std::env::var("CLOUDSYNC_PUBLIC_BASE_URL").ok();
    let loopback = matches!(config.host.as_str(), "127.0.0.1" | "::1" | "localhost");
    match (public.as_deref(), loopback) {
        (Some(url), _) if url.starts_with("https://") => {
            tracing::info!(
                base_url = url,
                "base URL: pinned via CLOUDSYNC_PUBLIC_BASE_URL"
            );
        }
        (Some(url), true) => {
            tracing::info!(
                base_url = url,
                "base URL: pinned via CLOUDSYNC_PUBLIC_BASE_URL (http, loopback bind)"
            );
        }
        (Some(url), false) => {
            tracing::warn!(
                base_url = url,
                "base URL: CLOUDSYNC_PUBLIC_BASE_URL set to non-https on a non-loopback bind — \
                 cookies will be minted without `Secure` and travel in cleartext to any \
                 plain-HTTP client"
            );
        }
        (None, true) => {
            tracing::info!("base URL: not pinned, loopback bind — relying on request headers");
        }
        (None, false) => {
            tracing::warn!(
                "base URL: CLOUDSYNC_PUBLIC_BASE_URL unset on a non-loopback bind — \
                 cookie Secure flag will be derived from X-Forwarded-Proto, which a \
                 misconfigured proxy can silently strip. Pin to the public https URL."
            );
        }
    }
}

fn create_config(args: cli::Args) -> config::ServerConfig {
    config::ServerConfig {
        host: args.host,
        port: args.port,
        storage_dir: args.storage_dir,
        staging_dir: args.staging_dir,
        token: args.token,
        dbname: args.dbname,
        default_tenant_id: args.default_tenant_id,
        default_user_id: args.default_user_id,
        oidc_config: match (
            args.oidc_issuer,
            args.oidc_discovery_url,
            args.oidc_audience,
            args.oidc_client_id,
        ) {
            (Some(issuer), discovery_url, Some(audience), client_id) => Some(config::OidcConfig {
                discovery_url: discovery_url.unwrap_or_else(|| issuer.clone()),
                issuer,
                client_id: client_id.unwrap_or_else(|| audience.clone()),
                audience,
            }),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_config_defaults_to_issuer() {
        let args = cli::Args {
            host: "localhost".to_string(),
            port: 0,
            token: "test_token".to_string(),
            storage_dir: "test_storage".to_string(),
            staging_dir: "test_staging".to_string(),
            dbname: "test_db".to_string(),
            default_tenant_id: "test_tenant".to_string(),
            default_user_id: "test_user".to_string(),
            oidc_issuer: Some("https://example.com/issuer".to_string()),
            oidc_discovery_url: None,
            oidc_audience: Some("cloudsync".to_string()),
            oidc_client_id: None,
        };

        let config = create_config(args);

        assert!(config.oidc_config.is_some());
        let oidc_config = config.oidc_config.unwrap();
        assert_eq!(oidc_config.issuer, "https://example.com/issuer");
        assert_eq!(oidc_config.discovery_url, "https://example.com/issuer");
        assert_eq!(oidc_config.audience, "cloudsync");
        assert_eq!(
            oidc_config.client_id, "cloudsync",
            "client_id should default to audience when unset"
        );
    }

    #[test]
    fn test_create_config_keeps_distinct_discovery_url() {
        // Public issuer and internal discovery URL differ — the common
        // Docker case where the IdP is reachable on a different hostname
        // from inside the network.
        let args = cli::Args {
            host: "localhost".to_string(),
            port: 0,
            token: "test_token".to_string(),
            storage_dir: "test_storage".to_string(),
            staging_dir: "test_staging".to_string(),
            dbname: "test_db".to_string(),
            default_tenant_id: "test_tenant".to_string(),
            default_user_id: "test_user".to_string(),
            oidc_issuer: Some("https://auth.example.com/realms/cloudsync".to_string()),
            oidc_discovery_url: Some("http://keycloak:8080/realms/cloudsync".to_string()),
            oidc_audience: Some("cloudsync".to_string()),
            oidc_client_id: None,
        };

        let config = create_config(args);

        let oidc_config = config.oidc_config.expect("oidc config should be set");
        assert_eq!(
            oidc_config.issuer,
            "https://auth.example.com/realms/cloudsync"
        );
        assert_eq!(
            oidc_config.discovery_url,
            "http://keycloak:8080/realms/cloudsync"
        );
    }

    #[test]
    fn test_create_config_explicit_client_id() {
        // The web/CLI OAuth client_id can be configured independently of the
        // audience claim — the two collapse to the same value in the default
        // Keycloak setup, but the resource server identity (audience) and the
        // OAuth client identity (client_id) are different concepts.
        let args = cli::Args {
            host: "localhost".to_string(),
            port: 0,
            token: "test_token".to_string(),
            storage_dir: "test_storage".to_string(),
            staging_dir: "test_staging".to_string(),
            dbname: "test_db".to_string(),
            default_tenant_id: "test_tenant".to_string(),
            default_user_id: "test_user".to_string(),
            oidc_issuer: Some("https://example.com/issuer".to_string()),
            oidc_discovery_url: None,
            oidc_audience: Some("cloudsync-api".to_string()),
            oidc_client_id: Some("cloudsync-web".to_string()),
        };

        let config = create_config(args);

        let oidc_config = config.oidc_config.expect("oidc config should be set");
        assert_eq!(oidc_config.audience, "cloudsync-api");
        assert_eq!(oidc_config.client_id, "cloudsync-web");
    }
}
