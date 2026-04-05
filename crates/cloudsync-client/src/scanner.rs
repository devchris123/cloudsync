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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_scan_dir() {
        let tmp_dir = TempDir::new().unwrap();
        let dir0 = tmp_dir.path().join("dir0");
        let dir1 = tmp_dir.path().join("dir1");
        let subdir00 = tmp_dir.path().join("dir0").join("subdir0");
        let subdir01 = tmp_dir.path().join("dir0").join("subdir1");
        let subdir10 = tmp_dir.path().join("dir1").join("subdir2");
        let file0 = tmp_dir.path().join("file0");
        let file00 = dir0.join("file1");
        let file10 = dir1.join("file2");
        let file100 = subdir10.join("file4");

        std::fs::create_dir_all(subdir00).unwrap();
        std::fs::create_dir_all(subdir01).unwrap();
        std::fs::create_dir_all(subdir10).unwrap();
        std::fs::write(file0, b"hello world").unwrap();
        std::fs::write(file00, b"hello world").unwrap();
        std::fs::write(file10, b"hello world").unwrap();
        std::fs::write(file100, b"hello world").unwrap();

        let result = scan_dir(tmp_dir.path(), &[]).unwrap();

        assert_eq!(4, result.len());
    }

    #[test]
    fn test_scan_dir_ignores_ignorelist() {
        let tmp_dir = TempDir::new().unwrap();
        let dir0 = tmp_dir.path().join("dir0");
        std::fs::create_dir_all(&dir0).unwrap();
        std::fs::write(dir0.join("file0"), b"hello world").unwrap();
        let ignored = ["dir0".to_string()];

        let result = scan_dir(tmp_dir.path(), &ignored).unwrap();

        assert_eq!(0, result.len());
    }

    #[test]
    fn test_scan_dir_ignores_cloudsync() {
        let tmp_dir = TempDir::new().unwrap();
        let dir0 = tmp_dir.path().join(".cloudsync");
        std::fs::create_dir_all(&dir0).unwrap();
        std::fs::write(dir0.join("file0"), b"hello world").unwrap();

        let result = scan_dir(tmp_dir.path(), &[]).unwrap();

        assert_eq!(0, result.len());
    }

    #[test]
    fn test_scan_dir_empty() {
        let tmp_dir = TempDir::new().unwrap();
        let dir0 = tmp_dir.path().join("emptydir");
        std::fs::create_dir_all(&dir0).unwrap();

        let result = scan_dir(tmp_dir.path(), &[]).unwrap();

        assert_eq!(0, result.len());
    }

    #[test]
    fn test_get_ignored_returns_list() {
        let tmp_dir = TempDir::new().unwrap();
        let dir0 = tmp_dir.path().join("dir0");
        std::fs::create_dir_all(&dir0).unwrap();
        std::fs::write(dir0.join("file0"), b"hello world").unwrap();
        std::fs::write(dir0.join("file1"), b"hello world").unwrap();

        let ignore_file = tmp_dir.path().join(".cloudsyncignore");
        std::fs::write(ignore_file, b"file0\nfile1").unwrap();

        let result = get_ignored(tmp_dir.path());

        assert_eq!(2, result.len());
        assert!(result.iter().any(|f| f == "file0"));
        assert!(result.iter().any(|f| f == "file1"));
    }

    #[test]
    fn test_get_ignored_ignores_missing_file() {
        let tmp_dir = TempDir::new().unwrap();

        let result = get_ignored(tmp_dir.path());

        assert_eq!(0, result.len());
    }
}
