use cloudsync_common::{CreateFileResponse, DeleteFileResponse, ListFilesResponse};

pub struct SyncClient {
    server_url: String,
    token: String,
    client: reqwest::Client
}

impl SyncClient {
    pub fn new( server_url: String, token: String) -> Self {
        SyncClient {
            server_url,
            token,
            client:  reqwest::Client::new()
        }
    }

    pub async fn list_files(&self) -> anyhow::Result<ListFilesResponse> {
        let resp = self.client
            .get(format!("{}/{}", self.server_url, "api/v1/files"))
            .bearer_auth(&self.token)
            .send()
            .await?;
        let bytes = resp.bytes().await?;
        Ok(serde_json::from_slice::<ListFilesResponse>(&bytes)?)
    }

    pub async fn create_file(
        &self,
        path: &str,
        content: Vec<u8>,
    ) -> anyhow::Result<CreateFileResponse> {
        let form = reqwest::multipart::Form::new()
            .text("path", path.to_string())
            .part("file", reqwest::multipart::Part::bytes(content).file_name("file"));
        let resp = self.client
            .post(format!("{}/{}", self.server_url, "api/v1/files"))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            anyhow::bail!("Server error {}: {}", status, String::from_utf8_lossy(&bytes))
        }
        Ok(serde_json::from_slice::<CreateFileResponse>(&bytes)?)
    }

    pub async fn get_file(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let resp = self.client
            .get(format!("{}/{}/{}", self.server_url, "api/v1/files", path))
            .bearer_auth(&self.token)
            .send()
            .await?;
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    }

    pub async fn delete_file(&self, path: &str) -> anyhow::Result<DeleteFileResponse> {
        let resp = self.client
            .delete(format!("{}/{}/{}", self.server_url, "api/v1/files", path))
            .bearer_auth(&self.token)
            .send()
            .await?;
        let bytes = resp.bytes().await?;
        Ok(serde_json::from_slice::<DeleteFileResponse>(&bytes)?)
    }
}
