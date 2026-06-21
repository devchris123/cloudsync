use std::path::PathBuf;

use ::serde::{Deserialize, Serialize};

use crate::auth::TokenSource;

pub const CONFIG_DIR: &str = ".cloudsync";
const CONFIG_PATH: &str = ".cloudsync/config.toml";

/// On-disk client configuration.
///
/// The TOML layout is intentionally flat:
///
/// ```toml
/// server_url = "https://cloudsync.example"
///
/// # Static-token auth (legacy default):
/// token = "..."
///
/// # OR — OIDC auth (cloudsync login):
/// [auth]
/// kind = "oidc"
/// issuer = "https://auth.example/realms/cloudsync"
/// client_id = "cloudsync"
/// access_token = "..."
/// refresh_token = "..."
/// expires_at = 1234567890
/// email = "user@example"
/// ```
///
/// The legacy bare-token shape is still accepted on read so that pre-OIDC
/// configs keep working after upgrade — they're transparently converted to
/// the `Static` variant in memory. We always *write* the explicit `[auth]`
/// block.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub server_url: String,
    pub auth: TokenSource,
}

/// Wire format — what actually lives on disk. Kept private so callers only
/// see the cleaned-up `ClientConfig`.
#[derive(Serialize, Deserialize)]
struct ConfigOnDisk {
    server_url: String,
    /// Legacy form: a top-level `token = "..."`. Present iff old config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    /// New form: a tagged `[auth]` block. Present iff post-OIDC config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth: Option<TokenSource>,
}

impl ClientConfig {
    pub fn exists() -> bool {
        ClientConfig::find_sync_root().is_ok()
    }

    pub fn load() -> anyhow::Result<ClientConfig> {
        let sync_root = ClientConfig::find_sync_root()?;
        let raw = std::fs::read_to_string(sync_root.join(CONFIG_PATH))?;
        let on_disk: ConfigOnDisk = toml::from_str(&raw)?;
        let auth = match (on_disk.auth, on_disk.token) {
            (Some(a), _) => a, // New form wins if both present.
            (None, Some(t)) => TokenSource::Static { token: t },
            (None, None) => {
                anyhow::bail!(
                    "config has neither `auth` block nor `token` — re-run `cloudsync init` or `cloudsync login`"
                )
            }
        };
        Ok(ClientConfig {
            server_url: on_disk.server_url,
            auth,
        })
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let sync_root = ClientConfig::find_sync_root()?;
        let on_disk = ConfigOnDisk {
            server_url: self.server_url.clone(),
            token: None,
            auth: Some(self.auth.clone()),
        };
        let data = toml::to_string(&on_disk)?;
        let path = sync_root.join(CONFIG_PATH);
        write_secure(&path, &data)?;
        Ok(())
    }

    pub fn find_sync_root() -> anyhow::Result<PathBuf> {
        let mut dir = std::env::current_dir()?;
        loop {
            if dir.join(CONFIG_DIR).exists() {
                return Ok(dir);
            }
            if !dir.pop() {
                return Err(anyhow::anyhow!("no sync dir found"));
            }
        }
    }
}

/// Write the config with permissions appropriate for a credential file
/// (refresh tokens live here). Equivalent to `umask 077` then write.
fn write_secure(path: &std::path::Path, content: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(content.as_bytes())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::OidcSession;

    #[test]
    fn parses_legacy_token_only_config() {
        let raw = r#"
server_url = "https://example"
token = "abc123"
"#;
        let on_disk: ConfigOnDisk = toml::from_str(raw).unwrap();
        assert_eq!(on_disk.token.as_deref(), Some("abc123"));
        assert!(on_disk.auth.is_none());
    }

    #[test]
    fn parses_new_auth_block_static() {
        let raw = r#"
server_url = "https://example"

[auth]
kind = "static"
token = "xyz"
"#;
        let on_disk: ConfigOnDisk = toml::from_str(raw).unwrap();
        assert!(on_disk.token.is_none());
        match on_disk.auth.unwrap() {
            TokenSource::Static { token } => assert_eq!(token, "xyz"),
            _ => panic!(),
        }
    }

    #[test]
    fn parses_new_auth_block_oidc() {
        let raw = r#"
server_url = "https://example"

[auth]
kind = "oidc"
issuer = "https://auth.example/realms/x"
client_id = "cloudsync"
access_token = "at"
refresh_token = "rt"
expires_at = 9999
email = "u@x"
"#;
        let on_disk: ConfigOnDisk = toml::from_str(raw).unwrap();
        match on_disk.auth.unwrap() {
            TokenSource::Oidc(s) => {
                assert_eq!(s.access_token, "at");
                assert_eq!(s.email.as_deref(), Some("u@x"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn save_writes_secure_auth_block() {
        // Tests for save() share a temp dir + chdir; merging them into one
        // avoids the cargo-test-parallel cwd race.
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(CONFIG_DIR);
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let cfg = ClientConfig {
            server_url: "https://example".into(),
            auth: TokenSource::Oidc(OidcSession {
                issuer: "https://auth/realms/x".into(),
                client_id: "cloudsync".into(),
                access_token: "at".into(),
                refresh_token: "rt".into(),
                expires_at: 100,
                email: None,
            }),
        };
        cfg.save().unwrap();

        let written = std::fs::read_to_string(tmp.path().join(CONFIG_PATH)).unwrap();
        assert!(written.contains("[auth]"));
        assert!(written.contains("kind = \"oidc\""));
        // No legacy top-level token leaking through.
        assert!(!written.contains("\ntoken ="));

        // Refresh tokens are credentials — file mode must be 0600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(tmp.path().join(CONFIG_PATH)).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "config should be 0600, got {mode:o}");
        }

        std::env::set_current_dir(original_cwd).unwrap();
    }
}
