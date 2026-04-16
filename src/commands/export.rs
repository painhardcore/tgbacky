use std::path::PathBuf;
use std::time::Duration;

use tracing::{info, warn};

use crate::app::open_export_context;
use crate::config::{AppConfig, DownloadConcurrencyOrigin};
use crate::error::Result;
use crate::export::{describe_export_mode, describe_export_scope, run_export, run_export_plan};
use crate::fsutil::write_utf8_file;
use crate::recovery::cleanup_stale_temp_files;
use crate::report::{RunArtifact, RunOutcome};
use crate::secrets::credential_storage_status;
use crate::storage::Database;
use crate::telegram::RealTelegramGateway;
use crate::types::{ExportOptions, MediaKind};

pub struct ExportCommand {
    pub chat: String,
    pub out_dir: PathBuf,
    pub resume: bool,
    pub verbose_progress: bool,
    pub media_filter: std::collections::BTreeSet<MediaKind>,
    pub since_id: Option<i32>,
    pub until_id: Option<i32>,
    pub date_from: Option<chrono::NaiveDate>,
    pub date_to: Option<chrono::NaiveDate>,
    pub limit: Option<usize>,
    pub json_report: bool,
    pub rescan: bool,
}

pub struct ExportPlanCommand {
    pub chat: String,
    pub out_dir: PathBuf,
    pub media_filter: std::collections::BTreeSet<MediaKind>,
    pub since_id: Option<i32>,
    pub until_id: Option<i32>,
    pub date_from: Option<chrono::NaiveDate>,
    pub date_to: Option<chrono::NaiveDate>,
    pub limit: Option<usize>,
    pub rescan: bool,
    pub save_queue: bool,
    pub json: bool,
}

pub async fn run(config: &AppConfig, command: ExportCommand) -> Result<()> {
    let requested_chat = command.chat.clone();
    let out_dir = command.out_dir.clone();
    print_export_prelude(config, &command);
    let mut context = open_export_context(config).await?;
    let run_id = context
        .database
        .start_run("export", Some(&requested_chat), Some(&out_dir))?;

    let stale_summary = match cleanup_stale_temp_files(
        &out_dir,
        &config.temp_extension,
        Duration::from_secs(config.stale_part_min_age_hours.saturating_mul(3_600)),
        config.cleanup_stale_parts_on_start,
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            context.database.finish_run_failure(
                run_id,
                Some(&requested_chat),
                Some(&out_dir),
                &error.to_string(),
                None,
            )?;
            return Err(error);
        }
    };
    if stale_summary.stale_found > 0 {
        info!(
            stale_found = stale_summary.stale_found,
            scanned_files = stale_summary.scanned_files,
            unreadable_entries = stale_summary.unreadable_entries,
            removed = stale_summary.removed,
            output_dir = %out_dir.display(),
            "found stale partial downloads before export"
        );
    }

    let result = run_export(
        &context.gateway,
        &mut context.database,
        config,
        ExportOptions {
            chat: requested_chat.clone(),
            out_dir: out_dir.clone(),
            resume: command.resume,
            verbose_progress: command.verbose_progress,
            media_filter: command.media_filter.clone(),
            since_id: command.since_id,
            until_id: command.until_id,
            date_from: command.date_from,
            date_to: command.date_to,
            limit: command.limit,
            rescan: command.rescan,
        },
    )
    .await;

    match result {
        Ok(crate::export::ExportRunOutcome::Completed(report)) => {
            let artifact_path = write_run_artifact(
                config,
                RunArtifact::success(
                    run_id,
                    "export",
                    Some(requested_chat.clone()),
                    report.clone(),
                ),
            )
            .await;
            context
                .database
                .finish_run_success(run_id, &report, artifact_path.as_deref())?;
            println!("{}", report.human());
            if command.json_report {
                println!("{}", report.to_json_pretty()?);
            }
            Ok(())
        }
        Ok(crate::export::ExportRunOutcome::Interrupted(report)) => {
            let preserved_checkpoint = report
                .last_checkpoint_message_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string());
            let artifact_path = write_run_artifact(
                config,
                RunArtifact::interrupted(
                    run_id,
                    "export",
                    Some(requested_chat.clone()),
                    report.clone(),
                    "export interrupted by signal; checkpoint saved",
                ),
            )
            .await;
            context
                .database
                .finish_run_interrupted(run_id, &report, artifact_path.as_deref())?;
            println!("Export interrupted. Progress was saved.");
            println!("{}", report.human());
            println!("Interrupted: checkpoint preserved at {preserved_checkpoint}");
            if command.json_report {
                println!("{}", report.to_json_pretty()?);
            }
            Err(crate::error::AppError::Interrupted(
                "export interrupted by signal; checkpoint saved".to_string(),
            ))
        }
        Err(error) => {
            let artifact_path = write_run_artifact(
                config,
                RunArtifact::failure(
                    run_id,
                    "export",
                    Some(requested_chat.clone()),
                    error.to_string(),
                ),
            )
            .await;
            context.database.finish_run_failure(
                run_id,
                Some(&requested_chat),
                Some(&out_dir),
                &error.to_string(),
                artifact_path.as_deref(),
            )?;
            Err(error)
        }
    }
}

