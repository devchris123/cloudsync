use std::pin::Pin;

use cloudsync_common::{
    CreateFileResponse, DeleteFileResponse, FinalizeUploadResponse, GetUploadResponse,
    InitUploadRequest, InitUploadResponse, ListFilesResponse,
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncRead;

#[derive(Serialize, Deserialize)]
pub struct SyncRecord {
    pub path: String,
    pub local_hash: String,
    pub server_version: u64,
    pub upload_id: Option<String>,
}

pub struct DownloadFileResponse {
    pub resumed: bool,
    pub stream: Box<dyn AsyncRead + Unpin + Send>,
}

#[allow(async_fn_in_trait)]
pub trait SyncApi {
    async fn list_files(&self) -> anyhow::Result<ListFilesResponse>;
    async fn create_file(&self, path: &str, content: Vec<u8>)
    -> anyhow::Result<CreateFileResponse>;
    async fn get_file(&self, path: &str, start_bytes: u64) -> anyhow::Result<DownloadFileResponse>;
    async fn delete_file(&self, path: &str) -> anyhow::Result<DeleteFileResponse>;
    async fn init_upload(&self, request: InitUploadRequest) -> anyhow::Result<InitUploadResponse>;
    async fn send_chunk(
        &self,
        upload_id: &str,
        chunk_index: u32,
        content: Vec<u8>,
    ) -> anyhow::Result<()>;
    async fn get_upload(&self, upload_id: &str) -> anyhow::Result<GetUploadResponse>;
    async fn finalize_upload(&self, upload_id: &str) -> anyhow::Result<FinalizeUploadResponse>;
}

pub struct ProgressReader<R> {
    pub inner: R,
    pub on_bytes: Box<dyn Fn(u64)>,
}

impl<R: AsyncRead + Unpin> AsyncRead for ProgressReader<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = &result {
            let bytes_read = buf.filled().len() - before;
            (self.on_bytes)(bytes_read as u64);
        }
        result
    }
}
