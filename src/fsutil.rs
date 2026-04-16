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
}
