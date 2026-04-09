use cloudsync_common::hash_bytes;

pub fn write(data_dir: &str, content: &[u8]) -> anyhow::Result<String> {
    let content_hash = hash_bytes(content);
    let dir = std::path::Path::new(data_dir).join(&content_hash[0..2]);
    let path = dir.join(&content_hash);
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, content)?;
    Ok(content_hash)
}

pub async fn read_async(data_dir: &str, content_hash: &str) -> anyhow::Result<tokio::fs::File> {
    let dir = std::path::Path::new(data_dir).join(&content_hash[0..2]);
    let path = dir.join(content_hash);
    let file = tokio::fs::File::open(path).await?;
    Ok(file)
}

pub fn get_storage_path(data_dir: &str, total_hash: &str) -> std::path::PathBuf {
    std::path::Path::new(data_dir)
        .join(&total_hash[0..2])
        .join(total_hash)
}

#[cfg(test)]
mod test {
    use super::*;
    use tempfile::TempDir;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn test_write_read() {
        let dir = TempDir::new().unwrap();
        let dir = dir.path().to_str().unwrap();

        let bytes = b"hello world";
        let hash = write(dir, bytes).unwrap();

        assert_eq!(hash, hash_bytes(bytes));

        let file = read_async(dir, &hash).await.unwrap();
        let mut buf = Vec::new();
        file.take(1000).read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf.as_slice(), bytes);
    }
}
