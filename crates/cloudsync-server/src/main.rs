use clap::Parser;

mod app;
mod cli;
mod db;
mod storage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let app = app::bootstrap_app(args.storage_dir, args.token, args.dbname).unwrap();
    let listener = tokio::net::TcpListener::bind(format!("{}:{}", args.host, args.port))
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
    Ok(())
}
