use clap::{Parser, Subcommand};

#[derive(Parser)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
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
    Push,
    Pull,
    Status,
}
