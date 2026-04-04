use std::path::{Path, PathBuf};
use super::config;

pub fn scan_dir(sync_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let dirs_iter = std::fs::read_dir(sync_root)?;

    let mut changed_files: Vec<PathBuf> = Vec::new();
    for dir in dirs_iter {
        let dir = dir?;
        if dir.file_name() == config::CONFIG_DIR {
            continue;
        }
        if dir.file_type()?.is_dir() {
            let mut sub_files = scan_dir(&dir.path())?;
            changed_files.append(&mut sub_files);
            continue;
        }
        changed_files.push(dir.path());
    }
    Ok(changed_files)
}