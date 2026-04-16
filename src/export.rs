mod processor;
mod progress;
mod scope;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::config::{AppConfig, auto_scan_ahead_messages};
use crate::error::{AppError, Result};
use crate::report::{ExportCounters, ExportReport, ExportReportInput};
use crate::shutdown::ShutdownFlag;
use crate::storage::{CompleteExportPlan, Database, NewExportPlan, PersistedMediaItem};
use crate::telegram::TelegramGateway;
use crate::types::{CheckpointState, ExportOptions, MediaKind, MediaStatus};
use futures_util::future::{AbortHandle, Abortable, FutureExt, LocalBoxFuture};
use futures_util::stream::{FuturesUnordered, StreamExt};
use processor::{DownloadJob, MessageProcessor, MessageProcessorParams, execute_download};
pub(crate) use progress::{
    ExportProgress, ProgressRuntime, describe_export_mode, describe_export_scope,
};
use scope::{
    empty_checkpoint, reached_limit, should_stop_on_message, validate_export_options, within_scope,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracing::info;

pub enum ExportRunOutcome {
    Completed(ExportReport),
    Interrupted(ExportReport),
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportPlanReport {
    pub chat_id: i64,
    pub chat_title: String,
    pub output_dir: PathBuf,
    pub mode: String,
    pub scope: String,
    pub media_filter: Vec<String>,
    pub save_queue: bool,
    pub plan_id: Option<i64>,
    pub scanned_messages: usize,
    pub media_found: usize,
    pub already_tracked: usize,
    pub already_downloaded: usize,
    pub skipped_existing: usize,
    pub would_queue: usize,
    pub estimated_bytes: u64,
    pub per_kind: BTreeMap<String, usize>,
    pub duration_ms: u128,
}

impl ExportPlanReport {
    pub fn human(&self) -> String {
        let kinds = self.media_filter.join(", ");
        let per_kind = self
            .per_kind
            .iter()
            .map(|(kind, count)| format!("{kind}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        let plan_id = self
            .plan_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string());
        let mut lines = vec![
            format!(
                "Target chat        : {} ({})",
                self.chat_title, self.chat_id
            ),
            format!("Output directory   : {}", self.output_dir.display()),
            format!("Mode               : {}", self.mode),
            format!("Scope              : {}", self.scope),
            format!("Media types        : {kinds}"),
            format!("Saved queue        : {}", yes_no(self.save_queue)),
            format!("Plan id            : {plan_id}"),
            format!("Scanned messages   : {}", self.scanned_messages),
            format!("Media found        : {}", self.media_found),
            format!("Already tracked    : {}", self.already_tracked),
            format!("Already downloaded : {}", self.already_downloaded),
            format!("Skipped existing   : {}", self.skipped_existing),
            format!("Would queue        : {}", self.would_queue),
            format!("Estimated bytes    : {}", self.estimated_bytes),
            format!(
                "Duration           : {:.3}s",
                self.duration_ms as f64 / 1_000.0
            ),
        ];
        if !per_kind.is_empty() {
            lines.push(format!("Per kind           : {per_kind}"));
        }
        lines.join("\n")
    }

    pub fn to_json_pretty(&self) -> std::result::Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[derive(Default)]
struct ExportPlanCounters {
    scanned_messages: usize,
    media_found: usize,
    already_tracked: usize,
    already_downloaded: usize,
    skipped_existing: usize,
    would_queue: usize,
    estimated_bytes: u64,
    per_kind: BTreeMap<String, usize>,
}

struct MessageFrontier {
    message_id: i32,
    checkpoint: Option<crate::types::CheckpointState>,
    remaining_downloads: usize,
}

struct DownloadEvent<H> {
    message_id: i32,
    record: crate::storage::PersistedMediaItem,
    retry_job: Option<DownloadJob<H>>,
    final_retry_job: Option<DownloadJob<H>>,
}

type InFlightDownload<'a, H> = Abortable<LocalBoxFuture<'a, Result<DownloadEvent<H>>>>;

struct DownloadPump<'a, G: TelegramGateway> {
    gateway: &'a G,
    config: &'a AppConfig,
    shutdown: ShutdownFlag,
    scan_ahead_messages: usize,
    progress: &'a ExportProgress,
}

struct AdaptiveConcurrency {
    max_limit: usize,
    current_limit: usize,
    calm_successes: usize,
    last_flood_wait_count: u64,
}

#[derive(Default)]
struct HistoryBatchMeter {
    started_at: Option<Instant>,
    completed_batches: usize,
    completed_messages: usize,
}

impl HistoryBatchMeter {
    fn observe_batch(&mut self, message_count: usize) {
        if self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }
        self.completed_batches += 1;
        self.completed_messages += message_count;
    }

    fn batches_per_sec(&self) -> Option<f64> {
        let started_at = self.started_at?;
        let elapsed = started_at.elapsed().as_secs_f64().max(0.001);
        Some(self.completed_batches as f64 / elapsed)
    }

    fn messages_per_sec(&self) -> Option<f64> {
        let started_at = self.started_at?;
        let elapsed = started_at.elapsed().as_secs_f64().max(0.001);
        Some(self.completed_messages as f64 / elapsed)
    }
}

async fn progress_runtime<G: TelegramGateway>(
    gateway: &G,
    concurrency: &AdaptiveConcurrency,
    active_downloads: usize,
    queued_downloads: usize,
    frontier_depth: usize,
    history_batches_per_sec: Option<f64>,
) -> ProgressRuntime {
    let pacing = gateway.pacing_stats().await;
    ProgressRuntime {
        worker_limit: concurrency.current_limit(),
        worker_max: concurrency.max_limit(),
        active_downloads,
        cooldown_active: pacing.cooldown_active,
        flood_wait_count: pacing.flood_wait_count,
        queued_downloads,
        frontier_depth,
        history_batches_per_sec,
        history_messages_per_sec: None,
    }
}

async fn history_progress_runtime<G: TelegramGateway>(
    gateway: &G,
    concurrency: &AdaptiveConcurrency,
    active_downloads: usize,
    queued_downloads: usize,
    frontier_depth: usize,
    meter: &HistoryBatchMeter,
) -> ProgressRuntime {
    let pacing = gateway.pacing_stats().await;
    ProgressRuntime {
        worker_limit: concurrency.current_limit(),
        worker_max: concurrency.max_limit(),
        active_downloads,
        cooldown_active: pacing.cooldown_active,
        flood_wait_count: pacing.flood_wait_count,
        queued_downloads,
        frontier_depth,
        history_batches_per_sec: meter.batches_per_sec(),
        history_messages_per_sec: meter.messages_per_sec(),
    }
}

impl AdaptiveConcurrency {
    fn new(max_limit: usize) -> Self {
        Self {
            max_limit,
            current_limit: 1,
            calm_successes: 0,
            last_flood_wait_count: 0,
        }
    }

    fn current_limit(&self) -> usize {
        self.current_limit
    }

    fn max_limit(&self) -> usize {
        self.max_limit
    }

    fn constrain_to(&mut self, limit: usize) {
        self.current_limit = limit.clamp(1, self.max_limit.max(1));
        self.calm_successes = 0;
    }

    fn observe(&mut self, flood_wait_count: u64, success: bool) {
        if flood_wait_count > self.last_flood_wait_count {
            self.last_flood_wait_count = flood_wait_count;
            self.current_limit = 1;
            self.calm_successes = 0;
            return;
        }

        if !success || self.current_limit >= self.max_limit {
            return;
        }

        self.calm_successes += 1;
        if self.calm_successes >= 8 {
            self.current_limit += 1;
            self.calm_successes = 0;
        }
    }
}

pub async fn run_export<G: TelegramGateway>(
    gateway: &G,
    database: &mut Database,
    config: &AppConfig,
    options: ExportOptions,
) -> Result<ExportRunOutcome> {
    if !gateway.is_authorized().await? {
        return Err(AppError::Authentication(
            "session is not authorized; run `tgbacky auth` first".to_string(),
        ));
    }

    validate_export_options(&options)?;
    let started_at = Instant::now();
    let shutdown = ShutdownFlag::spawn();
    let chat = gateway.resolve_chat(&options.chat).await?;
    database.upsert_chat(&chat)?;
    if options.rescan {
        let superseded =
            database.supersede_complete_export_plans_for_chat(chat.id, &config.profile)?;
        if superseded > 0 {
            info!(
                chat_id = chat.id,
                superseded, "superseded saved export plans because --rescan was requested"
            );
        }
    }

    let updates_checkpoint = options.since_id.is_none()
        && options.until_id.is_none()
        && options.date_from.is_none()
        && options.date_to.is_none()
        && !options.rescan;

    let mut checkpoint = if updates_checkpoint {
        database
            .load_checkpoint(chat.id)?
            .unwrap_or_else(|| empty_checkpoint(chat.id))
    } else {
        database
            .load_checkpoint(chat.id)?
            .unwrap_or_else(|| empty_checkpoint(chat.id))
    };

    let progress = ExportProgress::new(&chat.title, &options);
    let mut counters = ExportCounters::default();
    let processor = MessageProcessor::new(MessageProcessorParams {
        config,
        media_filter: options.media_filter.clone(),
        chat_id: chat.id,
        chat_title: &chat.title,
        output_root: &options.out_dir,
    });

    let mut stop = false;
    let mut pending_jobs = VecDeque::new();
    let mut final_retry_jobs = VecDeque::new();
    let mut in_flight = FuturesUnordered::new();
    let mut abort_handles = Vec::<AbortHandle>::new();
    let mut frontiers = VecDeque::new();
    let mut durable_checkpoint = checkpoint.clone();
    let mut concurrency = AdaptiveConcurrency::new(config.download_concurrency);
    let mut history_batch_meter = HistoryBatchMeter::default();
    let pump = DownloadPump {
        gateway,
        config,
        shutdown: shutdown.clone(),
        scan_ahead_messages: auto_scan_ahead_messages(config.download_concurrency),
        progress: &progress,
    };

    let media_filter_key = media_filter_key(&options.media_filter);
    let scope_hash = export_scope_hash(&options);
    let normalized_out_dir = normalize_plan_output_dir(&options.out_dir)?;
    let saved_plan = if updates_checkpoint {
        database.latest_complete_export_plan(
            chat.id,
            &config.profile,
            &normalized_out_dir,
            &media_filter_key,
            &scope_hash,
        )?
    } else {
        None
    };
    if saved_plan.is_some() {
        println!("Saved plan        : found, draining queued media first");
    } else {
        println!("Saved plan        : none");
    }

    if updates_checkpoint {
        replay_retryable_messages(
            gateway,
            database,
            &chat.handle,
            &processor,
            &mut counters,
            &mut frontiers,
            &mut pending_jobs,
            &mut in_flight,
            &mut abort_handles,
            &mut durable_checkpoint,
            &mut final_retry_jobs,
            &mut concurrency,
            &pump,
        )
        .await?;

        if let Some(plan) = saved_plan.as_ref()
            && database.list_all_retry_message_ids(chat.id)?.is_empty()
        {
            checkpoint = CheckpointState {
                chat_id: chat.id,
                high_watermark_message_id: plan.planned_high_watermark_message_id,
                backfill_cursor_message_id: plan.planned_backfill_cursor_message_id,
                backfill_complete: plan.planned_backfill_complete,
            };
            database.save_checkpoint(&checkpoint)?;
            durable_checkpoint = checkpoint.clone();
        }
    }

    if let Some(old_high) = updates_checkpoint
        .then_some(checkpoint.high_watermark_message_id)
        .flatten()
    {
        let mut newest_seen = checkpoint.high_watermark_message_id;
        let mut offset_id = None;
        progress.set_scanning_new(
            &counters,
            progress_runtime(
                gateway,
                &concurrency,
                in_flight.len(),
                pending_jobs.len(),
                frontiers.len(),
                None,
            )
            .await,
        );

        'newer: loop {
            if shutdown.is_requested() {
                stop = true;
                break 'newer;
            }

            let batch = gateway
                .fetch_history_batch(&chat.handle, offset_id, 100)
                .await?;
            if batch.is_empty() {
                checkpoint.high_watermark_message_id = newest_seen;
                break 'newer;
            }

            for message in &batch {
                if message.message_id <= old_high {
                    checkpoint.high_watermark_message_id = newest_seen;
                    break 'newer;
                }

                newest_seen = Some(
                    newest_seen.map_or(message.message_id, |value| value.max(message.message_id)),
                );
                let mut pending_checkpoint = checkpoint.clone();
                pending_checkpoint.high_watermark_message_id = newest_seen;
                queue_message::<G>(
                    &processor,
                    database,
                    message,
                    updates_checkpoint.then_some(&pending_checkpoint),
                    &mut counters,
                    &mut frontiers,
                    &mut pending_jobs,
                )
                .await?;
                drain_completed_frontier(database, &mut frontiers, &mut durable_checkpoint)?;
                checkpoint = pending_checkpoint;
                pump.maybe_start_downloads(
                    database,
                    &mut pending_jobs,
                    &mut in_flight,
                    &mut abort_handles,
                    &counters,
                    &concurrency,
                    frontiers.len(),
                    stop,
                )
                .await?;
                drain_ready_downloads(
                    gateway,
                    database,
                    &mut pending_jobs,
                    &mut in_flight,
                    &mut abort_handles,
                    &mut frontiers,
                    &mut counters,
                    &mut durable_checkpoint,
                    &mut final_retry_jobs,
                    &mut concurrency,
                    true,
                )
                .await?;
                pump.drain_until_within_window(
                    database,
                    &mut pending_jobs,
                    &mut in_flight,
                    &mut abort_handles,
                    &mut frontiers,
                    &mut counters,
                    &mut durable_checkpoint,
                    &mut final_retry_jobs,
                    &mut concurrency,
                )
                .await?;

                if reached_limit(&options, &counters) || shutdown.is_requested() {
                    stop = true;
                    break 'newer;
                }
            }

            progress.set_scanning_new(
                &counters,
                progress_runtime(
                    gateway,
                    &concurrency,
                    in_flight.len(),
                    pending_jobs.len(),
                    frontiers.len(),
                    None,
                )
                .await,
            );

            offset_id = batch.last().map(|message| message.message_id);
        }
    }

    let needs_backfill = !stop && (!updates_checkpoint || !checkpoint.backfill_complete);
    if needs_backfill {
        progress.set_scanning_history(
            &counters,
            history_progress_runtime(
                gateway,
                &concurrency,
                in_flight.len(),
                pending_jobs.len(),
                frontiers.len(),
                &history_batch_meter,
            )
            .await,
        );
        let mut offset_id = if updates_checkpoint {
            checkpoint.backfill_cursor_message_id
        } else {
            options.until_id.map(|value| value.saturating_add(1))
        };

        'backfill: loop {
            if shutdown.is_requested() {
                stop = true;
                break 'backfill;
            }

            let batch = gateway
                .fetch_history_batch(&chat.handle, offset_id, 100)
                .await?;
            if batch.is_empty() {
                if updates_checkpoint {
                    checkpoint.backfill_complete = true;
                }
                break;
            }

            history_batch_meter.observe_batch(batch.len());

            for message in &batch {
                if !within_scope(message, &options) {
                    if should_stop_on_message(message, &options) {
                        break 'backfill;
                    }
                    continue;
                }

                let mut pending_checkpoint = checkpoint.clone();
                if updates_checkpoint {
                    if pending_checkpoint.high_watermark_message_id.is_none() {
                        pending_checkpoint.high_watermark_message_id = Some(message.message_id);
                    }
                    pending_checkpoint.backfill_cursor_message_id = Some(message.message_id);
                }

                queue_message::<G>(
                    &processor,
                    database,
                    message,
                    updates_checkpoint.then_some(&pending_checkpoint),
                    &mut counters,
                    &mut frontiers,
                    &mut pending_jobs,
                )
                .await?;
                drain_completed_frontier(database, &mut frontiers, &mut durable_checkpoint)?;
                checkpoint = pending_checkpoint;
                pump.maybe_start_downloads(
                    database,
                    &mut pending_jobs,
                    &mut in_flight,
                    &mut abort_handles,
                    &counters,
                    &concurrency,
                    frontiers.len(),
                    stop,
                )
                .await?;
                drain_ready_downloads(
                    gateway,
                    database,
                    &mut pending_jobs,
                    &mut in_flight,
                    &mut abort_handles,
                    &mut frontiers,
                    &mut counters,
                    &mut durable_checkpoint,
                    &mut final_retry_jobs,
                    &mut concurrency,
                    true,
                )
                .await?;
                pump.drain_until_within_window(
                    database,
                    &mut pending_jobs,
                    &mut in_flight,
                    &mut abort_handles,
                    &mut frontiers,
                    &mut counters,
                    &mut durable_checkpoint,
                    &mut final_retry_jobs,
                    &mut concurrency,
                )
                .await?;

                if reached_limit(&options, &counters) || shutdown.is_requested() {
                    stop = true;
                    break 'backfill;
                }
            }

            progress.set_scanning_history(
                &counters,
                history_progress_runtime(
                    gateway,
                    &concurrency,
                    in_flight.len(),
                    pending_jobs.len(),
                    frontiers.len(),
                    &history_batch_meter,
                )
                .await,
            );

            offset_id = batch.last().map(|message| message.message_id);
        }
    }

    if !stop {
        while !pending_jobs.is_empty() || !in_flight.is_empty() {
            if shutdown.is_requested() {
                stop = true;
                break;
            }
            pump.maybe_start_downloads(
                database,
                &mut pending_jobs,
                &mut in_flight,
                &mut abort_handles,
                &counters,
                &concurrency,
                frontiers.len(),
                false,
            )
            .await?;
            process_next_download(
                gateway,
                database,
                &mut pending_jobs,
                &mut in_flight,
                &mut abort_handles,
                &mut frontiers,
                &mut counters,
                &mut durable_checkpoint,
                &mut final_retry_jobs,
                &mut concurrency,
                true,
                &shutdown,
            )
            .await?;
        }
        if !stop && !final_retry_jobs.is_empty() {
            info!(
                failed_count = final_retry_jobs.len(),
                "starting final retry sweep for failed downloads"
            );
            concurrency.constrain_to(config.download_concurrency.clamp(1, 2));
            pending_jobs = final_retry_jobs;
            let mut retry_capture = VecDeque::new();
            while !pending_jobs.is_empty() || !in_flight.is_empty() {
                if shutdown.is_requested() {
                    stop = true;
                    break;
                }
                pump.maybe_start_downloads(
                    database,
                    &mut pending_jobs,
                    &mut in_flight,
                    &mut abort_handles,
                    &counters,
                    &concurrency,
                    frontiers.len(),
                    false,
                )
                .await?;
                process_next_download(
                    gateway,
                    database,
                    &mut pending_jobs,
                    &mut in_flight,
                    &mut abort_handles,
                    &mut frontiers,
                    &mut counters,
                    &mut durable_checkpoint,
                    &mut retry_capture,
                    &mut concurrency,
                    false,
                    &shutdown,
                )
                .await?;
            }
        }
        drain_completed_frontier(database, &mut frontiers, &mut durable_checkpoint)?;
        if updates_checkpoint && !stop {
            database.save_checkpoint(&checkpoint)?;
            durable_checkpoint = checkpoint.clone();
        }
    }

    if stop {
        abort_in_flight_downloads(&mut in_flight, &mut abort_handles);
    }

    progress.finish();
    let pacing = gateway.pacing_stats().await;
    let report = ExportReport::from(ExportReportInput {
        chat_id: chat.id,
        chat_title: chat.title,
        last_checkpoint_message_id: durable_checkpoint
            .high_watermark_message_id
            .or(durable_checkpoint.backfill_cursor_message_id),
        output_dir: options.out_dir,
        duration: started_at.elapsed(),
        counters,
        pacing_stats: pacing,
    });

    if stop {
        Ok(ExportRunOutcome::Interrupted(report))
    } else {
        Ok(ExportRunOutcome::Completed(report))
    }
}

