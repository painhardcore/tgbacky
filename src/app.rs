use crate::config::AppConfig;
use crate::error::Result;
use crate::storage::Database;
use crate::telegram::RealTelegramGateway;

pub struct ExportContext {
    pub gateway: RealTelegramGateway,
    pub database: Database,
}

pub async fn open_export_context(config: &AppConfig) -> Result<ExportContext> {
    let (gateway, database) = initialize_export_resources(
        config.clone(),
        |config| async move { RealTelegramGateway::new(&config).await },
        |config| Database::open(&config.db_path),
    )
    .await?;
    Ok(ExportContext { gateway, database })
}

async fn initialize_export_resources<G, D, FG, FutG, FD>(
    config: AppConfig,
    open_gateway: FG,
    open_database: FD,
) -> Result<(G, D)>
where
    FG: FnOnce(AppConfig) -> FutG,
    FutG: Future<Output = Result<G>>,
    FD: FnOnce(&AppConfig) -> Result<D>,
{
    // Telegram session storage must initialize before rusqlite touches SQLite,
    // otherwise libsql inside grammers-session can abort during global setup.
    let gateway = open_gateway(config.clone()).await?;
    let database = open_database(&config)?;
    Ok((gateway, database))
}

use std::future::Future;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::filter::LevelFilter;

    use super::*;
    use crate::config::{CredentialSource, DownloadConcurrencyOrigin, ProfileSource};
    use crate::types::{FilenameMode, MediaKind};

    fn test_config() -> AppConfig {
        AppConfig {
            profile: "default".to_string(),
            api_profile: "default".to_string(),
            profile_source: ProfileSource::Default,
            credential_source: CredentialSource::Flags,
            api_id: Some(1),
            api_hash: Some("hash".to_string()),
            session_path: "data/session.db".into(),
            db_path: "data/state.db".into(),
            download_dir: "downloads".into(),
            run_artifact_dir: "artifacts".into(),
            log_level: LevelFilter::ERROR,
            retry_count: 0,
            retry_backoff_ms: 1,
            download_stall_timeout_secs: 1,
            media_filter: std::collections::BTreeSet::from(MediaKind::ALL),
            filename_mode: FilenameMode::Stable,
            temp_extension: ".part".to_string(),
            request_delay_ms: 1,
            download_delay_ms: 1,
            flood_sleep_threshold_secs: 1,
            jitter_ms: 0,
            download_concurrency: 2,
            download_concurrency_origin: DownloadConcurrencyOrigin::Auto,
            cleanup_stale_parts_on_start: false,
            stale_part_min_age_hours: 12,
            verbose_dependency_logs: false,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn initializes_gateway_before_database() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let config = test_config();

        let calls_for_gateway = Arc::clone(&calls);
        let calls_for_database = Arc::clone(&calls);
        let result = initialize_export_resources(
            config.clone(),
            move |_| {
                let calls = Arc::clone(&calls_for_gateway);
                async move {
                    calls.lock().expect("calls").push("gateway");
                    Ok::<_, crate::error::AppError>("gateway")
                }
            },
            move |_| {
                calls_for_database.lock().expect("calls").push("database");
                Ok::<_, crate::error::AppError>("database")
            },
        )
        .await
        .expect("resources");

        assert_eq!(result, ("gateway", "database"));
        assert_eq!(
            calls.lock().expect("calls").as_slice(),
            ["gateway", "database"]
        );
    }
}
