use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::config::AppConfig;
use crate::error::{AppError, Result};
use crate::fsutil::compute_sha256_async;
use crate::storage::{Database, StoredMediaRecord};
use crate::telegram::{RealTelegramGateway, TelegramGateway};
use crate::types::MediaStatus;

pub struct VerifyCommand {
    pub chat: String,
    pub out_dir: Option<PathBuf>,
    pub deep: bool,
    pub json: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct VerifyReport {
    pub chat_id: i64,
    pub chat_title: Option<String>,
    pub total_tracked: usize,
    pub outside_output_root: usize,
    pub ok: usize,
    pub missing: usize,
    pub size_mismatch: usize,
    pub hash_mismatch: usize,
    pub failed_status: usize,
    pub pending_status: usize,
    pub unreadable: usize,
    pub checked_bytes: u64,
    pub duration_ms: u128,
    pub deep: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<VerifyProblem>,
}

#[derive(Debug, Serialize)]
pub struct VerifyProblem {
    pub problem: &'static str,
    pub message_id: i32,
    pub media_kind: &'static str,
    pub local_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_size_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl VerifyReport {
    fn problem_count(&self) -> usize {
        self.missing
            + self.size_mismatch
            + self.hash_mismatch
            + self.failed_status
            + self.pending_status
            + self.unreadable
    }

    fn human(&self) -> String {
        let title = self.chat_title.as_deref().unwrap_or("unknown");
        let mut output = format!(
            "\
Verify report
Chat              : {title} ({chat_id})
Mode              : {mode}
Total tracked     : {total_tracked}
Outside --out     : {outside_output_root}
OK                : {ok}
Missing           : {missing}
Size mismatch     : {size_mismatch}
Hash mismatch     : {hash_mismatch}
Failed status     : {failed_status}
Pending status    : {pending_status}
Unreadable        : {unreadable}
Checked bytes     : {checked_bytes}
Duration          : {duration:.3}s",
            chat_id = self.chat_id,
            mode = if self.deep { "deep" } else { "fast" },
            total_tracked = self.total_tracked,
            outside_output_root = self.outside_output_root,
            ok = self.ok,
            missing = self.missing,
            size_mismatch = self.size_mismatch,
            hash_mismatch = self.hash_mismatch,
            failed_status = self.failed_status,
            pending_status = self.pending_status,
            unreadable = self.unreadable,
            checked_bytes = self.checked_bytes,
            duration = self.duration_ms as f64 / 1_000.0,
        );

        if !self.problems.is_empty() {
            output.push_str("\n\nProblems");
            for problem in &self.problems {
                output.push_str(&format!(
                    "\n- {problem_type}: message {message_id}, {media_kind}, {path}",
                    problem_type = problem.problem,
                    message_id = problem.message_id,
                    media_kind = problem.media_kind,
                    path = problem.local_path.display(),
                ));
                if let (Some(expected), Some(actual)) =
                    (problem.expected_size_bytes, problem.actual_size_bytes)
                {
                    output.push_str(&format!(" (expected {expected} bytes, got {actual} bytes)"));
                }
                if let Some(status) = problem.status.as_deref() {
                    output.push_str(&format!(" (status {status})"));
                }
                if let Some(error) = problem.error.as_deref() {
                    output.push_str(&format!(" ({error})"));
                }
            }
        }

        output
    }
}

pub async fn run(config: &AppConfig, command: VerifyCommand) -> Result<()> {
    let started = Instant::now();
    let database = Database::open_readonly(&config.db_path)?;
    let chat = resolve_verify_chat(config, &database, &command.chat).await?;
    let records = database.list_media_for_chat(chat.chat_id)?;
    let output_root = command
        .out_dir
        .as_deref()
        .map(normalize_for_prefix)
        .transpose()?;

    let mut report = verify_records(records, output_root.as_deref(), command.deep).await?;
    report.chat_id = chat.chat_id;
    report.chat_title = Some(chat.title);
    report.duration_ms = started.elapsed().as_millis();
    report.deep = command.deep;

    if command.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", report.human());
    }

    if report.problem_count() > 0 {
        return Err(AppError::Runtime(format!(
            "verification found {} problem(s)",
            report.problem_count()
        )));
    }

