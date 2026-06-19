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
            "auth: OIDC enabled (static token also accepted)"
        ),
        None => tracing::warn!("auth: OIDC disabled, only static token accepted"),
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
        ) {
            (Some(issuer), discovery_url, Some(audience)) => Some(config::OidcConfig {
                discovery_url: discovery_url.unwrap_or_else(|| issuer.clone()),
                issuer,
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
        };

        let config = create_config(args);

        assert!(config.oidc_config.is_some());
        let oidc_config = config.oidc_config.unwrap();
        assert_eq!(oidc_config.issuer, "https://example.com/issuer");
        assert_eq!(oidc_config.discovery_url, "https://example.com/issuer");
        assert_eq!(oidc_config.audience, "cloudsync");
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
}
