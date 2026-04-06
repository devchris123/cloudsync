use std::{fs::OpenOptions, io::Write};

use cloudsync_common::hash_bytes;

pub fn read(data_dir: &str, content_hash: &str) -> anyhow::Result<Vec<u8>> {
    let dir = std::path::Path::new(data_dir).join(&content_hash[0..2]);
    let path = dir.join(content_hash);
    let res = std::fs::read(path)?;
    Ok(res)
}

pub fn write(data_dir: &str, content: &[u8]) -> anyhow::Result<String> {
    let content_hash = hash_bytes(content);
    let dir = std::path::Path::new(data_dir).join(&content_hash[0..2]);
    let path = dir.join(&content_hash);
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, content)?;
    Ok(content_hash)
}

pub fn write_chunk(data_dir: &str, total_hash: &str, content: &[u8]) -> anyhow::Result<()> {
    let dir = std::path::Path::new(data_dir).join(&total_hash[0..2]);
    let path = dir.join(total_hash);
    std::fs::create_dir_all(dir)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(content)?;
    Ok(())
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

    #[test]
    fn test_write_read() {
        let dir = TempDir::new().unwrap();
        let dir = dir.path().to_str().unwrap();

        let bytes = b"hello world";
        let hash = write(dir, bytes).unwrap();

        assert_eq!(hash, hash_bytes(bytes));

        let content = read(dir, &hash).unwrap();
        assert_eq!(content.as_slice(), bytes);
    }
}
