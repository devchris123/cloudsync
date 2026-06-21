use cloudsync_common::{
    CreateFileResponse, DeleteFileResponse, FinalizeUploadResponse, GetUploadResponse,
    InitUploadRequest, InitUploadResponse, ListFilesResponse,
};
use reqwest::StatusCode;
use tokio::sync::Mutex;

use crate::auth::TokenSource;
use crate::sync::{DownloadFileResponse, SyncApi};
use futures::TryStreamExt;

pub struct SyncClient {
    server_url: String,
    // Held behind a Mutex so we can keep `&self` on the SyncApi methods while
    // letting the OIDC variant (lands in step 4) mutate cached tokens during
    // refresh. The legacy Static variant doesn't touch state, but the lock is
    // cheap and uncontended in practice — sync runs one HTTP request at a time
    // per file and the lock is released before await.
    token_source: Mutex<TokenSource>,
    client: reqwest::Client,
}

impl SyncClient {
    pub fn new(server_url: &str, token: String) -> Self {
        Self::with_source(server_url, TokenSource::Static { token })
    }

    pub fn with_source(server_url: &str, source: TokenSource) -> Self {
        SyncClient {
            server_url: server_url.to_string(),
            token_source: Mutex::new(source),
            client: reqwest::Client::new(),
        }
    }

    /// Resolve a fresh bearer token for outbound requests. For the static
    /// variant this is a clone; for OIDC it may trigger a refresh roundtrip.
    async fn bearer(&self) -> anyhow::Result<String> {
        let mut guard = self.token_source.lock().await;
        guard.access_token().await
    }

    pub async fn health(&self) -> anyhow::Result<()> {
        let url = format!("{}/health", self.server_url);
        self.client.get(url).send().await?;
        Ok(())
    }
}

impl SyncApi for SyncClient {
    async fn list_files(&self) -> anyhow::Result<ListFilesResponse> {
        let url = format!("{}/{}", self.server_url, "api/v1/files");
        tracing::debug!("request: {} {}", "get", &url);
        let token = self.bearer().await?;
        let resp = self.client.get(url).bearer_auth(token).send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Server error {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            )
        }
        Ok(serde_json::from_slice::<ListFilesResponse>(&bytes)?)
    }

    async fn create_file(
        &self,
        path: &str,
        content: Vec<u8>,
    ) -> anyhow::Result<CreateFileResponse> {
        let form = reqwest::multipart::Form::new()
            .text("path", path.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(content).file_name("file"),
            );
        let url = format!("{}/{}", self.server_url, "api/v1/files");
        tracing::debug!("request: {} {}", "post", &url);
        let token = self.bearer().await?;
        let resp = self
            .client
            .post(url)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Server error {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            )
        }
        Ok(serde_json::from_slice::<CreateFileResponse>(&bytes)?)
    }

    async fn get_file(&self, path: &str, start_bytes: u64) -> anyhow::Result<DownloadFileResponse> {
        let url = format!("{}/{}/{}", self.server_url, "api/v1/files", path);
        tracing::debug!("request: {} {}", "get", &url);

        let token = self.bearer().await?;
        let mut req = self.client.get(url).bearer_auth(token);
        if start_bytes > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={start_bytes}-"));
        }
        let resp = req.send().await?;

        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!(
                "Server error {}: {}",
                status,
                String::from_utf8_lossy(&resp.bytes().await?)
            )
        }
        let bytes_stream = resp.bytes_stream();
        let stream = bytes_stream.map_err(std::io::Error::other);
        let reader = tokio_util::io::StreamReader::new(stream);

        Ok(DownloadFileResponse {
            resumed: status == StatusCode::PARTIAL_CONTENT,
            stream: Box::new(reader),
        })
    }

    async fn delete_file(&self, path: &str) -> anyhow::Result<DeleteFileResponse> {
        let url = format!("{}/{}/{}", self.server_url, "api/v1/files", path);
        tracing::debug!("request: {} {}", "delete", &url);
        let token = self.bearer().await?;
        let resp = self.client.delete(url).bearer_auth(token).send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Server error {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            )
        }
        Ok(serde_json::from_slice::<DeleteFileResponse>(&bytes)?)
    }

    async fn init_upload(&self, request: InitUploadRequest) -> anyhow::Result<InitUploadResponse> {
        // Init upload
        let url = format!("{}/api/v1/uploads", self.server_url);
        let token = self.bearer().await?;
        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        let resp_bytes = response.bytes().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Server error {}: {}",
                status,
                String::from_utf8_lossy(&resp_bytes)
            )
        }
        let resp = serde_json::from_slice::<InitUploadResponse>(&resp_bytes)?;
        Ok(resp)
    }

    async fn send_chunk(
        &self,
        upload_id: &str,
        chunk_index: u32,
        content: Vec<u8>,
    ) -> anyhow::Result<()> {
        let url = format!(
            "{}/api/v1/uploads/{}/chunks/{}",
            self.server_url, upload_id, chunk_index
        );
        let token = self.bearer().await?;
        let resp = self
            .client
            .put(url)
            .bearer_auth(token)
            .body(content)
            .send()
            .await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Server error {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            )
        }
        Ok(())
    }

    async fn get_upload(&self, upload_id: &str) -> anyhow::Result<GetUploadResponse> {
        // Get status
        let url = format!("{}/api/v1/uploads/{}", self.server_url, upload_id);
        let token = self.bearer().await?;
        let resp = self.client.get(url).bearer_auth(token).send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Server error {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            )
        }
        Ok(serde_json::from_slice::<GetUploadResponse>(&bytes)?)
    }

    async fn finalize_upload(&self, upload_id: &str) -> anyhow::Result<FinalizeUploadResponse> {
        // finalize upload
        let url = format!("{}/api/v1/uploads/{}/finalize", self.server_url, upload_id);
        let token = self.bearer().await?;
        let resp = self.client.post(url).bearer_auth(token).send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Server error {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            )
        }
        let file = serde_json::from_slice::<FinalizeUploadResponse>(&bytes)?;
        Ok(file)
    }
}
