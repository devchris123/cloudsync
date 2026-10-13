pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 3050;
pub const DEFAULT_STORAGE_DIR: &str = "cloudsync/data/files";
pub const DEFAULT_STAGING_DIR: &str = "cloudsync/data/staging";
pub const DEFAULT_DBNAME: &str = "data.redb";
pub const DEFAULT_TENANT_ID: &str = "default-tenant";
pub const DEFAULT_USER_ID: &str = "default-user";
pub const DEFAULT_OIDC_AUDIENCE: &str = "cloudsync";
pub const DEFAULT_USER_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub struct OidcConfig {
    pub issuer: String,
    pub discovery_url: String,
    pub audience: String,
    pub client_id: String,
}

pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub storage_dir: String,
    pub staging_dir: String,
    pub token: String,
    pub dbname: String,
    pub default_tenant_id: String,
    pub default_user_id: String,
    pub oidc_config: Option<OidcConfig>,
    pub max_upload_file_size: u64,
    pub default_user_quota_bytes: u64,
}

impl ServerConfig {
    /// Create a config with default settings and an explicit authentication token.
    /// Does not read CLI arguments or environment variables.
    pub fn new(token: String) -> Self {
        Self {
            host: DEFAULT_HOST.into(),
            port: DEFAULT_PORT,
            storage_dir: DEFAULT_STORAGE_DIR.into(),
            staging_dir: DEFAULT_STAGING_DIR.into(),
            token,
            dbname: DEFAULT_DBNAME.into(),
            default_tenant_id: DEFAULT_TENANT_ID.into(),
            default_user_id: DEFAULT_USER_ID.into(),
            oidc_config: None,
            max_upload_file_size: 10 * 1024 * 1024 * 1024, // 10 GiB
            default_user_quota_bytes: DEFAULT_USER_QUOTA_BYTES,
        }
    }
}
