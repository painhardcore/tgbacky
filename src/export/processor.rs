use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::time::sleep;
use tracing::{info, warn};

use crate::config::AppConfig;
use crate::error::{AppError, Result};
use crate::fsutil::{
    build_media_directory, cleanup_file_if_exists, compute_sha256_async, ensure_parent_dir,
    move_atomic, slugify_chat_title,
};
use crate::media::{build_filename, choose_extension, stable_suffix};
use crate::shutdown::ShutdownFlag;
use crate::storage::{Database, PersistedMediaItem, StoredMediaRecord};
use crate::telegram::TelegramGateway;
use crate::types::{MediaDescriptor, MediaKind, MediaStatus, ScannedMessage};

pub(crate) struct MessageProcessorParams<'a> {
    pub(crate) config: &'a AppConfig,
    pub(crate) media_filter: BTreeSet<MediaKind>,
    pub(crate) chat_id: i64,
    pub(crate) chat_title: &'a str,
    pub(crate) output_root: &'a Path,
}

pub(crate) struct PlannedMessage<H> {
    pub(crate) initial_records: Vec<PersistedMediaItem>,
    pub(crate) jobs: Vec<DownloadJob<H>>,
}

#[derive(Clone)]
pub(crate) struct DownloadJob<H> {
    pub(crate) message_id: i32,
    pub(crate) kind: MediaKind,
    pub(crate) handle: H,
    pub(crate) scheduler_retry_round: u8,
    pub(crate) pending_record: PersistedMediaItem,
    temp_path: PathBuf,
}

pub(crate) struct MessageProcessor<'a> {
    config: &'a AppConfig,
    media_filter: BTreeSet<MediaKind>,
    chat_id: i64,
    chat_slug: String,
    output_root: &'a Path,
}

impl<'a> MessageProcessor<'a> {
    pub(crate) fn new(params: MessageProcessorParams<'a>) -> Self {
        Self {
            config: params.config,
            media_filter: params.media_filter,
            chat_id: params.chat_id,
            chat_slug: slugify_chat_title(params.chat_title),
            output_root: params.output_root,
        }
    }