pub async fn run_export_plan<G: TelegramGateway>(
    gateway: &G,
    database: &mut Database,
    config: &AppConfig,
    options: ExportOptions,
    save_queue: bool,
) -> Result<ExportPlanReport> {
    if !gateway.is_authorized().await? {
        return Err(AppError::Authentication(
            "session is not authorized; run `tgbacky auth` first".to_string(),
        ));
    }
    validate_export_options(&options)?;
    if save_queue && !is_canonical_automatic_sync(&options) {
        return Err(AppError::InvalidArgument(
            "`export plan --save-queue` only supports automatic full-chat sync in v1; remove bounds, --limit, and --rescan".to_string(),
        ));
    }

    let started_at = Instant::now();
    let shutdown = ShutdownFlag::spawn();
    let chat = gateway.resolve_chat(&options.chat).await?;
    if save_queue {
        database.upsert_chat(&chat)?;
    }

    let media_filter_key = media_filter_key(&options.media_filter);
    let scope_hash = export_scope_hash(&options);
    let normalized_out_dir = normalize_plan_output_dir(&options.out_dir)?;
    let plan_id = if save_queue {
        Some(database.start_export_plan(NewExportPlan {
            chat_id: chat.id,
            profile: &config.profile,
            output_dir: &normalized_out_dir,
            media_filter: &media_filter_key,
            scope_hash: &scope_hash,
        })?)
    } else {
        None
    };

    let processor = MessageProcessor::new(MessageProcessorParams {
        config,
        media_filter: options.media_filter.clone(),
        chat_id: chat.id,
        chat_title: &chat.title,
        output_root: &options.out_dir,
    });
    let mut counters = ExportPlanCounters::default();
    let mut planned_checkpoint = empty_checkpoint(chat.id);
    let mut offset_id = options.until_id.map(|value| value.saturating_add(1));
    let mut interrupted = false;

    'scan: loop {
        if shutdown.is_requested() {
            interrupted = true;
            break 'scan;
        }
        let batch = gateway
            .fetch_history_batch(&chat.handle, offset_id, 100)
            .await?;
        if batch.is_empty() {
            planned_checkpoint.backfill_complete = true;
            break;
        }

        for message in &batch {
            if !within_scope(message, &options) {
                if should_stop_on_message(message, &options) {
                    break 'scan;
                }
                continue;
            }

            counters.scanned_messages += 1;
            for media in &message.media {
                if options.media_filter.contains(&media.kind) {
                    counters.media_found += 1;
                    counters.estimated_bytes = counters
                        .estimated_bytes
                        .saturating_add(media.file_size_bytes.unwrap_or(0).max(0) as u64);
                    *counters
                        .per_kind
                        .entry(media.kind.as_str().to_string())
                        .or_insert(0) += 1;
                }
            }

            let planned = processor.plan_message(database, message).await?;
            counters.already_tracked += planned
                .initial_records
                .iter()
                .filter(|record| record.status == MediaStatus::SkippedExisting)
                .count();
            counters.already_downloaded += planned
                .initial_records
                .iter()
                .filter(|record| record.status == MediaStatus::SkippedExisting)
                .count();
            counters.skipped_existing += planned
                .initial_records
                .iter()
                .filter(|record| record.status == MediaStatus::SkippedExisting)
                .count();
            counters.would_queue += planned.jobs.len();

            if save_queue {
                database.commit_message(&planned.initial_records, None)?;
            }

            if planned_checkpoint.high_watermark_message_id.is_none() {
                planned_checkpoint.high_watermark_message_id = Some(message.message_id);
            }
            planned_checkpoint.backfill_cursor_message_id = Some(message.message_id);

            if reached_plan_limit(&options, &counters) || shutdown.is_requested() {
                interrupted = shutdown.is_requested();
                break 'scan;
            }
        }

        offset_id = batch.last().map(|message| message.message_id);
    }

    let per_kind_json = serde_json::to_string(&counters.per_kind)?;
    if let Some(plan_id) = plan_id {
        if interrupted {
            database.interrupt_export_plan(plan_id)?;
        } else {
            database.complete_export_plan(CompleteExportPlan {
                id: plan_id,
                planned_high_watermark_message_id: planned_checkpoint.high_watermark_message_id,
                planned_backfill_cursor_message_id: planned_checkpoint.backfill_cursor_message_id,
                planned_backfill_complete: planned_checkpoint.backfill_complete,
                scanned_messages: counters.scanned_messages,
                media_found: counters.media_found,
                queued: counters.would_queue,
                estimated_bytes: counters.estimated_bytes,
                per_kind_json: &per_kind_json,
            })?;
        }
    }

    let report = ExportPlanReport {
        chat_id: chat.id,
        chat_title: chat.title,
        output_dir: options.out_dir.clone(),
        mode: describe_export_mode(&options).to_string(),
        scope: describe_export_scope(&options),
        media_filter: options
            .media_filter
            .iter()
            .map(|kind| kind.as_str().to_string())
            .collect(),
        save_queue,
        plan_id,
        scanned_messages: counters.scanned_messages,
        media_found: counters.media_found,
        already_tracked: counters.already_tracked,
        already_downloaded: counters.already_downloaded,
        skipped_existing: counters.skipped_existing,
        would_queue: counters.would_queue,
        estimated_bytes: counters.estimated_bytes,
        per_kind: counters.per_kind,
        duration_ms: started_at.elapsed().as_millis(),
    };

    if interrupted {
        return Err(AppError::Interrupted(
            "export plan interrupted; saved queue was marked interrupted".to_string(),
        ));
    }

    Ok(report)
}

