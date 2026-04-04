use clap::Parser;

use crate::config::ClientConfig;

mod cli;
mod client;
mod config;
mod sync;
mod db;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    match cli.command {
        cli::Command::Init { server_url, token } => {
            if ClientConfig::exists() {
                anyhow::bail!("Already initialized. Delete .cloudsync/ to reinitialize.")
            }
            std::fs::create_dir_all(config::CONFIG_DIR)?;
            let config = config::ClientConfig { server_url, token };
            config.save()?;
            db::init_db()?;
        }
        cli::Command::Push => {
            let _config = load_config()?;
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
        anyhow::bail!("Not initialized. Run 'cloudsynch init' first.")
    }
    config::ClientConfig::load()
}
