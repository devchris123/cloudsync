use std::path::PathBuf;

use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use redb::Database;

use crate::config::ClientConfig;

use cloudsync_client::{cli, client, config, db, sync, ui};

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
            let (db, sync_client, sync_root) = setup().await?;
            let mp = MultiProgress::new();
            let on_file_start = |path: &str, count: u64| -> Box<dyn Fn()> {
                let pb = mp.add(ProgressBar::new(count));
                pb.set_style(ProgressStyle::with_template("{msg} [{bar:20}] {pos}/{len}").unwrap());
                pb.set_message(path.to_string());
                Box::new(move || pb.inc(1))
            };
            sync::push(&db, &sync_client, &sync_root, &on_file_start).await?;
        }
        cli::Command::Pull => {
            let (db, sync_client, sync_root) = setup().await?;
            sync::pull(&db, &sync_client, &sync_root).await?;
        }
        cli::Command::Status => {
            let (db, sync_client, sync_root) = setup().await?;
            sync::status(&db, &sync_client, &sync_root).await?;
        }
    }

    Ok(())
}

async fn setup() -> anyhow::Result<(Database, client::SyncClient, PathBuf)> {
    let config = load_config()?;
    let sync_root = config::ClientConfig::find_sync_root()?;
    let db = db::open_db(&sync_root)?;
    let sync_client = client::SyncClient::new(&config.server_url, config.token);
    sync_client
        .health()
        .await
        .map_err(|_| anyhow::anyhow!("Cannot connect to server at {}", &config.server_url))?;
    Ok((db, sync_client, sync_root))
}

fn load_config() -> anyhow::Result<ClientConfig> {
    if !config::ClientConfig::exists() {
        anyhow::bail!("Not initialized. Run 'cloudsync init' first.")
    }
    config::ClientConfig::load()
}
