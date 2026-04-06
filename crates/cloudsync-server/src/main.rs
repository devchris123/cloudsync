use clap::Parser;

mod cli;
use cloudsync_server::app;
use cloudsync_server::config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let config = config::ServerConfig {
        storage_dir: args.storage_dir,
        staging_dir: args.staging_dir,
        token: args.token,
        dbname: args.dbname,
    };
    let app = app::bootstrap_app(config).unwrap();
    let listener = tokio::net::TcpListener::bind(format!("{}:{}", args.host, args.port))
        .await
        .unwrap();
    tracing::info!("server listening on {}:{}", args.host, args.port);
    axum::serve(listener, app).await.unwrap();
    Ok(())
}
