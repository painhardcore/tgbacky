use std::path::Path;

use crate::error::Result;
use crate::pacing::PaceBucket;
use crate::shutdown::ShutdownFlag;
use crate::telegram::{RealMediaHandle, RealTelegramGateway};
use crate::types::PacingStats;
use grammers_client::InvocationError;
use tokio::io::AsyncWriteExt;
use tokio::time::{Duration, timeout};

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
    let mut download = match media {
        RealMediaHandle::Photo(photo) => gateway.client.iter_download(photo),
        RealMediaHandle::Document(document) => gateway.client.iter_download(document),
    };
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