    pub(crate) async fn plan_message<H: Clone + Send + Sync + 'static>(
        &self,
        database: &Database,
        message: &ScannedMessage<H>,
    ) -> Result<PlannedMessage<H>> {
        let mut initial_records = Vec::new();
        let mut jobs = Vec::new();

        for media in &message.media {
            if !self.media_filter.contains(&media.kind) {
                continue;
            }

            match self.plan_media(database, message, media).await? {
                MediaPlan::Skip(record) => initial_records.push(record),
                MediaPlan::Queue(job) => {
                    initial_records.push(job.pending_record.clone());
                    jobs.push(job);
                }
            }
        }

        Ok(PlannedMessage {
            initial_records,
            jobs,
        })
    }

    pub(crate) fn includes_kind(&self, kind: MediaKind) -> bool {
        self.media_filter.contains(&kind)
    }

    pub(crate) fn chat_id(&self) -> i64 {
        self.chat_id
    }

    async fn plan_media<H: Clone + Send + Sync + 'static>(
        &self,
        database: &Database,
        message: &ScannedMessage<H>,
        media: &MediaDescriptor<H>,
    ) -> Result<MediaPlan<H>> {
        if let Some(existing) = database.find_media_by_key(
            self.chat_id,
            message.message_id,
            &media.telegram_media_key,
        )? && matches!(
            existing.status,
            MediaStatus::Downloaded | MediaStatus::SkippedExisting
        ) && let Some(verified) = self.verify_existing_record(&existing).await?
        {
            return Ok(MediaPlan::Skip(PersistedMediaItem {
                chat_id: self.chat_id,
                message_id: message.message_id,
                message_date: message.date,
                kind: media.kind,
                telegram_media_key: media.telegram_media_key.clone(),
                mime_type: media.mime_type.clone(),
                file_size_bytes: Some(verified.file_size_bytes),
                local_path: existing.local_path,
                sha256: existing.sha256,
                status: MediaStatus::SkippedExisting,
                error_message: None,
            }));
        }

        let extension = choose_extension(
            media.original_name.as_deref(),
            media.mime_type.as_deref(),
            media.kind,
        );
        let file_name = build_filename(
            message.message_id,
            message.date.date_naive(),
            media.kind,
            media.original_name.as_deref(),
            &media.telegram_media_key,
            &extension,
            self.config.filename_mode,
        );
        let directory =
            build_media_directory(self.output_root, &self.chat_slug, media.kind, message.date);
        let base_path = directory.join(file_name);
        let final_path = self
            .resolve_available_target_path(database, &base_path, &media.telegram_media_key)
            .await?;
        let temp_path = temp_path_for(&final_path, &self.config.temp_extension);

        Ok(MediaPlan::Queue(DownloadJob {
            message_id: message.message_id,
            kind: media.kind,
            handle: media.handle.clone(),
            scheduler_retry_round: 0,
            pending_record: PersistedMediaItem {
                chat_id: self.chat_id,
                message_id: message.message_id,
                message_date: message.date,
                kind: media.kind,
                telegram_media_key: media.telegram_media_key.clone(),
                mime_type: media.mime_type.clone(),
                file_size_bytes: media.file_size_bytes,
                local_path: final_path,
                sha256: None,
                status: MediaStatus::Pending,
                error_message: None,
            },
            temp_path,
        }))
    }

    async fn verify_existing_record(
        &self,
        record: &StoredMediaRecord,
    ) -> Result<Option<VerifiedExistingRecord>> {
        if !record.local_path.exists() {
            return Ok(None);
        }
        let Some(expected_sha) = record.sha256.as_deref() else {
            return Ok(None);
        };
        let actual_sha = compute_sha256_async(&record.local_path).await?;
        if actual_sha != expected_sha {
            return Ok(None);
        }
        let file_size_bytes = i64_file_size(tokio::fs::metadata(&record.local_path).await?.len())?;
        Ok(Some(VerifiedExistingRecord { file_size_bytes }))
    }

    async fn resolve_available_target_path(
        &self,
        database: &Database,
        base_path: &Path,
        telegram_media_key: &str,
    ) -> Result<PathBuf> {
        for attempt in 0..32_u8 {
            let candidate = if attempt == 0 {
                base_path.to_path_buf()
            } else {
                collision_variant(base_path, telegram_media_key, attempt)
            };

            if self.path_is_available(database, &candidate, telegram_media_key)? {
                return Ok(candidate);
            }
        }

        Err(AppError::Filesystem(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not resolve a deterministic collision-free destination path",
        )))
    }

    fn path_is_available(
        &self,
        database: &Database,
        candidate: &Path,
        telegram_media_key: &str,
    ) -> Result<bool> {
        if let Some(existing) = database.find_media_by_path(candidate)?
            && existing.telegram_media_key != telegram_media_key
        {
            return Ok(false);
        }

        Ok(!candidate.exists())
    }
}

enum MediaPlan<H> {
    Skip(PersistedMediaItem),
    Queue(DownloadJob<H>),
}

struct DownloadedFile {
    final_path: PathBuf,
    final_size_bytes: i64,
    sha256: String,
}

struct VerifiedExistingRecord {
    file_size_bytes: i64,
}

