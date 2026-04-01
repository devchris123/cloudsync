use cloudsync_common::hash_bytes;

use crate::DATA_DIR;

pub fn read(content_hash: &str) -> anyhow::Result<Vec<u8>> {
    let dir = std::path::Path::new(DATA_DIR).join(&content_hash[0..2]);
    let path = dir.join(content_hash);
    let res = std::fs::read(path)?;
    Ok(res)
}

pub fn write(content: &[u8]) -> anyhow::Result<String> {
    let content_hash = hash_bytes(content);
    let dir = std::path::Path::new(DATA_DIR).join(&content_hash[0..2]);
    let path = dir.join(&content_hash);
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, content)?;
    Ok(content_hash)
}
