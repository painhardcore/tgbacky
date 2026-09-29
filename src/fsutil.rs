use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Utc};
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::error::{AppError, Result};
use crate::types::MediaKind;

pub fn slugify_chat_title(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut prev_sep = false;
    for ch in value.chars() {
        if ch.is_alphanumeric() {
            for lowered in ch.to_lowercase() {
                output.push(lowered);
            }
            prev_sep = false;
        } else if !prev_sep {
            output.push('_');
            prev_sep = true;
        }
    }

    let slug = output.trim_matches('_').to_string();
    if slug.is_empty() {
        "chat".to_string()
    } else {
        slug
    }
}

pub fn build_media_directory(
    output_root: &Path,
    chat_slug: &str,
    kind: MediaKind,
    message_date: DateTime<Utc>,
) -> PathBuf {
    output_root
        .join(chat_slug)
        .join(kind.bucket_dir())
        .join(format!("{:04}", message_date.year()))
        .join(format!("{:02}", message_date.month()))
}

pub async fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    Ok(())
}

pub async fn compute_sha256_async(path: &Path) -> Result<String> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || compute_sha256_sync(&owned))
        .await
        .map_err(|error| AppError::Runtime(format!("hash task failed: {error}")))?
}

pub async fn cleanup_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Absolute form of `path`: canonical when it exists, otherwise with `.` and `..` resolved
/// lexically, so paths compare equal regardless of the working directory.
pub fn normalize_path(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    if absolute.exists() {
        Ok(absolute.canonicalize()?)
    } else {
        Ok(normalize_lexically(absolute))
    }
}

fn normalize_lexically(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

pub fn temp_sidecar_path(final_path: &Path, temp_extension: &str) -> PathBuf {
    let file_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    final_path.with_file_name(format!("{file_name}{temp_extension}"))
}

pub async fn write_utf8_file(path: &Path, contents: &str) -> Result<()> {
    ensure_parent_dir(path).await?;
    fs::write(path, contents).await?;
    Ok(())
}

pub async fn move_atomic(from: &Path, to: &Path) -> Result<()> {
    ensure_parent_dir(to).await?;
    match fs::rename(from, to).await {
        Ok(()) => Ok(()),
        Err(error) if is_cross_device(&error) => {
            let src = from.to_path_buf();
            let dst = to.to_path_buf();
            tokio::task::spawn_blocking(move || copy_then_replace_sync(&src, &dst))
                .await
                .map_err(|error| AppError::Runtime(format!("move task failed: {error}")))?
        }
        Err(error) => Err(error.into()),
    }
}

fn compute_sha256_sync(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn copy_then_replace_sync(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut source = File::open(from)?;
    let mut target = File::create(to)?;
    std::io::copy(&mut source, &mut target)?;
    target.flush()?;
    target.sync_all()?;
    drop(target);
    std::fs::remove_file(from)?;
    sync_parent_dir(to);
    Ok(())
}

fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
}

fn is_cross_device(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::CrossesDevices)
        || matches!(error.raw_os_error(), Some(18))
}

/// Writes `contents` to a file that only the owner can read or write.
pub fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = private_open_options().truncate(true).open(path)?;
    restrict_to_owner(path)?;
    file.write_all(contents)?;
    Ok(())
}

/// Makes a SQLite database and its WAL/SHM files owner-only. The database file is
/// created first because SQLite copies its mode to the sidecar files it creates.
pub fn restrict_sqlite_files(path: &Path) -> Result<()> {
    private_open_options().open(path)?;
    for suffix in ["", "-wal", "-shm"] {
        let mut file_name = path.as_os_str().to_owned();
        file_name.push(suffix);
        let file = PathBuf::from(file_name);
        if file.exists() {
            restrict_to_owner(&file)?;
        }
    }
    Ok(())
}

fn private_open_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugifies_chat_titles() {
        assert_eq!(slugify_chat_title("Memes Channel"), "memes_channel");
        assert_eq!(
            slugify_chat_title("Пятисотые на проде"),
            "пятисотые_на_проде"
        );
        assert_eq!(slugify_chat_title("***"), "chat");
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn restricts_new_and_existing_sqlite_files_to_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("state.db");
        let wal = dir.path().join("state.db-wal");
        std::fs::write(&wal, b"").expect("wal");
        std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        restrict_sqlite_files(&db).expect("restrict");

        assert_eq!(mode(&db), 0o600);
        assert_eq!(mode(&wal), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn private_file_is_owner_only_even_when_it_existed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("secret.json");
        std::fs::write(&path, b"old contents that are longer").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        write_private_file(&path, b"new").expect("write");

        assert_eq!(mode(&path), 0o600);
        assert_eq!(std::fs::read(&path).expect("read"), b"new");
    }

    #[test]
    fn normalizes_missing_relative_paths_lexically() {
        let normalized = normalize_path(Path::new("no-such-dir/./a/../b")).expect("normalize");
        let expected = std::env::current_dir().expect("cwd").join("no-such-dir/b");
        assert_eq!(normalized, expected);
    }
}