pub fn media_filter_key(filter: &std::collections::BTreeSet<MediaKind>) -> String {
    filter
        .iter()
        .map(|kind| kind.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

pub fn export_scope_hash(options: &ExportOptions) -> String {
    let payload = format!(
        "v1|since={:?}|until={:?}|from={:?}|to={:?}|limit={:?}|rescan={}",
        options.since_id,
        options.until_id,
        options.date_from,
        options.date_to,
        options.limit,
        options.rescan
    );
    let mut hasher = Sha256::new();
    hasher.update(payload.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn normalize_plan_output_dir(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
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

fn is_canonical_automatic_sync(options: &ExportOptions) -> bool {
    options.since_id.is_none()
        && options.until_id.is_none()
        && options.date_from.is_none()
        && options.date_to.is_none()
        && options.limit.is_none()
        && !options.rescan
}

fn reached_plan_limit(options: &ExportOptions, counters: &ExportPlanCounters) -> bool {
    options
        .limit
        .is_some_and(|limit| counters.scanned_messages >= limit)
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[allow(clippy::too_many_arguments)]
async fn replay_retryable_messages<'a, G: TelegramGateway>(
    gateway: &G,
    database: &mut Database,
    chat_handle: &G::ChatHandle,
    processor: &MessageProcessor<'_>,
    counters: &mut ExportCounters,
    frontiers: &mut VecDeque<MessageFrontier>,
    pending_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
    in_flight: &mut FuturesUnordered<InFlightDownload<'a, G::MediaHandle>>,
    abort_handles: &mut Vec<AbortHandle>,
    durable_checkpoint: &mut crate::types::CheckpointState,
    final_retry_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
    concurrency: &mut AdaptiveConcurrency,
    pump: &DownloadPump<'a, G>,
) -> Result<()> {
    let retry_ids = database.list_all_retry_message_ids(processor.chat_id())?;
    for batch in retry_ids.chunks(100) {
        let messages = gateway.fetch_messages_by_ids(chat_handle, batch).await?;
        let returned_ids = messages
            .iter()
            .map(|message| message.message_id)
            .collect::<std::collections::BTreeSet<_>>();
        for message in &messages {
            queue_message::<G>(
                processor,
                database,
                message,
                None,
                counters,
                frontiers,
                pending_jobs,
            )
            .await?;
        }
        for missing_id in batch
            .iter()
            .copied()
            .filter(|message_id| !returned_ids.contains(message_id))
        {
            mark_missing_retry_message_failed(database, processor.chat_id(), missing_id, counters)?;
        }

        while !pending_jobs.is_empty() || !in_flight.is_empty() {
            if pump.shutdown.is_requested() {
                abort_in_flight_downloads(in_flight, abort_handles);
                break;
            }
            pump.maybe_start_downloads(
                database,
                pending_jobs,
                in_flight,
                abort_handles,
                counters,
                concurrency,
                frontiers.len(),
                false,
            )
            .await?;
            if in_flight.is_empty() {
                break;
            }
            process_next_download(
                gateway,
                database,
                pending_jobs,
                in_flight,
                abort_handles,
                frontiers,
                counters,
                durable_checkpoint,
                final_retry_jobs,
                concurrency,
                true,
                &pump.shutdown,
            )
            .await?;
        }
    }

    Ok(())
}

fn mark_missing_retry_message_failed(
    database: &mut Database,
    chat_id: i64,
    message_id: i32,
    counters: &mut ExportCounters,
) -> Result<()> {
    let failed = database
        .list_retry_media_for_message(chat_id, message_id)?
        .into_iter()
        .map(|record| PersistedMediaItem {
            chat_id: record.chat_id,
            message_id: record.message_id,
            message_date: record.message_date,
            kind: record.kind,
            telegram_media_key: record.telegram_media_key,
            mime_type: None,
            file_size_bytes: record.file_size_bytes,
            local_path: record.local_path,
            sha256: record.sha256,
            status: MediaStatus::Failed,
            error_message: Some(
                "planned media could not be refetched from Telegram message".to_string(),
            ),
        })
        .collect::<Vec<_>>();
    if failed.is_empty() {
        return Ok(());
    }
    counters.failed += failed.len();
    database.commit_message(&failed, None)
}

async fn queue_message<G: TelegramGateway>(
    processor: &MessageProcessor<'_>,
    database: &mut Database,
    message: &crate::types::ScannedMessage<G::MediaHandle>,
    checkpoint: Option<&crate::types::CheckpointState>,
    counters: &mut ExportCounters,
    frontiers: &mut VecDeque<MessageFrontier>,
    pending_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
) -> Result<()> {
    for media in &message.media {
        if processor.includes_kind(media.kind) {
            counters.record_found(media.kind);
        }
    }

    let planned = processor.plan_message(database, message).await?;
    counters.skipped_existing += planned
        .initial_records
        .iter()
        .filter(|record| record.status == crate::types::MediaStatus::SkippedExisting)
        .count();
    let remaining_downloads = planned.jobs.len();
    database.commit_message(&planned.initial_records, None)?;
    pending_jobs.extend(planned.jobs);
    frontiers.push_back(MessageFrontier {
        message_id: message.message_id,
        checkpoint: checkpoint.cloned(),
        remaining_downloads,
    });
    counters.scanned_messages += 1;
    Ok(())
}

impl<'a, G: TelegramGateway> DownloadPump<'a, G> {
    #[allow(clippy::too_many_arguments)]
    async fn maybe_start_downloads(
        &self,
        database: &mut Database,
        pending_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
        in_flight: &mut FuturesUnordered<InFlightDownload<'a, G::MediaHandle>>,
        abort_handles: &mut Vec<AbortHandle>,
        counters: &ExportCounters,
        concurrency: &AdaptiveConcurrency,
        frontier_depth: usize,
        stop: bool,
    ) -> Result<()> {
        if stop || self.shutdown.is_requested() {
            return Ok(());
        }

        while in_flight.len() < concurrency.current_limit() {
            if self.shutdown.is_requested() {
                break;
            }
            let Some(job) = pending_jobs.pop_front() else {
                break;
            };
            self.progress.set_downloading(
                job.kind,
                job.message_id,
                counters,
                progress_runtime(
                    self.gateway,
                    concurrency,
                    in_flight.len() + 1,
                    pending_jobs.len(),
                    frontier_depth,
                    None,
                )
                .await,
            );
            let loading = crate::storage::PersistedMediaItem {
                status: crate::types::MediaStatus::Downloading,
                error_message: None,
                ..job.pending_record.clone()
            };
            database.commit_message(&[loading], None)?;

            let message_id = job.message_id;
            let gateway: &'a G = self.gateway;
            let config: &'a AppConfig = self.config;
            let shutdown = self.shutdown.clone();
            let retry_source = job.clone();
            let (abort_handle, abort_registration) = AbortHandle::new_pair();
            abort_handles.push(abort_handle);
            in_flight.push(Abortable::new(
                async move {
                    let record = execute_download(gateway, config, &shutdown, job).await?;
                    let failed = record.status == crate::types::MediaStatus::Failed;
                    let retry_job = if failed && retry_source.scheduler_retry_round == 0 {
                        let mut retry_job = retry_source.clone();
                        retry_job.scheduler_retry_round = 1;
                        Some(retry_job)
                    } else {
                        None
                    };
                    let final_retry_job = failed.then_some(retry_source);
                    Ok(DownloadEvent {
                        message_id,
                        record,
                        retry_job,
                        final_retry_job,
                    })
                }
                .boxed_local(),
                abort_registration,
            ));
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn drain_until_within_window(
        &self,
        database: &mut Database,
        pending_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
        in_flight: &mut FuturesUnordered<InFlightDownload<'a, G::MediaHandle>>,
        abort_handles: &mut Vec<AbortHandle>,
        frontiers: &mut VecDeque<MessageFrontier>,
        counters: &mut ExportCounters,
        durable_checkpoint: &mut crate::types::CheckpointState,
        final_retry_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
        concurrency: &mut AdaptiveConcurrency,
    ) -> Result<()> {
        while frontiers.len() >= self.scan_ahead_messages {
            if self.shutdown.is_requested() {
                abort_in_flight_downloads(in_flight, abort_handles);
                break;
            }
            self.maybe_start_downloads(
                database,
                pending_jobs,
                in_flight,
                abort_handles,
                counters,
                concurrency,
                frontiers.len(),
                false,
            )
            .await?;
            if in_flight.is_empty() {
                break;
            }
            process_next_download(
                self.gateway,
                database,
                pending_jobs,
                in_flight,
                abort_handles,
                frontiers,
                counters,
                durable_checkpoint,
                final_retry_jobs,
                concurrency,
                true,
                &self.shutdown,
            )
            .await?;
        }

        Ok(())
    }
}

fn abort_in_flight_downloads<H>(
    in_flight: &mut FuturesUnordered<InFlightDownload<'_, H>>,
    abort_handles: &mut Vec<AbortHandle>,
) {
    for abort_handle in abort_handles.drain(..) {
        abort_handle.abort();
    }
    let _dropped = std::mem::take(in_flight);
}

#[allow(clippy::too_many_arguments)]
async fn process_next_download<G: TelegramGateway>(
    gateway: &G,
    database: &mut Database,
    pending_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
    in_flight: &mut FuturesUnordered<InFlightDownload<'_, G::MediaHandle>>,
    abort_handles: &mut Vec<AbortHandle>,
    frontiers: &mut VecDeque<MessageFrontier>,
    counters: &mut ExportCounters,
    durable_checkpoint: &mut crate::types::CheckpointState,
    final_retry_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
    concurrency: &mut AdaptiveConcurrency,
    capture_retry_jobs: bool,
    shutdown: &ShutdownFlag,
) -> Result<()> {
    let maybe_result = tokio::select! {
        _ = shutdown.cancelled() => {
            abort_in_flight_downloads(in_flight, abort_handles);
            return Ok(());
        }
        result = in_flight.next() => result,
    };
    let Some(result) = maybe_result else {
        return Ok(());
    };
    let Ok(result) = result else {
        abort_handles.clear();
        return Ok(());
    };
    if matches!(result, Err(AppError::Interrupted(_))) {
        return Ok(());
    }
    handle_download_result(
        gateway,
        database,
        result,
        pending_jobs,
        frontiers,
        counters,
        durable_checkpoint,
        final_retry_jobs,
        concurrency,
        capture_retry_jobs,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn drain_ready_downloads<G: TelegramGateway>(
    gateway: &G,
    database: &mut Database,
    pending_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
    in_flight: &mut FuturesUnordered<InFlightDownload<'_, G::MediaHandle>>,
    abort_handles: &mut Vec<AbortHandle>,
    frontiers: &mut VecDeque<MessageFrontier>,
    counters: &mut ExportCounters,
    durable_checkpoint: &mut crate::types::CheckpointState,
    final_retry_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
    concurrency: &mut AdaptiveConcurrency,
    capture_retry_jobs: bool,
) -> Result<()> {
    loop {
        let Some(maybe_result) = in_flight.next().now_or_never() else {
            break;
        };
        let Some(result) = maybe_result else {
            break;
        };
        let Ok(result) = result else {
            abort_handles.clear();
            break;
        };
        handle_download_result(
            gateway,
            database,
            result,
            pending_jobs,
            frontiers,
            counters,
            durable_checkpoint,
            final_retry_jobs,
            concurrency,
            capture_retry_jobs,
        )
        .await?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_download_result<G: TelegramGateway>(
    gateway: &G,
    database: &mut Database,
    result: Result<DownloadEvent<G::MediaHandle>>,
    pending_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
    frontiers: &mut VecDeque<MessageFrontier>,
    counters: &mut ExportCounters,
    durable_checkpoint: &mut crate::types::CheckpointState,
    final_retry_jobs: &mut VecDeque<DownloadJob<G::MediaHandle>>,
    concurrency: &mut AdaptiveConcurrency,
    capture_retry_jobs: bool,
) -> Result<()> {
    let event = result?;
    let status = event.record.status;
    database.commit_message(&[event.record], None)?;
    match status {
        crate::types::MediaStatus::Downloaded => {
            counters.downloaded += 1;
            if !capture_retry_jobs {
                counters.failed = counters.failed.saturating_sub(1);
            }
        }
        crate::types::MediaStatus::Failed if !capture_retry_jobs || event.retry_job.is_none() => {
            counters.failed += 1;
        }
        _ => {}
    }

    if capture_retry_jobs {
        if let Some(job) = event.retry_job {
            info!(
                media_key = %job.pending_record.telegram_media_key,
                round = job.scheduler_retry_round,
                "re-queueing failed download immediately"
            );
            pending_jobs.push_front(job);
        } else if let Some(job) = event.final_retry_job {
            final_retry_jobs.push_back(job);
        }
    }

    let flood_wait_count = gateway.pacing_stats().await.flood_wait_count;
    concurrency.observe(
        flood_wait_count,
        status == crate::types::MediaStatus::Downloaded,
    );

    if let Some(frontier) = frontiers
        .iter_mut()
        .find(|frontier| frontier.message_id == event.message_id)
    {
        frontier.remaining_downloads = frontier.remaining_downloads.saturating_sub(1);
    }

    drain_completed_frontier(database, frontiers, durable_checkpoint)
}

fn drain_completed_frontier(
    database: &mut Database,
    frontiers: &mut VecDeque<MessageFrontier>,
    durable_checkpoint: &mut crate::types::CheckpointState,
) -> Result<()> {
    while frontiers
        .front()
        .is_some_and(|frontier| frontier.remaining_downloads == 0)
    {
        let Some(frontier) = frontiers.pop_front() else {
            break;
        };
        if let Some(checkpoint) = frontier.checkpoint {
            database.save_checkpoint(&checkpoint)?;
            *durable_checkpoint = checkpoint;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use std::path::Path;
    use std::sync::Arc;

    use async_trait::async_trait;
    use chrono::Utc;
    use tokio::sync::Mutex;
    use tokio::time::{Duration, timeout};

    use crate::config::{CredentialSource, DownloadConcurrencyOrigin, ProfileSource};
    use crate::fsutil::{build_media_directory, ensure_parent_dir, slugify_chat_title};
    use crate::media::build_filename;
    use crate::telegram::AuthStep;
    use crate::types::{
        ChatKind, ChatSummary, CheckpointState, FilenameMode, MediaDescriptor, MediaKind,
        PacingStats, ScannedMessage,
    };

    struct FakeState {
        batches: VecDeque<Vec<ScannedMessage<String>>>,
        all_messages: Vec<ScannedMessage<String>>,
        downloads: BTreeMap<String, Vec<u8>>,
    }

    #[derive(Clone)]
    struct FakeGateway {
        state: Arc<Mutex<FakeState>>,
    }

    impl FakeGateway {
        fn new(
            batches: Vec<Vec<ScannedMessage<String>>>,
            downloads: BTreeMap<String, Vec<u8>>,
        ) -> Self {
            let all_messages = batches
                .iter()
                .flat_map(|batch| batch.iter().cloned())
                .collect();
            Self {
                state: Arc::new(Mutex::new(FakeState {
                    batches: batches.into(),
                    all_messages,
                    downloads,
                })),
            }
        }
    }

    #[async_trait]
    impl TelegramGateway for FakeGateway {
        type ChatHandle = String;
        type MediaHandle = String;

        async fn is_authorized(&self) -> Result<bool> {
            Ok(true)
        }

        async fn start_auth(&self, _: &str) -> Result<()> {
            Ok(())
        }

        async fn submit_code(&self, _: &str) -> Result<AuthStep> {
            Ok(AuthStep::Authorized)
        }

        async fn submit_password(&self, _: &str) -> Result<()> {
            Ok(())
        }

        async fn list_chats(&self) -> Result<Vec<ChatSummary<Self::ChatHandle>>> {
            Ok(vec![ChatSummary {
                id: 1,
                title: "Demo".to_string(),
                username: Some("demo".to_string()),
                kind: ChatKind::Channel,
                handle: "demo".to_string(),
            }])
        }

        async fn resolve_chat(&self, _: &str) -> Result<ChatSummary<Self::ChatHandle>> {
            Ok(ChatSummary {
                id: 1,
                title: "Demo".to_string(),
                username: Some("demo".to_string()),
                kind: ChatKind::Channel,
                handle: "demo".to_string(),
            })
        }

        async fn fetch_history_batch(
            &self,
            _: &Self::ChatHandle,
            _: Option<i32>,
            _: usize,
        ) -> Result<Vec<ScannedMessage<Self::MediaHandle>>> {
            let mut state = self.state.lock().await;
            Ok(state.batches.pop_front().unwrap_or_default())
        }

        async fn fetch_messages_by_ids(
            &self,
            _: &Self::ChatHandle,
            message_ids: &[i32],
        ) -> Result<Vec<ScannedMessage<Self::MediaHandle>>> {
            let state = self.state.lock().await;
            let mut all = state.all_messages.clone();
            all.retain(|message| message_ids.contains(&message.message_id));
            Ok(all)
        }

        async fn download_media_to_path(
            &self,
            media: &Self::MediaHandle,
            path: &Path,
            _shutdown: &crate::shutdown::ShutdownFlag,
        ) -> Result<()> {
            let state = self.state.lock().await;
            let bytes = state.downloads.get(media).expect("media bytes");
            tokio::fs::write(path, bytes).await?;
            Ok(())
        }

        async fn pacing_stats(&self) -> PacingStats {
            PacingStats::default()
        }
    }

    fn config(download_dir: &Path) -> AppConfig {
        AppConfig {
            profile: "default".to_string(),
            api_profile: "default".to_string(),
            profile_source: ProfileSource::Default,
            credential_source: CredentialSource::Flags,
            api_id: Some(1),
            api_hash: Some("hash".to_string()),
            session_path: download_dir.join("session.db"),
            db_path: download_dir.join("state.db"),
            download_dir: download_dir.join("downloads"),
            log_level: tracing_subscriber::filter::LevelFilter::ERROR,
            retry_count: 0,
            retry_backoff_ms: 1,
            download_stall_timeout_secs: 1,
            media_filter: std::collections::BTreeSet::from(MediaKind::ALL),
            filename_mode: FilenameMode::Stable,
            temp_extension: ".part".to_string(),
            request_delay_ms: 1,
            download_delay_ms: 1,
            flood_sleep_threshold_secs: 5,
            jitter_ms: 0,
            download_concurrency: 3,
            download_concurrency_origin: DownloadConcurrencyOrigin::Auto,
            run_artifact_dir: download_dir.join("run-artifacts"),
            cleanup_stale_parts_on_start: false,
            stale_part_min_age_hours: 12,
            verbose_dependency_logs: false,
        }
    }

    fn photo_message(message_id: i32, bytes: i64) -> ScannedMessage<String> {
        ScannedMessage {
            message_id,
            date: Utc::now(),
            media: vec![MediaDescriptor {
                kind: MediaKind::Photo,
                telegram_media_key: format!("photo:{message_id}"),
                mime_type: Some("image/jpeg".to_string()),
                file_size_bytes: Some(bytes),
                original_name: None,
                handle: format!("photo:{message_id}"),
            }],
        }
    }

    fn export_options(config: &AppConfig) -> ExportOptions {
        ExportOptions {
            chat: "@demo".to_string(),
            out_dir: config.download_dir.clone(),
            resume: false,
            verbose_progress: false,
            media_filter: config.media_filter.clone(),
            since_id: None,
            until_id: None,
            date_from: None,
            date_to: None,
            limit: None,
            rescan: false,
        }
    }

    #[tokio::test]
    async fn exports_media_and_writes_report() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config = config(tempdir.path());
        let mut db = Database::open(&config.db_path).expect("db");
        let gateway = FakeGateway::new(
            vec![
                vec![ScannedMessage {
                    message_id: 10,
                    date: Utc::now(),
                    media: vec![MediaDescriptor {
                        kind: MediaKind::Photo,
                        telegram_media_key: "photo:10".to_string(),
                        mime_type: Some("image/jpeg".to_string()),
                        file_size_bytes: Some(4),
                        original_name: None,
                        handle: "photo:10".to_string(),
                    }],
                }],
                vec![],
            ],
            BTreeMap::from([("photo:10".to_string(), b"demo".to_vec())]),
        );
        let outcome = run_export(
            &gateway,
            &mut db,
            &config,
            ExportOptions {
                chat: "@demo".to_string(),
                out_dir: config.download_dir.clone(),
                resume: false,
                verbose_progress: false,
                media_filter: config.media_filter.clone(),
                since_id: None,
                until_id: None,
                date_from: None,
                date_to: None,
                limit: None,
                rescan: false,
            },
        )
        .await
        .expect("export");
        let ExportRunOutcome::Completed(report) = outcome else {
            panic!("expected completed outcome");
        };
        assert_eq!(report.downloaded, 1);
        assert_eq!(report.media_found, 1);
    }

    #[tokio::test]
    async fn waiting_download_aborts_when_shutdown_requested() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config = config(tempdir.path());
        let gateway = FakeGateway::new(vec![], BTreeMap::new());
        let mut db = Database::open(&config.db_path).expect("db");
        let mut pending_jobs = VecDeque::new();
        let mut in_flight = FuturesUnordered::new();
        let mut abort_handles = Vec::new();
        let mut frontiers = VecDeque::new();
        let mut counters = ExportCounters::default();
        let mut durable_checkpoint = CheckpointState {
            chat_id: 1,
            high_watermark_message_id: None,
            backfill_cursor_message_id: None,
            backfill_complete: false,
        };
        let mut final_retry_jobs = VecDeque::new();
        let mut concurrency = AdaptiveConcurrency::new(1);
        let shutdown = ShutdownFlag::default();
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        abort_handles.push(abort_handle);
        in_flight.push(Abortable::new(
            async move {
                std::future::pending::<()>().await;
                unreachable!("pending download should be aborted")
            }
            .boxed_local(),
            abort_registration,
        ));

        let shutdown_trigger = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            shutdown_trigger.request_for_test();
        });

        timeout(
            Duration::from_secs(1),
            process_next_download(
                &gateway,
                &mut db,
                &mut pending_jobs,
                &mut in_flight,
                &mut abort_handles,
                &mut frontiers,
                &mut counters,
                &mut durable_checkpoint,
                &mut final_retry_jobs,
                &mut concurrency,
                true,
                &shutdown,
            ),
        )
        .await
        .expect("shutdown should abort pending download")
        .expect("processing should not fail");

        assert!(in_flight.is_empty());
        assert!(abort_handles.is_empty());
    }

    #[tokio::test]
    async fn dry_run_plan_does_not_mutate_database() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config = config(tempdir.path());
        let mut db = Database::open(&config.db_path).expect("db");
        let message = photo_message(10, 4);
        let gateway = FakeGateway::new(
            vec![vec![message], vec![]],
            BTreeMap::from([("photo:10".to_string(), b"demo".to_vec())]),
        );

        let report = run_export_plan(&gateway, &mut db, &config, export_options(&config), false)
            .await
            .expect("plan");

        assert_eq!(report.media_found, 1);
        assert_eq!(report.would_queue, 1);
        assert!(db.list_media_for_chat(1).expect("media").is_empty());
        assert!(db.load_checkpoint(1).expect("checkpoint").is_none());
    }

    #[tokio::test]
    async fn save_queue_writes_pending_media_without_checkpoint() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config = config(tempdir.path());
        let mut db = Database::open(&config.db_path).expect("db");
        let message = photo_message(10, 4);
        let gateway = FakeGateway::new(
            vec![vec![message], vec![]],
            BTreeMap::from([("photo:10".to_string(), b"demo".to_vec())]),
        );

        let report = run_export_plan(&gateway, &mut db, &config, export_options(&config), true)
            .await
            .expect("plan");

        assert_eq!(report.plan_id, Some(1));
        let media = db.list_media_for_chat(1).expect("media");
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].status, MediaStatus::Pending);
        assert!(db.load_checkpoint(1).expect("checkpoint").is_none());

        let plan = db
            .latest_complete_export_plan(
                1,
                &config.profile,
                &normalize_plan_output_dir(&config.download_dir).expect("out"),
                &media_filter_key(&config.media_filter),
                &export_scope_hash(&export_options(&config)),
            )
            .expect("plan lookup")
            .expect("plan");
        assert_eq!(plan.queued, 1);
        assert_eq!(plan.planned_high_watermark_message_id, Some(10));
    }

    #[tokio::test]
    async fn save_queue_rejects_bounded_scope() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config = config(tempdir.path());
        let mut db = Database::open(&config.db_path).expect("db");
        let gateway = FakeGateway::new(vec![], BTreeMap::new());
        let mut options = export_options(&config);
        options.limit = Some(1);

        let error = run_export_plan(&gateway, &mut db, &config, options, true)
            .await
            .expect_err("bounded save queue should fail");
        assert!(matches!(error, AppError::InvalidArgument(_)));
    }

    #[test]
    fn plan_scope_hash_is_stable() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config = config(tempdir.path());
        assert_eq!(
            export_scope_hash(&export_options(&config)),
            export_scope_hash(&export_options(&config))
        );
    }

    #[tokio::test]
    async fn export_drains_saved_queue_and_promotes_checkpoint() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config = config(tempdir.path());
        let mut db = Database::open(&config.db_path).expect("db");
        let message = photo_message(10, 4);
        let plan_gateway = FakeGateway::new(
            vec![vec![message.clone()], vec![]],
            BTreeMap::from([("photo:10".to_string(), b"demo".to_vec())]),
        );
        run_export_plan(
            &plan_gateway,
            &mut db,
            &config,
            export_options(&config),
            true,
        )
        .await
        .expect("plan");

        let export_gateway = FakeGateway::new(
            vec![vec![message], vec![]],
            BTreeMap::from([("photo:10".to_string(), b"demo".to_vec())]),
        );
        let outcome = run_export(&export_gateway, &mut db, &config, export_options(&config))
            .await
            .expect("export");
        let ExportRunOutcome::Completed(report) = outcome else {
            panic!("expected completed outcome");
        };

        assert_eq!(report.downloaded, 1);
        let checkpoint = db
            .load_checkpoint(1)
            .expect("checkpoint")
            .expect("checkpoint");
        assert_eq!(checkpoint.high_watermark_message_id, Some(10));
        assert!(checkpoint.backfill_complete);
        let media = db.list_media_for_chat(1).expect("media");
        assert_eq!(media[0].status, MediaStatus::Downloaded);
    }

    #[tokio::test]
    async fn automatic_sync_limit_persists_watermark() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config = config(tempdir.path());
        let mut db = Database::open(&config.db_path).expect("db");
        db.save_checkpoint(&CheckpointState {
            chat_id: 1,
            high_watermark_message_id: Some(3),
            backfill_cursor_message_id: None,
            backfill_complete: true,
        })
        .expect("seed checkpoint");

        let gateway = FakeGateway::new(
            vec![vec![
                ScannedMessage {
                    message_id: 5,
                    date: Utc::now(),
                    media: vec![],
                },
                ScannedMessage {
                    message_id: 4,
                    date: Utc::now(),
                    media: vec![],
                },
            ]],
            BTreeMap::new(),
        );

        let outcome = run_export(
            &gateway,
            &mut db,
            &config,
            ExportOptions {
                chat: "@demo".to_string(),
                out_dir: config.download_dir.clone(),
                resume: false,
                verbose_progress: false,
                media_filter: config.media_filter.clone(),
                since_id: None,
                until_id: None,
                date_from: None,
                date_to: None,
                limit: Some(1),
                rescan: false,
            },
        )
        .await
        .expect("export");
        let ExportRunOutcome::Interrupted(report) = outcome else {
            panic!("expected interrupted outcome");
        };

        assert_eq!(report.scanned_messages, 1);
        let checkpoint = db
            .load_checkpoint(1)
            .expect("load checkpoint")
            .expect("checkpoint");
        assert_eq!(checkpoint.high_watermark_message_id, Some(5));
    }

    #[tokio::test]
    async fn wrong_existing_file_does_not_get_trusted() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config = config(tempdir.path());
        let mut db = Database::open(&config.db_path).expect("db");

        let message_date = Utc::now();
        let directory = build_media_directory(
            &config.download_dir,
            &slugify_chat_title("Demo"),
            MediaKind::Photo,
            message_date,
        );
        let file_name = build_filename(
            10,
            message_date.date_naive(),
            MediaKind::Photo,
            None,
            "photo:10",
            "jpg",
            FilenameMode::Stable,
        );
        let base_path = directory.join(file_name);
        ensure_parent_dir(&base_path).await.expect("parent");
        tokio::fs::write(&base_path, b"wrong")
            .await
            .expect("seed file");

        let gateway = FakeGateway::new(
            vec![
                vec![ScannedMessage {
                    message_id: 10,
                    date: message_date,
                    media: vec![MediaDescriptor {
                        kind: MediaKind::Photo,
                        telegram_media_key: "photo:10".to_string(),
                        mime_type: Some("image/jpeg".to_string()),
                        file_size_bytes: Some(4),
                        original_name: None,
                        handle: "photo:10".to_string(),
                    }],
                }],
                vec![],
            ],
            BTreeMap::from([("photo:10".to_string(), b"demo".to_vec())]),
        );

        let outcome = run_export(
            &gateway,
            &mut db,
            &config,
            ExportOptions {
                chat: "@demo".to_string(),
                out_dir: config.download_dir.clone(),
                resume: false,
                verbose_progress: false,
                media_filter: config.media_filter.clone(),
                since_id: None,
                until_id: None,
                date_from: None,
                date_to: None,
                limit: None,
                rescan: false,
            },
        )
        .await
        .expect("export");
        let ExportRunOutcome::Completed(report) = outcome else {
            panic!("expected completed outcome");
        };

        assert_eq!(report.downloaded, 1);
        let files = std::fs::read_dir(base_path.parent().expect("parent"))
            .expect("read dir")
            .count();
        assert_eq!(files, 2);
    }

    #[test]
    fn stops_on_date_floor() {
        let message = ScannedMessage::<String> {
            message_id: 10,
            date: chrono::DateTime::from_naive_utc_and_offset(
                chrono::NaiveDate::from_ymd_opt(2025, 12, 20)
                    .expect("date")
                    .and_hms_opt(0, 0, 0)
                    .expect("time"),
                Utc,
            ),
            media: vec![],
        };
        let options = ExportOptions {
            chat: "@demo".to_string(),
            out_dir: Path::new("/tmp").to_path_buf(),
            resume: false,
            verbose_progress: false,
            media_filter: std::collections::BTreeSet::from(MediaKind::ALL),
            since_id: None,
            until_id: None,
            date_from: Some(chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date")),
            date_to: None,
            limit: None,
            rescan: false,
        };
        assert!(should_stop_on_message(&message, &options));
    }
}