    Ok(())
}

async fn resolve_verify_chat(
    config: &AppConfig,
    database: &Database,
    query: &str,
) -> Result<VerifyChat> {
    if let Some(chat) = database.find_chat(query)? {
        return Ok(VerifyChat {
            chat_id: chat.chat_id,
            title: chat.title,
        });
    }

    if query.parse::<i64>().is_ok() {
        return Err(AppError::ChatResolution(format!(
            "chat `{query}` is not known in the local state DB; run an export first"
        )));
    }

    if config.telegram_credentials().is_err() || !config.session_path.exists() {
        return Err(AppError::ChatResolution(format!(
            "chat `{query}` is not known locally; use a numeric chat id for offline verify or run `tgbacky auth` first"
        )));
    }

    let gateway = RealTelegramGateway::new(config).await?;
    if !gateway.is_authorized().await? {
        return Err(AppError::Authentication(format!(
            "profile `{}` is not authorized; run `tgbacky auth --profile {}`",
            config.profile, config.profile
        )));
    }
    let chat = gateway.resolve_chat(query).await?;
    Ok(VerifyChat {
        chat_id: chat.id,
        title: chat.title,
    })
}

#[derive(Debug)]
struct VerifyChat {
    chat_id: i64,
    title: String,
}

async fn verify_records(
    records: Vec<StoredMediaRecord>,
    output_root: Option<&Path>,
    deep: bool,
) -> Result<VerifyReport> {
    let mut report = VerifyReport {
        total_tracked: records.len(),
        deep,
        ..VerifyReport::default()
    };

    for record in records {
        if let Some(root) = output_root {
            let local_path = normalize_for_prefix(&record.local_path)?;
            if !local_path.starts_with(root) {
                report.outside_output_root += 1;
                continue;
            }
        }

        match record.status {
            MediaStatus::Downloaded | MediaStatus::SkippedExisting => {}
            MediaStatus::Failed => {
                report.failed_status += 1;
                report.problems.push(VerifyProblem {
                    problem: "failed_status",
                    message_id: record.message_id,
                    media_kind: record.kind.as_str(),
                    local_path: record.local_path,
                    expected_size_bytes: record.file_size_bytes,
                    actual_size_bytes: None,
                    expected_sha256: None,
                    actual_sha256: None,
                    status: Some(record.status.as_str().to_string()),
                    error: record.error_message,
                });
                continue;
            }
            MediaStatus::Pending | MediaStatus::Downloading => {
                report.pending_status += 1;
                report.problems.push(VerifyProblem {
                    problem: "pending_status",
                    message_id: record.message_id,
                    media_kind: record.kind.as_str(),
                    local_path: record.local_path,
                    expected_size_bytes: record.file_size_bytes,
                    actual_size_bytes: None,
                    expected_sha256: None,
                    actual_sha256: None,
                    status: Some(record.status.as_str().to_string()),
                    error: record.error_message,
                });
                continue;
            }
        }

        let metadata = match tokio::fs::metadata(&record.local_path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                report.missing += 1;
                report.problems.push(VerifyProblem {
                    problem: "missing",
                    message_id: record.message_id,
                    media_kind: record.kind.as_str(),
                    local_path: record.local_path,
                    expected_size_bytes: record.file_size_bytes,
                    actual_size_bytes: None,
                    expected_sha256: None,
                    actual_sha256: None,
                    status: Some(record.status.as_str().to_string()),
                    error: Some(error.to_string()),
                });
                continue;
            }
            Err(_) => {
                report.unreadable += 1;
                report.problems.push(VerifyProblem {
                    problem: "unreadable",
                    message_id: record.message_id,
                    media_kind: record.kind.as_str(),
                    local_path: record.local_path,
                    expected_size_bytes: record.file_size_bytes,
                    actual_size_bytes: None,
                    expected_sha256: None,
                    actual_sha256: None,
                    status: Some(record.status.as_str().to_string()),
                    error: None,
                });
                continue;
            }
        };

        if !metadata.is_file() {
            report.unreadable += 1;
            report.problems.push(VerifyProblem {
                problem: "unreadable",
                message_id: record.message_id,
                media_kind: record.kind.as_str(),
                local_path: record.local_path,
                expected_size_bytes: record.file_size_bytes,
                actual_size_bytes: Some(metadata.len()),
                expected_sha256: None,
                actual_sha256: None,
                status: Some(record.status.as_str().to_string()),
                error: Some("path is not a file".to_string()),
            });
            continue;
        }

        let actual_size = metadata.len();
        report.checked_bytes = report.checked_bytes.saturating_add(actual_size);
        let size_mismatched = record
            .file_size_bytes
            .is_some_and(|expected_size| expected_size >= 0 && actual_size != expected_size as u64);

        if deep && let Some(expected_hash) = record.sha256.as_deref() {
            let actual_hash = compute_sha256_async(&record.local_path).await?;
            if size_mismatched && actual_hash == expected_hash {
                report.ok += 1;
                continue;
            }
            if actual_hash != expected_hash {
                report.hash_mismatch += 1;
                report.problems.push(VerifyProblem {
                    problem: "hash_mismatch",
                    message_id: record.message_id,
                    media_kind: record.kind.as_str(),
                    local_path: record.local_path,
                    expected_size_bytes: record.file_size_bytes,
                    actual_size_bytes: Some(actual_size),
                    expected_sha256: record.sha256.clone(),
                    actual_sha256: Some(actual_hash),
                    status: Some(record.status.as_str().to_string()),
                    error: None,
                });
                continue;
            }
        }

        if size_mismatched {
            report.size_mismatch += 1;
            report.problems.push(VerifyProblem {
                problem: "size_mismatch",
                message_id: record.message_id,
                media_kind: record.kind.as_str(),
                local_path: record.local_path,
                expected_size_bytes: record.file_size_bytes,
                actual_size_bytes: Some(actual_size),
                expected_sha256: None,
                actual_sha256: None,
                status: Some(record.status.as_str().to_string()),
                error: None,
            });
            continue;
        }

        report.ok += 1;
    }

