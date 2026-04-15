use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::report::ExportCounters;
use crate::types::{ExportOptions, MediaKind};

#[derive(Clone, Copy)]
pub(crate) struct ProgressRuntime {
    pub(crate) worker_limit: usize,
    pub(crate) worker_max: usize,
    pub(crate) active_downloads: usize,
    pub(crate) cooldown_active: bool,
    pub(crate) flood_wait_count: u64,
    pub(crate) queued_downloads: usize,
    pub(crate) frontier_depth: usize,
    pub(crate) history_batches_per_sec: Option<f64>,
    pub(crate) history_messages_per_sec: Option<f64>,
}

#[derive(Clone)]
pub(crate) struct ExportProgress {
    progress: ProgressBar,
    mode_label: String,
    verbose_progress: bool,
}

impl ExportProgress {
    pub(crate) fn new(chat_title: &str, options: &ExportOptions) -> Self {
        let progress = ProgressBar::new_spinner();
        let style = ProgressStyle::with_template("{spinner} {msg}")
            .map(|style| style.tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]))
            .unwrap_or_else(|_| ProgressStyle::default_spinner());
        progress.set_style(style);
        progress.enable_steady_tick(Duration::from_millis(120));

        let mode_label = describe_export_mode(options).to_string();
        let state = Self {
            progress,
            mode_label,
            verbose_progress: options.verbose_progress,
        };
        state.set_resolving(chat_title);
        state
    }

    pub(crate) fn set_resolving(&self, chat_title: &str) {
        self.progress
            .set_message(format!("{} | resolving {}", self.mode_label, chat_title));
    }

    pub(crate) fn set_scanning_new(&self, counters: &ExportCounters, runtime: ProgressRuntime) {
        self.set_state("checking for new messages", counters, runtime, None);
    }

    pub(crate) fn set_scanning_history(&self, counters: &ExportCounters, runtime: ProgressRuntime) {
        self.set_state("scanning history", counters, runtime, None);
    }

    pub(crate) fn set_downloading(
        &self,
        kind: MediaKind,
        message_id: i32,
        counters: &ExportCounters,
        runtime: ProgressRuntime,
    ) {
        self.set_state(
            "downloading",
            counters,
            runtime,
            Some(format!("{kind} from message {message_id}")),
        );
    }

    pub(crate) fn finish(&self) {
        self.progress.finish_and_clear();
    }

    fn set_state(
        &self,
        phase: &str,
        counters: &ExportCounters,
        runtime: ProgressRuntime,
        detail: Option<String>,
    ) {
        self.progress.set_message(build_progress_message(
            &self.mode_label,
            phase,
            counters,
            runtime,
            detail,
            self.verbose_progress,
        ));
    }
}

pub(crate) fn describe_export_mode(options: &ExportOptions) -> &'static str {
    if options.rescan {
        "forced rescan"
    } else if has_bounded_scope(options) {
        "bounded scan"
    } else {
        "automatic sync"
    }
}

pub(crate) fn describe_export_scope(options: &ExportOptions) -> String {
    let mut clauses = Vec::new();

    if let Some(since_id) = options.since_id {
        clauses.push(format!("since message id {since_id}"));
    }
    if let Some(until_id) = options.until_id {
        clauses.push(format!("until message id {until_id}"));
    }
    if let Some(date_from) = options.date_from {
        clauses.push(format!("from {date_from}"));
    }
    if let Some(date_to) = options.date_to {
        clauses.push(format!("to {date_to}"));
    }
    if let Some(limit) = options.limit {
        clauses.push(format!("limit {limit} matched messages"));
    }

    if clauses.is_empty() {
        "entire chat history".to_string()
    } else {
        clauses.join(", ")
    }
}

fn build_progress_message(
    mode_label: &str,
    phase: &str,
    counters: &ExportCounters,
    runtime: ProgressRuntime,
    detail: Option<String>,
    verbose_progress: bool,
) -> String {
    let mut message = format!(
        "{} | {} | scanned={} found={} downloaded={} skipped={} failed={} pending={} active={} workers={} cooldown={}",
        mode_label,
        phase,
        counters.scanned_messages,
        counters.media_found,
        counters.downloaded,
        counters.skipped_existing,
        counters.failed,
        runtime.queued_downloads,
        runtime.active_downloads,
        runtime.worker_limit,
        if runtime.cooldown_active { "yes" } else { "no" },
    );

    if verbose_progress {
        message.push_str(&format!(
            " worker_max={} frontier={} floods={}",
            runtime.worker_max, runtime.frontier_depth, runtime.flood_wait_count
        ));
    }

    if let Some(rate) = runtime.history_batches_per_sec {
        message.push_str(&format!(" history={rate:.1} batches/s"));
    }
    if let Some(rate) = runtime.history_messages_per_sec {
        message.push_str(&format!(" {rate:.1} msgs/s"));
    }

    if let Some(detail) = detail {
        message.push_str(" | ");
        message.push_str(&detail);
    }

    message
}

