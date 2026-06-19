pub struct OidcConfig {
    pub issuer: String,
    pub discovery_url: String,
    pub audience: String,
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
}