    Ok(report)
}

fn normalize_for_prefix(path: &Path) -> Result<PathBuf> {
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

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::types::MediaKind;

    fn record(path: PathBuf, size: Option<i64>, status: MediaStatus) -> StoredMediaRecord {
        StoredMediaRecord {
            chat_id: 1,
            message_id: 1,
            message_date: Utc::now(),
            kind: MediaKind::Photo,
            telegram_media_key: "photo:1".to_string(),
            status,
            local_path: path,
            file_size_bytes: size,
            sha256: None,
            error_message: None,
        }
    }

    #[tokio::test]
    async fn verify_records_catches_missing_and_size_mismatch() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let ok_path = tempdir.path().join("ok.jpg");
        tokio::fs::write(&ok_path, b"abcd").await.expect("write ok");
        let bad_path = tempdir.path().join("bad.jpg");
        tokio::fs::write(&bad_path, b"x").await.expect("write bad");

        let report = verify_records(
            vec![
                record(ok_path, Some(4), MediaStatus::Downloaded),
                record(bad_path.clone(), Some(4), MediaStatus::Downloaded),
                record(
                    tempdir.path().join("missing.jpg"),
                    Some(1),
                    MediaStatus::Downloaded,
                ),
                record(tempdir.path().join("failed.jpg"), None, MediaStatus::Failed),
            ],
            None,
            false,
        )
        .await
        .expect("verify");

        assert_eq!(report.ok, 1);
        assert_eq!(report.size_mismatch, 1);
        assert_eq!(report.missing, 1);
        assert_eq!(report.failed_status, 1);
        assert_eq!(report.problems.len(), 3);
        assert!(
            report
                .problems
                .iter()
                .any(|problem| problem.problem == "size_mismatch"
                    && problem.local_path == bad_path
                    && problem.expected_size_bytes == Some(4)
                    && problem.actual_size_bytes == Some(1))
        );
    }

    #[tokio::test]
    async fn deep_verify_accepts_size_mismatch_when_hash_matches() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("normalized.jpg");
        tokio::fs::write(&path, b"x").await.expect("write");

        let mut item = record(path, Some(4), MediaStatus::Downloaded);
        item.sha256 = Some(compute_sha256_async(&item.local_path).await.expect("hash"));

        let report = verify_records(vec![item], None, true)
            .await
            .expect("verify");

        assert_eq!(report.ok, 1);
        assert_eq!(report.size_mismatch, 0);
        assert_eq!(report.hash_mismatch, 0);
        assert!(report.problems.is_empty());
    }
}