fn has_bounded_scope(options: &ExportOptions) -> bool {
    options.since_id.is_some()
        || options.until_id.is_some()
        || options.date_from.is_some()
        || options.date_to.is_some()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn base_options() -> ExportOptions {
        ExportOptions {
            chat: "@example".to_string(),
            out_dir: PathBuf::from("downloads"),
            resume: false,
            verbose_progress: false,
            media_filter: std::collections::BTreeSet::from(crate::types::MediaKind::ALL),
            since_id: None,
            until_id: None,
            date_from: None,
            date_to: None,
            limit: None,
            rescan: false,
        }
    }

    #[test]
    fn describes_automatic_sync_mode() {
        let options = base_options();
        assert_eq!(describe_export_mode(&options), "automatic sync");
    }

    #[test]
    fn describes_rescan_mode() {
        let mut options = base_options();
        options.rescan = true;
        assert_eq!(describe_export_mode(&options), "forced rescan");
    }

    #[test]
    fn describes_scope_with_bounds_and_limit() {
        let mut options = base_options();
        options.since_id = Some(100);
        options.date_to =
            Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 5).expect("valid test date"));
        options.limit = Some(50);

        assert_eq!(
            describe_export_scope(&options),
            "since message id 100, to 2026-04-05, limit 50 matched messages"
        );
    }

    #[test]
    fn progress_message_includes_cooldown_marker() {
        let message = build_progress_message(
            "automatic sync",
            "downloading",
            &ExportCounters::default(),
            ProgressRuntime {
                worker_limit: 1,
                worker_max: 4,
                active_downloads: 1,
                cooldown_active: true,
                flood_wait_count: 2,
                queued_downloads: 7,
                frontier_depth: 9,
                history_batches_per_sec: None,
                history_messages_per_sec: None,
            },
            None,
            true,
        );

        assert!(message.contains("cooldown=yes"));
        assert!(message.contains("floods=2"));
    }

    #[test]
    fn progress_message_hides_internal_metrics_by_default() {
        let message = build_progress_message(
            "automatic sync",
            "downloading",
            &ExportCounters::default(),
            ProgressRuntime {
                worker_limit: 3,
                worker_max: 8,
                active_downloads: 2,
                cooldown_active: false,
                flood_wait_count: 4,
                queued_downloads: 17,
                frontier_depth: 32,
                history_batches_per_sec: None,
                history_messages_per_sec: None,
            },
            None,
            false,
        );

        assert!(message.contains("pending=17"));
        assert!(message.contains("active=2"));
        assert!(message.contains("workers=3"));
        assert!(!message.contains("worker_max="));
        assert!(!message.contains("frontier="));
        assert!(!message.contains("floods="));
    }

    #[test]
    fn progress_message_includes_history_batch_rate() {
        let message = build_progress_message(
            "automatic sync",
            "scanning history",
            &ExportCounters::default(),
            ProgressRuntime {
                worker_limit: 1,
                worker_max: 4,
                active_downloads: 0,
                cooldown_active: false,
                flood_wait_count: 0,
                queued_downloads: 32,
                frontier_depth: 64,
                history_batches_per_sec: Some(3.4),
                history_messages_per_sec: None,
            },
            None,
            false,
        );

        assert!(message.contains("history=3.4 batches/s"));
    }

    #[test]
    fn progress_message_includes_history_message_rate() {
        let message = build_progress_message(
            "automatic sync",
            "scanning history",
            &ExportCounters::default(),
            ProgressRuntime {
                worker_limit: 1,
                worker_max: 4,
                active_downloads: 0,
                cooldown_active: false,
                flood_wait_count: 0,
                queued_downloads: 32,
                frontier_depth: 64,
                history_batches_per_sec: Some(3.4),
                history_messages_per_sec: Some(312.0),
            },
            None,
            false,
        );

        assert!(message.contains("312.0 msgs/s"));
    }
}
