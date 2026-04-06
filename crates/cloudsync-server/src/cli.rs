use clap::Parser;

#[derive(Parser)]
pub struct Args {
    #[arg(long, env = "CLOUDSYNC_HOST", default_value = "127.0.0.1")]
    pub host: String,
    #[arg(long, env = "CLOUDSYNC_PORT", default_value = "3050")]
    pub port: u16,
    #[arg(long, env = "CLOUDSYNC_TOKEN")]
    pub token: String,
    #[arg(
        long,
        env = "CLOUDSYNC_STORAGE_DIR",
        default_value = "cloudsync/data/files"
    )]
    pub storage_dir: String,
    #[arg(
        long,
        env = "CLOUDSYNC_STAGING_DIR",
        default_value = "cloudsync/data/staging"
    )]
    pub staging_dir: String,
    #[arg(long, env = "CLOUDSYNC_DBNAME", default_value = "data.redb")]
    pub dbname: String,
}
