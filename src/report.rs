use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::types::{MediaKind, PacingStats};

#[derive(Debug, Clone, Default)]
pub struct ExportCounters {
    pub scanned_messages: usize,
    pub media_found: usize,
    pub downloaded: usize,
    pub skipped_existing: usize,
    pub failed: usize,
    pub per_kind: BTreeMap<MediaKind, usize>,
}

impl ExportCounters {
    pub fn record_found(&mut self, kind: MediaKind) {
        self.media_found += 1;
        *self.per_kind.entry(kind).or_insert(0) += 1;
    }
}

#[derive(Debug, Clone)]
pub struct ExportReportInput {
    pub chat_id: i64,
    pub chat_title: String,
    pub last_checkpoint_message_id: Option<i32>,
    pub output_dir: PathBuf,
    pub duration: Duration,
    pub counters: ExportCounters,
    pub pacing_stats: PacingStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportReport {
    pub chat_id: i64,
    pub chat_title: String,
    pub scanned_messages: usize,
    pub media_found: usize,
    pub downloaded: usize,
    pub skipped_existing: usize,
    pub failed: usize,
    pub last_checkpoint_message_id: Option<i32>,
    pub output_dir: PathBuf,
    pub duration_ms: u128,
    pub per_kind: BTreeMap<String, usize>,
    pub flood_wait_count: u64,
    pub flood_sleep_ms_total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Succeeded,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunArtifact {
    pub run_id: i64,
    pub operation: String,
    pub requested_chat: Option<String>,
    pub finished_at: DateTime<Utc>,
    pub outcome: RunOutcome,
    pub report: Option<ExportReport>,
    pub error: Option<String>,
}

impl ExportReport {
    pub fn human(&self) -> String {
        let mut lines = vec![
            format!(
                "Target chat        : {} ({})",
                self.chat_title, self.chat_id
            ),
            format!("Scanned messages   : {}", self.scanned_messages),
            format!("Media found        : {}", self.media_found),
            format!("Downloaded         : {}", self.downloaded),
            format!("Skipped existing   : {}", self.skipped_existing),
            format!("Failed             : {}", self.failed),
            format!(
                "Checkpoint         : {}",
                self.last_checkpoint_message_id
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string())
            ),
            format!("Output directory   : {}", self.output_dir.display()),
            format!("Duration           : {}", format_duration(self.duration_ms)),
            format!("Flood waits        : {}", self.flood_wait_count),
        ];

        if !self.per_kind.is_empty() {
            let kinds = self
                .per_kind
                .iter()
                .map(|(kind, count)| format!("{kind}={count}"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Per kind           : {kinds}"));
        }

        lines.join("\n")
    }

    pub fn to_json_pretty(&self) -> std::result::Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

impl RunArtifact {
    pub fn success(
        run_id: i64,
        operation: &str,
        requested_chat: Option<String>,
        report: ExportReport,
    ) -> Self {
        Self {
            run_id,
            operation: operation.to_string(),
            requested_chat,
            finished_at: Utc::now(),
            outcome: RunOutcome::Succeeded,
            report: Some(report),
            error: None,
        }
    }

    pub fn failure(
        run_id: i64,
        operation: &str,
        requested_chat: Option<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            run_id,
            operation: operation.to_string(),
            requested_chat,
            finished_at: Utc::now(),
            outcome: RunOutcome::Failed,
            report: None,
            error: Some(error.into()),
        }
    }

    pub fn interrupted(
        run_id: i64,
        operation: &str,
        requested_chat: Option<String>,
        report: ExportReport,
        message: impl Into<String>,
    ) -> Self {
        Self {
            run_id,
            operation: operation.to_string(),
            requested_chat,
            finished_at: Utc::now(),
            outcome: RunOutcome::Interrupted,
            report: Some(report),
            error: Some(message.into()),
        }
    }

    pub fn to_json_pretty(&self) -> std::result::Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

impl From<ExportReportInput> for ExportReport {
    fn from(value: ExportReportInput) -> Self {
        Self {
            chat_id: value.chat_id,
            chat_title: value.chat_title,
            scanned_messages: value.counters.scanned_messages,
            media_found: value.counters.media_found,
            downloaded: value.counters.downloaded,
            skipped_existing: value.counters.skipped_existing,
            failed: value.counters.failed,
            last_checkpoint_message_id: value.last_checkpoint_message_id,
            output_dir: value.output_dir,
            duration_ms: value.duration.as_millis(),
            per_kind: value
                .counters
                .per_kind
                .into_iter()
                .map(|(kind, count)| (kind.as_str().to_string(), count))
                .collect(),
            flood_wait_count: value.pacing_stats.flood_wait_count,
            flood_sleep_ms_total: value.pacing_stats.flood_sleep_ms_total,
        }
    }
}

fn format_duration(duration_ms: u128) -> String {
    let duration = Duration::from_millis(duration_ms as u64);
    let seconds = duration.as_secs();
    let millis = duration.subsec_millis();
    format!("{seconds}.{millis:03}s")
}