pub(crate) async fn execute_download<G: TelegramGateway>(
    gateway: &G,
    config: &AppConfig,
    shutdown: &ShutdownFlag,
    job: DownloadJob<G::MediaHandle>,
) -> Result<PersistedMediaItem> {
    let base_record = job.pending_record;
    ensure_parent_dir(&base_record.local_path).await?;

    match download_with_retry(
        gateway,
        config,
        shutdown,
        &job.handle,
        base_record.file_size_bytes,
        &job.temp_path,
        &base_record.local_path,
    )
    .await
    {
        Ok(downloaded_file) => Ok(PersistedMediaItem {
            file_size_bytes: Some(downloaded_file.final_size_bytes),
            sha256: Some(downloaded_file.sha256),
            local_path: downloaded_file.final_path,
            status: MediaStatus::Downloaded,
            error_message: None,
            ..base_record
        }),
        Err(error @ AppError::FloodWaitExceeded { .. }) => Err(error),
        Err(error) => {
            warn!(
                "failed to download {}: {error}",
                base_record.telegram_media_key
            );
            Ok(PersistedMediaItem {
                status: MediaStatus::Failed,
                error_message: Some(error.to_string()),
                ..base_record
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_with_retry<G: TelegramGateway>(
    gateway: &G,
    config: &AppConfig,
    shutdown: &ShutdownFlag,
    handle: &G::MediaHandle,
    expected_size_bytes: Option<i64>,
    temp_path: &Path,
    final_path: &Path,
) -> Result<DownloadedFile> {
    let mut attempt = 0_u32;
    loop {
        cleanup_file_if_exists(temp_path).await?;
        match gateway
            .download_media_to_path(handle, temp_path, shutdown)
            .await
        {
            Ok(()) => {
                validate_download_size(temp_path, expected_size_bytes).await?;
                let final_size_bytes = i64_file_size(tokio::fs::metadata(temp_path).await?.len())?;
                let sha256 = compute_sha256_async(temp_path).await?;
                move_atomic(temp_path, final_path).await?;
                return Ok(DownloadedFile {
                    final_path: final_path.to_path_buf(),
                    final_size_bytes,
                    sha256,
                });
            }
            Err(error @ AppError::FloodWaitExceeded { .. }) => {
                cleanup_file_if_exists(temp_path).await?;
                return Err(error);
            }
            Err(error @ AppError::Interrupted(_)) => {
                cleanup_file_if_exists(temp_path).await?;
                return Err(error);
            }
            Err(error) => {
                cleanup_file_if_exists(temp_path).await?;
                if attempt >= config.retry_count {
                    return Err(error);
                }
                let delay_ms = config
                    .retry_backoff_ms
                    .saturating_mul(2_u64.saturating_pow(attempt));
                info!(
                    "download failed on attempt {}; retrying after {}ms",
                    attempt + 1,
                    delay_ms
                );
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        return Err(AppError::Interrupted(
                            "download cancelled by signal".to_string(),
                        ));
                    }
                    _ = sleep(Duration::from_millis(delay_ms)) => {}
                }
                attempt += 1;
            }
        }
    }
}

fn temp_path_for(final_path: &Path, temp_extension: &str) -> PathBuf {
    let file_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    final_path.with_file_name(format!("{file_name}{temp_extension}"))
}

async fn validate_download_size(path: &Path, expected_size_bytes: Option<i64>) -> Result<()> {
    let Some(expected_size_bytes) = expected_size_bytes else {
        return Ok(());
    };
    if expected_size_bytes < 0 {
        return Ok(());
    }

    let actual_size_bytes = tokio::fs::metadata(path).await?.len();
    if actual_size_bytes != expected_size_bytes as u64 {
        return Err(AppError::Runtime(format!(
            "downloaded size mismatch: expected {expected_size_bytes} bytes, got {actual_size_bytes} bytes"
        )));
    }

    Ok(())
}

fn i64_file_size(size: u64) -> Result<i64> {
    i64::try_from(size).map_err(|_| {
        AppError::Runtime(format!(
            "file size {size} bytes is too large to store in the state database"
        ))
    })
}

fn collision_variant(base_path: &Path, telegram_media_key: &str, attempt: u8) -> PathBuf {
    let suffix = stable_suffix(telegram_media_key);
    let stem = base_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    let extension = base_path.extension().and_then(|value| value.to_str());
    let extra = &suffix[..8];
    let file_name = match extension {
        Some(extension) => format!("{stem}_{attempt}_{extra}.{extension}"),
        None => format!("{stem}_{attempt}_{extra}"),
    };
    base_path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn validates_matching_download_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ok.bin");
        tokio::fs::write(&path, b"1234").await.expect("write");

        validate_download_size(&path, Some(4))
            .await
            .expect("matching size should pass");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_truncated_download_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.bin");
        tokio::fs::write(&path, b"1234").await.expect("write");

        let error = validate_download_size(&path, Some(8))
            .await
            .expect_err("size mismatch should fail");
        assert!(error.to_string().contains("downloaded size mismatch"));
    }
}
