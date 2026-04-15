use std::path::Path;

use crate::error::Result;
use crate::pacing::PaceBucket;
use crate::shutdown::ShutdownFlag;
use crate::telegram::{RealMediaHandle, RealTelegramGateway};
use crate::types::PacingStats;
use grammers_client::InvocationError;
use grammers_client::client::DownloadIter;
use tokio::io::AsyncWriteExt;
use tokio::time::{Duration, timeout};

const DEFAULT_DOWNLOAD_CHUNK_SIZE_BYTES: u64 = 512 * 1024;

pub(super) async fn download_media_to_path_impl(
    gateway: &RealTelegramGateway,
    media: &RealMediaHandle,
    path: &Path,
    shutdown: &ShutdownFlag,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::select! {
        _ = shutdown.cancelled() => {
            return Err(crate::error::AppError::Interrupted(
                "download cancelled by signal".to_string(),
            ));
        }
        _ = gateway.pacer.wait_for_turn(PaceBucket::Download) => {}
    }
    let mut file = tokio::fs::File::create(path).await?;
    let mut download = download_iter(gateway, media, 0);
    let mut bytes_written = 0_u64;
    let mut attempt = 0_u32;

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                return Err(crate::error::AppError::Interrupted(
                    "download cancelled by signal".to_string(),
                ));
            }
            _ = gateway.pacer.wait_for_download_step() => {}
        }
        match tokio::select! {
            _ = shutdown.cancelled() => {
                return Err(crate::error::AppError::Interrupted(
                    "download cancelled by signal".to_string(),
                ));
            }
            result = timeout(
                Duration::from_secs(gateway.config.download_stall_timeout_secs),
                download.next(),
            ) => {
                result.map_err(|_| {
                    crate::error::AppError::Runtime(format!(
                        "download stalled for {}s",
                        gateway.config.download_stall_timeout_secs
                    ))
                })?
            }
        } {
            Ok(Some(chunk)) => {
                attempt = 0;
                bytes_written += chunk.len() as u64;
                file.write_all(&chunk).await?;
            }
            Ok(None) => break,
            Err(InvocationError::Rpc(error)) if error.code == 420 => {
                let seconds = error.value.unwrap_or(0) as i32;
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        return Err(crate::error::AppError::Interrupted(
                            "download cancelled by signal".to_string(),
                        ));
                    }
                    result = gateway.pacer.sleep_on_flood_wait("download media", seconds, attempt) => {
                        result?;
                    }
                }
                let chunks_written = completed_chunk_count(bytes_written)?;
                download = download_iter(gateway, media, chunks_written);
                attempt += 1;
            }
            Err(error) => return Err(error.into()),
        }
    }
    file.flush().await?;
    Ok(())
}

pub(super) async fn pacing_stats_impl(gateway: &RealTelegramGateway) -> PacingStats {
    gateway.pacer.stats().await
}

fn download_iter(
    gateway: &RealTelegramGateway,
    media: &RealMediaHandle,
    skip_chunks: i32,
) -> DownloadIter {
    let download = match media {
        RealMediaHandle::Photo(photo) => gateway.client.iter_download(photo),
        RealMediaHandle::Document(document) => gateway.client.iter_download(document),
    };

    if skip_chunks > 0 {
        download.skip_chunks(skip_chunks)
    } else {
        download
    }
}

fn completed_chunk_count(bytes_written: u64) -> Result<i32> {
    if bytes_written % DEFAULT_DOWNLOAD_CHUNK_SIZE_BYTES != 0 {
        return Err(crate::error::AppError::Runtime(format!(
            "download hit flood wait after {bytes_written} bytes; cannot resume safely from a partial chunk"
        )));
    }

    i32::try_from(bytes_written / DEFAULT_DOWNLOAD_CHUNK_SIZE_BYTES).map_err(|_| {
        crate::error::AppError::Runtime(format!(
            "download offset {bytes_written} bytes is too large to resume"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_completed_download_chunks() {
        assert_eq!(completed_chunk_count(0).expect("zero"), 0);
        assert_eq!(
            completed_chunk_count(DEFAULT_DOWNLOAD_CHUNK_SIZE_BYTES * 3).expect("chunks"),
            3
        );
    }

    #[test]
    fn rejects_partial_chunk_resume() {
        let error =
            completed_chunk_count(DEFAULT_DOWNLOAD_CHUNK_SIZE_BYTES + 1).expect_err("partial");
        assert!(error.to_string().contains("cannot resume safely"));
    }
}
