use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize a sync directory using a static bearer token.
    ///
    /// For Keycloak-backed auth use `cloudsync login` instead. The two are
    /// mutually exclusive within one sync directory.
    Init {
        #[arg(
            long,
            env = "CLOUDSYNC_SERVER_URL",
            default_value = "http://localhost:3050"
        )]
        server_url: String,
        #[arg(long, env = "CLOUDSYNC_TOKEN", required = true)]
        token: String,
    },
    /// Sign in to the cloudsync server via Keycloak.
    ///
    /// Stores the resulting refresh+access tokens in `.cloudsync/config.toml`
    /// (0600 permissions). Auto-refreshes silently before each API call.
    /// `--mode loopback` opens a browser and catches the callback locally;
    /// `--mode device` prints a code for entry on another device (good for
    /// SSH / headless boxes).
    Login {
        #[arg(
            long,
            env = "CLOUDSYNC_SERVER_URL",
            default_value = "http://localhost:3050"
        )]
        server_url: String,
        #[arg(long, value_enum, default_value = "auto")]
        mode: LoginMode,
    },
    Push,
    Pull,
    Status,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum LoginMode {
    /// Pick automatically: loopback if a graphical session looks available,
    /// otherwise device flow. SSH sessions (SSH_TTY set) always pick device.
    Auto,
    /// Open the system browser, catch the callback on a localhost listener.
    /// Best UX on desktops. Requires a working browser.
    Loopback,
    /// Print a code + URL for the user to enter on another device. RFC 8628.
    /// Best for SSH / CI / headless. Requires the Keycloak realm's `cloudsync`
    /// client to have `oauth2.device.authorization.grant.enabled = "true"`.
    Device,
}
