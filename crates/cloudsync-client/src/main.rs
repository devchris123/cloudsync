use std::path::PathBuf;

use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use redb::Database;

use crate::config::ClientConfig;

use cloudsync_client::auth::TokenSource;
use cloudsync_client::{auth, cli, client, config, db, sync};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    match cli.command {
        cli::Command::Init { server_url, token } => {
            if ClientConfig::exists() {
                anyhow::bail!("Already initialized. Delete .cloudsync/ to reinitialize.")
            }
            std::fs::create_dir_all(config::CONFIG_DIR)?;
            let config = config::ClientConfig {
                server_url,
                auth: TokenSource::Static { token },
            };
            config.save()?;
            let sync_root = config::ClientConfig::find_sync_root()?;
            db::open_db(&sync_root)?;
        }
        cli::Command::Login { server_url, mode } => {
            login(server_url, mode).await?;
        }
        cli::Command::Push => {
            let (db, sync_client, sync_root) = setup().await?;
            let on_file_start = create_on_file_start_fixed_inc();
            sync::push(&db, &sync_client, &sync_root, &on_file_start).await?;
        }
        cli::Command::Pull => {
            let (db, sync_client, sync_root) = setup().await?;
            let on_file_start = create_on_file_start_var_inc();
            sync::pull(&db, &sync_client, &sync_root, &on_file_start).await?;
        }
        cli::Command::Status => {
            let (db, sync_client, sync_root) = setup().await?;
            sync::status(&db, &sync_client, &sync_root).await?;
        }
    }

    Ok(())
}

async fn login(server_url: String, mode: cli::LoginMode) -> anyhow::Result<()> {
    // First-time login (no existing config) initializes the sync directory
    // in the CWD; otherwise we re-key the existing one in place. Either way
    // we never reuse an existing static-token config without warning — if
    // the user runs `cloudsync login` from a directory already initialized
    // with `--token`, the OIDC session replaces it.
    let initializing = !ClientConfig::exists();
    if initializing {
        std::fs::create_dir_all(config::CONFIG_DIR)?;
    }

    let session = match resolve_mode(mode) {
        cli::LoginMode::Loopback => auth::loopback::run(&server_url).await?,
        cli::LoginMode::Device => auth::device::run(&server_url).await?,
        // resolve_mode never returns Auto.
        cli::LoginMode::Auto => unreachable!(),
    };

    if let Some(email) = &session.email {
        println!("Signed in as {email}.");
    } else {
        println!("Signed in.");
    }

    let new_config = config::ClientConfig {
        server_url,
        auth: TokenSource::Oidc(session),
    };
    new_config.save()?;

    if initializing {
        let sync_root = config::ClientConfig::find_sync_root()?;
        db::open_db(&sync_root)?;
    }
    Ok(())
}

/// Resolve `--mode auto` into a concrete mode based on the environment.
/// Heuristics:
/// - On an SSH session (`SSH_TTY` set) → device flow. The user almost
///   certainly can't open a local browser on this box.
/// - Otherwise on a desktop session (`DISPLAY` / `WAYLAND_DISPLAY` set, or
///   macOS where `webbrowser` opens via `open`) → loopback.
/// - Otherwise → device flow.
fn resolve_mode(mode: cli::LoginMode) -> cli::LoginMode {
    match mode {
        cli::LoginMode::Auto => {
            if std::env::var_os("SSH_TTY").is_some() {
                cli::LoginMode::Device
            } else if cfg!(target_os = "macos")
                || std::env::var_os("DISPLAY").is_some()
                || std::env::var_os("WAYLAND_DISPLAY").is_some()
            {
                cli::LoginMode::Loopback
            } else {
                cli::LoginMode::Device
            }
        }
        m => m,
    }
}

async fn setup() -> anyhow::Result<(Database, client::SyncClient, PathBuf)> {
    let config = load_config()?;
    let sync_root = config::ClientConfig::find_sync_root()?;
    let db = db::open_db(&sync_root)?;
    let sync_client = client::SyncClient::with_source(&config.server_url, config.auth);
    sync_client
        .health()
        .await
        .map_err(|_| anyhow::anyhow!("Cannot connect to server at {}", &config.server_url))?;
    Ok((db, sync_client, sync_root))
}

fn load_config() -> anyhow::Result<ClientConfig> {
    if !config::ClientConfig::exists() {
        anyhow::bail!("Not initialized. Run 'cloudsync init' or 'cloudsync login' first.")
    }
    config::ClientConfig::load()
}

fn create_on_file_start_fixed_inc() -> impl Fn(&str, u64, u64) -> Box<dyn Fn()> {
    let mp = MultiProgress::new();
    move |path: &str, count: u64, completed: u64| -> Box<dyn Fn()> {
        let pb = mp.add(ProgressBar::new(count));
        pb.set_position(completed);
        pb.set_style(ProgressStyle::with_template("{msg} [{bar:20}] {pos}/{len}").unwrap());
        pb.set_message(path.to_string());
        Box::new(move || pb.inc(1))
    }
}

fn create_on_file_start_var_inc() -> impl Fn(&str, u64, u64) -> Box<dyn Fn(u64)> {
    let mp = MultiProgress::new();
    move |path: &str, count: u64, completed: u64| -> Box<dyn Fn(u64)> {
        let pb = mp.add(ProgressBar::new(count));
        pb.set_position(completed);
        pb.set_style(ProgressStyle::with_template("{msg} [{bar:20}] {pos}/{len}").unwrap());
        pb.set_message(path.to_string());
        Box::new(move |inc: u64| pb.inc(inc))
    }
}
