use clap::Parser;

use crate::config::ClientConfig;

mod cli;
mod client;
mod config;
mod scanner;
mod sync;
mod db;


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    
    match cli.command {
        cli::Command::Init { server_url, token } => {
            if ClientConfig::exists() {
                anyhow::bail!("Already initialized. Delete .cloudsync/ to reinitialize.")
            }
            std::fs::create_dir_all(config::CONFIG_DIR)?;
            let config = config::ClientConfig { server_url, token };
            config.save()?;
            let sync_root = config::ClientConfig::find_sync_root()?; 
            db::open_db(&sync_root)?;
        }
        cli::Command::Push => {
            let config = load_config()?;
            let sync_root = config::ClientConfig::find_sync_root()?; 
            let db = db::open_db(&sync_root)?;
            let sync_client = client::SyncClient::new(
                config.server_url, config.token
            );
            sync::push(&db, &sync_client, &sync_root).await?;
        }
        cli::Command::Pull => {
            let _config = load_config()?;
        }
        cli::Command::Status => {
            let _config = load_config()?;
        }
    }

    Ok(())
}

fn load_config() -> anyhow::Result<ClientConfig> {
    if !config::ClientConfig::exists() {
        anyhow::bail!("Not initialized. Run 'cloudsync init' first.")
    }
    config::ClientConfig::load()
}
