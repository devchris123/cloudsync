use super::config;
use std::path::{Path, PathBuf};

const IGNORE_FILE: &str = ".cloudsyncignore";

pub fn scan_dir(sync_root: &Path, ignored: &[String]) -> anyhow::Result<Vec<PathBuf>> {
    let dirs_iter = std::fs::read_dir(sync_root)?;

    let mut changed_files: Vec<PathBuf> = Vec::new();
    for dir in dirs_iter {
        let dir = dir?;
        if dir.file_name() == config::CONFIG_DIR {
            continue;
        }
        if ignored.iter().any(|f| f.as_str() == dir.file_name()) {
            continue;
        }
        if dir.file_type()?.is_dir() {
            let mut sub_files = scan_dir(&dir.path(), ignored)?;
            changed_files.append(&mut sub_files);
            continue;
        }
        changed_files.push(dir.path());
    }
    Ok(changed_files)
}

pub fn get_ignored(sync_root: &Path) -> Vec<String> {
    let path = sync_root.join(IGNORE_FILE);
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content.lines().map(|l| l.to_string()).collect()
}
