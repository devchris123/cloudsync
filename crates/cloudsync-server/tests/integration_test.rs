// tests/integration_test.rs

use cloudsync_common::{GetUploadResponse, InitUploadRequest, InitUploadResponse, hash_bytes};
use cloudsync_server::config::ServerConfig;
use reqwest::StatusCode;
use tokio::{self, net::TcpListener};

#[tokio::test]
async fn test_range_download() {
    // start server
    let token = "HELLO";
    let storage_dir = tempfile::TempDir::new().unwrap();
    let staging_dir = storage_dir.path().join("staging");
    let storage_dir_str = storage_dir.path().to_str().unwrap().to_string();
    let dbname = storage_dir
        .path()
        .join("server.redb")
        .to_str()
        .unwrap()
        .to_string();
    let server = cloudsync_server::app::bootstrap_app(ServerConfig {
        storage_dir: storage_dir_str,
        staging_dir: staging_dir.to_str().unwrap().to_string(),
        token: token.to_string(),
        dbname: dbname.to_string(),
    })
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });

    // Initial upload
    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();
    let file_bytes = b"abc";
    let form = reqwest::multipart::Form::new()
        .text("path", "my/file")
        .part("file", reqwest::multipart::Part::bytes(file_bytes));
    let response = client
        .post(format!("{base_url}/api/v1/files"))
        .bearer_auth(token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(status, StatusCode::OK);
    let response = client
        .get(format!("{base_url}/api/v1/files/my/file"))
        .bearer_auth(token)
        .header("Range", "bytes=2-")
        .send()
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"c");
}

#[tokio::test]
async fn test_chunked_upload() {
    // start server
    let token = "HELLO";
    let storage_dir = tempfile::TempDir::new().unwrap();
    let staging_dir = storage_dir.path().join("staging");
    let storage_dir_str = storage_dir.path().to_str().unwrap().to_string();
    let dbname = storage_dir
        .path()
        .join("server.redb")
        .to_str()
        .unwrap()
        .to_string();
    let server = cloudsync_server::app::bootstrap_app(ServerConfig {
        storage_dir: storage_dir_str,
        staging_dir: staging_dir.to_str().unwrap().to_string(),
        token: token.to_string(),
        dbname: dbname.to_string(),
    })
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });

    // Init upload
    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();
    let file_bytes = b"abc";
    let response = client
        .post(format!("{base_url}/api/v1/uploads"))
        .bearer_auth(token)
        .json(&InitUploadRequest {
            path: "my/file".to_string(),
            total_size: 3,
            total_hash: hash_bytes(file_bytes),
            chunk_count: 3,
        })
        .send()
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(status, StatusCode::OK);
    let resp_bytes = response.bytes().await.unwrap();
    let upload = serde_json::from_slice::<InitUploadResponse>(&resp_bytes).unwrap();

    // Send 2 chunks
    for i in 0..2 {
        let body = vec![file_bytes[i]];
        let response = client
            .put(format!(
                "{base_url}/api/v1/uploads/{}/chunks/{}",
                upload.upload_id, i
            ))
            .bearer_auth(token)
            .body(body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        assert_eq!(status, StatusCode::OK);
    }

    // Get status
    let response = client
        .get(format!("{base_url}/api/v1/uploads/{}", upload.upload_id))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let bytes = response.bytes().await.unwrap();
    let status = serde_json::from_slice::<GetUploadResponse>(bytes.to_vec().as_slice()).unwrap();
    assert!(status.upload.chunks_received.iter().any(|ch| *ch == 0));
    assert!(status.upload.chunks_received.iter().any(|ch| *ch == 1));

    // Accidentally finalize before sending final chunk
    let response = client
        .post(format!(
            "{base_url}/api/v1/uploads/{}/finalize",
            upload.upload_id
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Send final chunk
    let body = vec![file_bytes[2]];
    let response = client
        .put(format!(
            "{base_url}/api/v1/uploads/{}/chunks/2",
            upload.upload_id
        ))
        .bearer_auth(token)
        .body(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(status, StatusCode::OK);

    // finalize upload
    let response = client
        .post(format!(
            "{base_url}/api/v1/uploads/{}/finalize",
            upload.upload_id
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(status, StatusCode::OK);

    let response = client
        .get(format!("{base_url}/api/v1/files/my/file"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(status, StatusCode::OK);
    let bytes = response.bytes().await.unwrap();
    assert_eq!(bytes.to_vec().as_slice(), b"abc");
}
