use crate::config::AppConfig;
use crate::error::Result;
use crate::storage::{Database, RunRecord, RunStatus};

pub async fn list(config: &AppConfig, limit: usize, failed_only: bool) -> Result<()> {
    let database = Database::open(&config.db_path)?;
    let runs = database.list_runs(limit, failed_only)?;
    print_runs(&runs);
    Ok(())
}

fn print_runs(runs: &[RunRecord]) {
    println!(
        "{:>6}  {:<9}  {:<12}  {:<16}  {:>6}  {:>6}  started_at",
        "run_id", "status", "operation", "chat", "dl", "fail"
    );
    for run in runs {
        let chat = run
            .chat_title
            .as_deref()
            .or(run.requested_chat.as_deref())
            .unwrap_or("-");
        println!(
            "{:>6}  {:<9}  {:<12}  {:<16}  {:>6}  {:>6}  {}",
            run.id,
            format_run_status(run.status),
            run.operation,
            truncate(chat, 16),
            run.downloaded,
            run.failed,
            run.started_at
        );
        if let Some(error) = &run.error_message {
            println!("         error: {error}");
        }
        if let Some(path) = &run.artifact_path {
            println!("         artifact: {}", path.display());
        }
    }
}

fn format_run_status(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Interrupted => "interrupt",
        RunStatus::Failed => "failed",
    }
}

fn truncate(value: &str, width: usize) -> String {
    let mut output = value.chars().take(width).collect::<String>();
    if value.chars().count() > width && width > 1 {
        output.pop();
        output.push('~');
    }
    output
}