pub async fn plan(config: &AppConfig, command: ExportPlanCommand) -> Result<()> {
    print_export_plan_prelude(config, &command);
    let gateway = RealTelegramGateway::new(config).await?;
    let mut database = if command.save_queue {
        Database::open(&config.db_path)?
    } else if config.db_path.exists() {
        Database::open_readonly(&config.db_path)?
    } else {
        Database::open_in_memory()?
    };

    let report = run_export_plan(
        &gateway,
        &mut database,
        config,
        ExportOptions {
            chat: command.chat.clone(),
            out_dir: command.out_dir.clone(),
            resume: false,
            verbose_progress: false,
            media_filter: command.media_filter.clone(),
            since_id: command.since_id,
            until_id: command.until_id,
            date_from: command.date_from,
            date_to: command.date_to,
            limit: command.limit,
            rescan: command.rescan,
        },
        command.save_queue,
    )
    .await?;

    println!("{}", report.human());
    if command.json {
        println!("{}", report.to_json_pretty()?);
    }
    Ok(())
}

fn print_export_prelude(config: &AppConfig, command: &ExportCommand) {
    let options = ExportOptions {
        chat: command.chat.clone(),
        out_dir: command.out_dir.clone(),
        resume: command.resume,
        verbose_progress: command.verbose_progress,
        media_filter: command.media_filter.clone(),
        since_id: command.since_id,
        until_id: command.until_id,
        date_from: command.date_from,
        date_to: command.date_to,
        limit: command.limit,
        rescan: command.rescan,
    };

    println!("Starting export");
    println!(
        "Profile           : {} ({})",
        config.profile,
        config.profile_source.label()
    );
    println!("API profile       : {}", config.api_profile);
    println!("API credentials   : {}", export_credential_label(config));
    println!("Session           : {}", config.session_path.display());
    println!("State DB          : {}", config.db_path.display());
    println!("Chat              : {}", command.chat);
    println!("Output directory  : {}", command.out_dir.display());
    println!("Mode              : {}", describe_export_mode(&options));
    println!("Scope             : {}", describe_export_scope(&options));
    println!(
        "Media types       : {}",
        format_media_filter(&command.media_filter)
    );
    println!(
        "Download workers  : {}",
        format_worker_setting(
            config.download_concurrency,
            config.download_concurrency_origin
        )
    );

    if command.resume {
        println!("Compatibility      : --resume is now the default behavior and can be omitted");
    }

    if !command.rescan
        && command.since_id.is_none()
        && command.until_id.is_none()
        && command.date_from.is_none()
        && command.date_to.is_none()
    {
        println!(
            "Behavior          : tgbacky resumes from saved checkpoint when available; use --rescan to force a full history pass"
        );
    }

    println!();
}

fn print_export_plan_prelude(config: &AppConfig, command: &ExportPlanCommand) {
    let options = ExportOptions {
        chat: command.chat.clone(),
        out_dir: command.out_dir.clone(),
        resume: false,
        verbose_progress: false,
        media_filter: command.media_filter.clone(),
        since_id: command.since_id,
        until_id: command.until_id,
        date_from: command.date_from,
        date_to: command.date_to,
        limit: command.limit,
        rescan: command.rescan,
    };

    println!("Planning export");
    println!(
        "Profile           : {} ({})",
        config.profile,
        config.profile_source.label()
    );
    println!("API profile       : {}", config.api_profile);
    println!("API credentials   : {}", export_credential_label(config));
    println!("Session           : {}", config.session_path.display());
    println!("State DB          : {}", config.db_path.display());
    println!("Chat              : {}", command.chat);
    println!("Output directory  : {}", command.out_dir.display());
    println!("Mode              : {}", describe_export_mode(&options));
    println!("Scope             : {}", describe_export_scope(&options));
    println!(
        "Media types       : {}",
        format_media_filter(&command.media_filter)
    );
    println!(
        "Persistence       : {}",
        if command.save_queue {
            "save pending queue"
        } else {
            "dry-run only"
        }
    );
    println!();
}

fn export_credential_label(config: &AppConfig) -> String {
    if config.credential_source.label() != "runtime" {
        return config.credential_source.label().to_string();
    }

    match credential_storage_status(&config.api_profile) {
        Ok(status) if status.label() != "none" => status.label().to_string(),
        _ => config.credential_source.label().to_string(),
    }
}

fn format_worker_setting(workers: usize, origin: DownloadConcurrencyOrigin) -> String {
    match origin {
        DownloadConcurrencyOrigin::Auto => format!("{workers} (auto-detected)"),
        DownloadConcurrencyOrigin::Cli => format!("{workers} (from --workers)"),
    }
}

fn format_media_filter(filter: &std::collections::BTreeSet<MediaKind>) -> String {
    filter
        .iter()
        .map(|kind| kind.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

async fn write_run_artifact(config: &AppConfig, artifact: RunArtifact) -> Option<PathBuf> {
    let suffix = match artifact.outcome {
        RunOutcome::Succeeded => "success",
        RunOutcome::Interrupted => "interrupted",
        RunOutcome::Failed => "failed",
    };
    let path = config
        .run_artifact_dir
        .join(format!("run_{:06}_{suffix}.json", artifact.run_id));
    match artifact.to_json_pretty() {
        Ok(contents) => match write_utf8_file(&path, &contents).await {
            Ok(()) => Some(path),
            Err(error) => {
                warn!("failed to write run artifact: {error}");
                None
            }
        },
        Err(error) => {
            warn!("failed to serialize run artifact: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn artifact_path_suffix_tracks_outcome() {
        let base = Path::new("/tmp/artifacts");
        let success = base.join("run_000001_success.json");
        let failed = base.join("run_000001_failed.json");
        assert!(success.ends_with("run_000001_success.json"));
        assert!(failed.ends_with("run_000001_failed.json"));
    }
}
