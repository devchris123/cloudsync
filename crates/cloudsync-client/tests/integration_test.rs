// tests/integration_test.rs

use cloudsync_client::{
    client::SyncClient,
    db::open_db,
    sync::{self, CHUNK_SIZE},
};
use cloudsync_server::config::ServerConfig;
use tokio::{self, net::TcpListener};

#[tokio::test]
async fn test_push_and_pull() {
    // start server
    let token = "HELLO";
    let storage_dir = tempfile::TempDir::new().unwrap();
    let storage_dir_str = storage_dir.path().to_str().unwrap().to_string();
    let staging_dir_str = storage_dir
        .path()
        .join("staging")
        .to_str()
        .unwrap()
        .to_string();
    let dbname = storage_dir
        .path()
        .join("server.redb")
        .to_str()
        .unwrap()
        .to_string();
    let server = cloudsync_server::app::bootstrap_app(ServerConfig {
        storage_dir: storage_dir_str,
        staging_dir: staging_dir_str,
        token: token.to_string(),
        dbname: dbname.to_string(),
    })
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });

    let client_root_dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(client_root_dir.path().join(".cloudsync")).unwrap();
    let db = open_db(client_root_dir.path()).unwrap();
    let sync_client = SyncClient::new(&format!("http://{addr}"), token.to_string());

    // create files
    let file0 = client_root_dir.path().join("file0");
    let file1 = client_root_dir.path().join("file1");
    let file2 = client_root_dir.path().join("file2");
    let file3 = client_root_dir.path().join("file3");
    let bytes0 = b"hello world";
    let bytes1 = vec![0u8; 10 * 1024 * 1024];
    let bytes2 = vec![0u8; CHUNK_SIZE as usize];
    let bytes3: Vec<u8> = vec![];
    std::fs::write(&file0, bytes0).unwrap();
    std::fs::write(&file1, &bytes1).unwrap();
    std::fs::write(&file2, &bytes2).unwrap();
    std::fs::write(&file3, &bytes3).unwrap();

    // push
    sync::push(&db, &sync_client, client_root_dir.path(), &noop_progress())
        .await
        .unwrap();

    // pull from different dir
    let other_client_root_dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(other_client_root_dir.path().join(".cloudsync")).unwrap();
    let other_db = open_db(other_client_root_dir.path()).unwrap();
    sync::pull(&other_db, &sync_client, other_client_root_dir.path())
        .await
        .unwrap();

    // assert
    let pulled_bytes0 = std::fs::read(other_client_root_dir.path().join("file0")).unwrap();
    let pulled_bytes1 = std::fs::read(other_client_root_dir.path().join("file1")).unwrap();
    let pulled_bytes2 = std::fs::read(other_client_root_dir.path().join("file2")).unwrap();
    let pulled_bytes3 = std::fs::read(other_client_root_dir.path().join("file3")).unwrap();

    assert_eq!(bytes0, pulled_bytes0.as_slice());
    assert_eq!(bytes1, pulled_bytes1.as_slice());
    assert_eq!(bytes2, pulled_bytes2.as_slice());
    assert_eq!(bytes3, pulled_bytes3.as_slice());
}

#[tokio::test]
async fn test_pull_conflict() {
    // start server
    let token = "HELLO";
    let storage_dir = tempfile::TempDir::new().unwrap();
    let storage_dir_str = storage_dir.path().to_str().unwrap().to_string();
    let staging_dir_str = storage_dir
        .path()
        .join("staging")
        .to_str()
        .unwrap()
        .to_string();
    let dbname = storage_dir
        .path()
        .join("server.redb")
        .to_str()
        .unwrap()
        .to_string();
    let server = cloudsync_server::app::bootstrap_app(ServerConfig {
        storage_dir: storage_dir_str,
        staging_dir: staging_dir_str,
        token: token.to_string(),
        dbname: dbname.to_string(),
    })
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });

    let client_root_dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(client_root_dir.path().join(".cloudsync")).unwrap();
    let db = open_db(client_root_dir.path()).unwrap();
    let sync_client = SyncClient::new(&format!("http://{addr}"), token.to_string());

    // create files
    let file0 = client_root_dir.path().join("file0");
    let file1 = client_root_dir.path().join("file1");

    let bytes0 = b"hello world";
    let bytes1 = b"hello world2";
    std::fs::write(&file0, bytes0).unwrap();
    std::fs::write(&file1, bytes1).unwrap();

    // push
    sync::push(&db, &sync_client, client_root_dir.path(), &noop_progress())
        .await
        .unwrap();

    // Modify locally
    std::fs::write(&file0, b"hello world client changed").unwrap();

    // pull from different dir
    let other_client_root_dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(other_client_root_dir.path().join(".cloudsync")).unwrap();
    let other_db = open_db(other_client_root_dir.path()).unwrap();
    sync::pull(&other_db, &sync_client, other_client_root_dir.path())
        .await
        .unwrap();
    std::fs::write(
        other_client_root_dir.path().join("file0"),
        "hello world other client changed",
    )
    .unwrap();
    sync::push(
        &other_db,
        &sync_client,
        other_client_root_dir.path(),
        &noop_progress(),
    )
    .await
    .unwrap();

    // Pull with first client again
    sync::pull(&db, &sync_client, client_root_dir.path())
        .await
        .unwrap();

    // assert
    let bytes0 = std::fs::read(client_root_dir.path().join("file0")).unwrap();
    let bytes1 = std::fs::read(client_root_dir.path().join("file1")).unwrap();
    assert_eq!(b"hello world client changed", bytes0.as_slice());
    assert_eq!(b"hello world2", bytes1.as_slice());
    let conflict_exist = std::fs::read_dir(client_root_dir.path()).unwrap().any(|f| {
        f.unwrap()
            .file_name()
            .into_string()
            .unwrap()
            .contains("file0.conflict")
    });
    assert!(conflict_exist);
}

fn noop_progress() -> impl Fn(&str, u64) -> Box<dyn Fn()> {
    |_: &str, _: u64| -> Box<dyn Fn()> { Box::new(|| {}) }
}
