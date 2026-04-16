use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct StaleTempFile {
    pub path: PathBuf,
    pub age: Duration,
}

#[derive(Debug, Clone)]
pub struct RecoverySummary {
    pub root: PathBuf,
    pub scanned_files: usize,
    pub stale_found: usize,
    pub removed: usize,
    pub unreadable_entries: usize,
}

#[derive(Debug, Clone)]
pub struct RecoveryScan {
    pub stale_files: Vec<StaleTempFile>,
    pub scanned_files: usize,
    pub unreadable_entries: usize,
}

pub async fn cleanup_stale_temp_files(
    root: &Path,
    temp_extension: &str,
    min_age: Duration,
    remove: bool,
) -> Result<RecoverySummary> {
    let scan = scan_stale_temp_files(root, temp_extension, min_age)?;
    let removed = if remove {
        remove_paths(&scan.stale_files).await?
    } else {
        0
    };

    Ok(RecoverySummary {
        root: root.to_path_buf(),
        scanned_files: scan.scanned_files,
        stale_found: scan.stale_files.len(),
        removed,
        unreadable_entries: scan.unreadable_entries,
    })
}

pub fn find_stale_temp_files(
    root: &Path,
    temp_extension: &str,
    min_age: Duration,
) -> Result<Vec<StaleTempFile>> {
    Ok(scan_stale_temp_files(root, temp_extension, min_age)?.stale_files)
}

pub fn scan_stale_temp_files(
    root: &Path,
    temp_extension: &str,
    min_age: Duration,
) -> Result<RecoveryScan> {
    if !root.exists() {
        return Ok(RecoveryScan {
            stale_files: Vec::new(),
            scanned_files: 0,
            unreadable_entries: 0,
        });
    }

    let now = SystemTime::now();
    let mut pending = VecDeque::from([root.to_path_buf()]);
    let mut stale = Vec::new();
    let mut scanned_files = 0;
    let mut unreadable_entries = 0;

    while let Some(path) = pending.pop_front() {
        let read_dir = match std::fs::read_dir(&path) {
            Ok(read_dir) => read_dir,
            Err(_) => {
                unreadable_entries += 1;
                continue;
            }
        };
        for entry in read_dir {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    unreadable_entries += 1;
                    continue;
                }
            };
            let entry_path = entry.path();
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => {
                    unreadable_entries += 1;
                    continue;
                }
            };
            if metadata.is_dir() {
                pending.push_back(entry_path);
                continue;
            }
            scanned_files += 1;

            let Some(name) = entry_path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if !name.ends_with(temp_extension) {
                continue;
            }

            let modified = metadata.modified().unwrap_or(now);
            let age = now.duration_since(modified).unwrap_or_default();
            if age >= min_age {
                stale.push(StaleTempFile {
                    path: entry_path,
                    age,
                });
            }
        }
    }

    stale.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(RecoveryScan {
        stale_files: stale,
        scanned_files,
        unreadable_entries,
    })
}

async fn remove_paths(paths: &[StaleTempFile]) -> Result<usize> {
    let mut removed = 0;
    for stale in paths {
        match tokio::fs::remove_file(&stale.path).await {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_stale_temp_files() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let stale = tempdir.path().join("old.part");
        let fresh = tempdir.path().join("new.part");
        std::fs::write(&stale, b"old").expect("write stale");
        std::fs::write(&fresh, b"new").expect("write fresh");

        let found = scan_stale_temp_files(tempdir.path(), ".part", Duration::ZERO).expect("scan");
        assert_eq!(found.stale_files.len(), 2);
        assert_eq!(found.scanned_files, 2);
    }
}
