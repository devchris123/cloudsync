mod app;
mod db;
mod storage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = app::bootstrap_app().unwrap();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3050").await.unwrap();
    axum::serve(listener, app).await.unwrap();
    Ok(())
}
