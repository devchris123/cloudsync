use std::path::PathBuf;

use ::serde::{Deserialize, Serialize};

pub const CONFIG_DIR: &str = ".cloudsync";
const CONFIG_PATH: &str = ".cloudsync/config.toml";

#[derive(Serialize, Deserialize)]
pub struct ClientConfig {
    pub server_url: String,
    pub token: String
}

impl ClientConfig {
    pub fn exists() -> bool {
        ClientConfig::find_sync_root().is_ok()
    }

    pub fn load() -> anyhow::Result<ClientConfig> {
        let sync_root = ClientConfig::find_sync_root()?;
        let raw = std::fs::read_to_string(sync_root.join(CONFIG_PATH))?;
        let config = toml::from_str::<ClientConfig>(&raw)?;
        Ok(config)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let sync_root = ClientConfig::find_sync_root()?;
        let data = toml::to_string(self)?;
        std::fs::write(sync_root.join(CONFIG_PATH), data)?;
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
