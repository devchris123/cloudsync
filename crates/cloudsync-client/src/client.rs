use cloudsync_common::{
    CreateFileResponse, DeleteFileResponse, FinalizeUploadResponse, GetUploadResponse,
    InitUploadRequest, InitUploadResponse, ListFilesResponse,
};

use crate::sync::SyncApi;

pub struct SyncClient {
    server_url: String,
    token: String,
    client: reqwest::Client,
}

impl SyncClient {
    pub fn new(server_url: &str, token: String) -> Self {
        SyncClient {
            server_url: server_url.to_string(),
            token,
            client: reqwest::Client::new(),
        }
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
        let resp = self.client.get(url).bearer_auth(&self.token).send().await?;
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
        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.token)
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

    async fn get_file(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let url = format!("{}/{}/{}", self.server_url, "api/v1/files", path);
        tracing::debug!("request: {} {}", "get", &url);
        let resp = self.client.get(url).bearer_auth(&self.token).send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Server error {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            )
        }
        Ok(bytes.to_vec())
    }

    async fn delete_file(&self, path: &str) -> anyhow::Result<DeleteFileResponse> {
        let url = format!("{}/{}/{}", self.server_url, "api/v1/files", path);
        tracing::debug!("request: {} {}", "delete", &url);
        let resp = self
            .client
            .delete(url)
            .bearer_auth(&self.token)
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
        Ok(serde_json::from_slice::<DeleteFileResponse>(&bytes)?)
    }

    async fn init_upload(&self, request: InitUploadRequest) -> anyhow::Result<InitUploadResponse> {
        // Init upload
        let url = format!("{}/api/v1/uploads", self.server_url);
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.token)
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
        let resp = self
            .client
            .put(url)
            .bearer_auth(&self.token)
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
        let resp = self.client.get(url).bearer_auth(&self.token).send().await?;
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
        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.token)
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
        let file = serde_json::from_slice::<FinalizeUploadResponse>(&bytes)?;
        Ok(file)
    }
}
