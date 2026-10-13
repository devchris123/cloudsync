use clap::Parser;
use cloudsync_server::config;

#[derive(Parser)]
pub struct Args {
    #[arg(
        long,
        env = "CLOUDSYNC_HOST",
        help = "Host address to bind the HTTP server to",
        default_value = config::DEFAULT_HOST
    )]
    pub host: String,
    #[arg(
        long,
        env = "CLOUDSYNC_PORT",
        help = "Port to listen on for HTTP requests",
        default_value_t = config::DEFAULT_PORT
    )]
    pub port: u16,
    #[arg(
        long,
        env = "CLOUDSYNC_TOKEN",
        help = "Static bearer token used to authenticate requests"
    )]
    pub token: String,
    #[arg(
        long,
        env = "CLOUDSYNC_STORAGE_DIR",
        help = "Directory for content-addressed file storage",
        default_value = config::DEFAULT_STORAGE_DIR
    )]
    pub storage_dir: String,
    #[arg(
        long,
        env = "CLOUDSYNC_STAGING_DIR",
        help = "Directory for temporary chunks from in-progress uploads",
        default_value = config::DEFAULT_STAGING_DIR
    )]
    pub staging_dir: String,
    #[arg(
        long,
        env = "CLOUDSYNC_DBNAME",
        help = "Path to the server metadata database file",
        default_value = config::DEFAULT_DBNAME
    )]
    pub dbname: String,
    #[arg(
        long,
        env = "CLOUDSYNC_DEFAULT_TENANT_ID",
        help = "Tenant ID assigned to requests authenticated with the static token",
        default_value = config::DEFAULT_TENANT_ID
    )]
    pub default_tenant_id: String,
    #[arg(
        long,
        env = "CLOUDSYNC_DEFAULT_USER_ID",
        help = "User ID assigned to requests authenticated with the static token",
        default_value = config::DEFAULT_USER_ID
    )]
    pub default_user_id: String,

    #[arg(
        long,
        env = "CLOUDSYNC_DEFAULT_USER_QUOTA_BYTES",
        help = "Default per-user storage quota in bytes (used when no per-user override is set)",
        default_value_t = config::DEFAULT_USER_QUOTA_BYTES
    )]
    pub default_user_quota_bytes: u64,

    #[arg(
        long,
        env = "CLOUDSYNC_OIDC_ISSUER",
        help = "Expected issuer URL for JWT validation"
    )]
    pub oidc_issuer: Option<String>,

    #[arg(
        long,
        env = "CLOUDSYNC_OIDC_DISCOVERY_URL",
        help = "Internal URL to fetch OIDC discovery document (defaults to issuer URL if not set)"
    )]
    pub oidc_discovery_url: Option<String>,

    #[arg(
        long,
        env = "CLOUDSYNC_OIDC_AUDIENCE",
        help = "Expected `aud` claim on incoming JWTs (the resource server identifier)",
        default_value = config::DEFAULT_OIDC_AUDIENCE
    )]
    pub oidc_audience: Option<String>,

    #[arg(
        long,
        env = "CLOUDSYNC_OIDC_CLIENT_ID",
        help = "OAuth client_id used by web UI + advertised to the CLI via /auth/info. Defaults to oidc_audience if unset."
    )]
    pub oidc_client_id: Option<String>,

    #[arg(
        long,
        env = "CLOUDSYNC_MAX_UPLOAD_FILE_SIZE",
        help = "Maximum allowed file size for clients to upload",
        default_value_t = 10 * 1024 * 1024 * 1024 // 10GiB
    )]
    pub max_upload_file_size: u64,
}
