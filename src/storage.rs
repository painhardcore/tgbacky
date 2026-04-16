mod migrations;
mod records;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};

use crate::error::Result;
use crate::report::ExportReport;
use crate::types::{ChatSummary, CheckpointState, MediaKind, MediaStatus};
use migrations::migrations;
use records::{run_record_from_row, stored_media_from_row};

#[derive(Debug, Clone)]
pub struct StoredMediaRecord {
    pub chat_id: i64,
    pub message_id: i32,
    pub message_date: DateTime<Utc>,
    pub kind: MediaKind,
    pub telegram_media_key: String,
    pub status: MediaStatus,
    pub local_path: PathBuf,
    pub file_size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoredChatRecord {
    pub chat_id: i64,
    pub title: String,
    pub username: Option<String>,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct PersistedMediaItem {
    pub chat_id: i64,
    pub message_id: i32,
    pub message_date: DateTime<Utc>,
    pub kind: MediaKind,
    pub telegram_media_key: String,
    pub mime_type: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub local_path: PathBuf,
    pub sha256: Option<String>,
    pub status: MediaStatus,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Succeeded,
    Interrupted,
    Failed,
}

impl RunStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "interrupted" => Self::Interrupted,
            "failed" => Self::Failed,
            _ => Self::Failed,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunRecord {
    pub id: i64,
    pub operation: String,
    pub requested_chat: Option<String>,
    pub chat_id: Option<i64>,
    pub chat_title: Option<String>,
    pub output_dir: Option<PathBuf>,
    pub status: RunStatus,
    pub scanned_messages: usize,
    pub media_found: usize,
    pub downloaded: usize,
    pub skipped_existing: usize,
    pub failed: usize,
    pub flood_wait_count: u64,
    pub flood_sleep_ms_total: u64,
    pub last_checkpoint_message_id: Option<i32>,
    pub error_message: Option<String>,
    pub artifact_path: Option<PathBuf>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExportPlanRecord {
    pub id: i64,
    pub chat_id: i64,
    pub profile: String,
    pub output_dir: PathBuf,
    pub media_filter: String,
    pub scope_hash: String,
    pub planned_high_watermark_message_id: Option<i32>,
    pub planned_backfill_cursor_message_id: Option<i32>,
    pub planned_backfill_complete: bool,
    pub scanned_messages: usize,
    pub media_found: usize,
    pub queued: usize,
    pub estimated_bytes: u64,
    pub per_kind_json: String,
}

#[derive(Debug, Clone)]
pub struct NewExportPlan<'a> {
    pub chat_id: i64,
    pub profile: &'a str,
    pub output_dir: &'a Path,
    pub media_filter: &'a str,
    pub scope_hash: &'a str,
}

#[derive(Debug, Clone)]
pub struct CompleteExportPlan<'a> {
    pub id: i64,
    pub planned_high_watermark_message_id: Option<i32>,
    pub planned_backfill_cursor_message_id: Option<i32>,
    pub planned_backfill_complete: bool,
    pub scanned_messages: usize,
    pub media_found: usize,
    pub queued: usize,
    pub estimated_bytes: u64,
    pub per_kind_json: &'a str,
}

struct Migration {
    version: i64,
    apply: fn(&Transaction<'_>) -> rusqlite::Result<()>,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        let mut database = Self { conn };
        database.initialize()?;
        Ok(database)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let mut database = Self { conn };
        database.initialize()?;
        Ok(database)
    }

    pub fn open_readonly(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(crate::error::AppError::Config(format!(
                "state DB does not exist at {}",
                path.display()
            )));
        }

        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self { conn })
    }

    fn initialize(&mut self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );
            "#,
        )?;

        let current_version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;

        for migration in migrations()
            .iter()
            .filter(|migration| migration.version > current_version)
        {
            let tx = self.conn.transaction()?;
            (migration.apply)(&tx)?;
            tx.execute(
                "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                params![migration.version, now_rfc3339()],
            )?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", migration.version))?;
            tx.commit()?;
        }

        Ok(())
    }

    pub fn upsert_chat<H>(&mut self, chat: &ChatSummary<H>) -> Result<()> {
        let now = now_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO chats (chat_id, title, username, kind, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ON CONFLICT(chat_id) DO UPDATE SET
                title = excluded.title,
                username = excluded.username,
                kind = excluded.kind,
                updated_at = excluded.updated_at
            "#,
            params![
                chat.id,
                chat.title,
                chat.username,
                chat.kind.to_string(),
                now
            ],
        )?;
        Ok(())
    }

    pub fn load_checkpoint(&self, chat_id: i64) -> Result<Option<CheckpointState>> {
        self.conn
            .query_row(
                r#"
                SELECT chat_id, high_watermark_message_id, backfill_cursor_message_id, backfill_complete
                FROM checkpoints
                WHERE chat_id = ?1
                "#,
                params![chat_id],
                |row| {
                    Ok(CheckpointState {
                        chat_id: row.get(0)?,
                        high_watermark_message_id: row.get(1)?,
                        backfill_cursor_message_id: row.get(2)?,
                        backfill_complete: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn save_checkpoint(&mut self, checkpoint: &CheckpointState) -> Result<()> {
        let tx = self.conn.transaction()?;
        save_checkpoint_tx(&tx, checkpoint)?;
        tx.commit()?;
        Ok(())
    }

    pub fn commit_message(
        &mut self,
        media_items: &[PersistedMediaItem],
        checkpoint: Option<&CheckpointState>,
    ) -> Result<()> {
        if media_items.is_empty() && checkpoint.is_none() {
            return Ok(());
        }

        let tx = self.conn.transaction()?;
        for record in media_items {
            upsert_media_tx(&tx, record)?;
        }
        if let Some(checkpoint) = checkpoint {
            save_checkpoint_tx(&tx, checkpoint)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn find_media_by_key(
        &self,
        chat_id: i64,
        message_id: i32,
        telegram_media_key: &str,
    ) -> Result<Option<StoredMediaRecord>> {
        self.conn
            .query_row(
                r#"
                SELECT
                    chat_id,
                    message_id,
                    message_date,
                    kind,
                    telegram_media_key,
                    status,
                    local_path,
                    sha256,
                    error_message,
                    file_size_bytes
                FROM media_items
                WHERE chat_id = ?1 AND message_id = ?2 AND telegram_media_key = ?3
                "#,
                params![chat_id, message_id, telegram_media_key],
                stored_media_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn find_media_by_path(&self, path: &Path) -> Result<Option<StoredMediaRecord>> {
        self.conn
            .query_row(
                r#"
                SELECT
                    chat_id,
                    message_id,
                    message_date,
                    kind,
                    telegram_media_key,
                    status,
                    local_path,
                    sha256,
                    error_message,
                    file_size_bytes
                FROM media_items
                WHERE local_path = ?1
                "#,
                params![path.to_string_lossy().to_string()],
                stored_media_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_retry_message_ids(&self, chat_id: i64, limit: usize) -> Result<Vec<i32>> {
        let mut statement = self.conn.prepare(
            r#"
            SELECT DISTINCT message_id
            FROM media_items
            WHERE chat_id = ?1
              AND status IN ('pending', 'downloading', 'failed')
            ORDER BY message_id DESC
            LIMIT ?2
            "#,
        )?;
        let rows = statement.query_map(params![chat_id, limit as i64], |row| row.get(0))?;
        let mut message_ids = Vec::new();
        for row in rows {
            message_ids.push(row?);
        }
        Ok(message_ids)
    }

    pub fn list_all_retry_message_ids(&self, chat_id: i64) -> Result<Vec<i32>> {
        let mut statement = self.conn.prepare(
            r#"
            SELECT DISTINCT message_id
            FROM media_items
            WHERE chat_id = ?1
              AND status IN ('pending', 'downloading', 'failed')
            ORDER BY message_id DESC
            "#,
        )?;
        let rows = statement.query_map(params![chat_id], |row| row.get(0))?;
        let mut message_ids = Vec::new();
        for row in rows {
            message_ids.push(row?);
        }
        Ok(message_ids)
    }

    pub fn start_export_plan(&mut self, plan: NewExportPlan<'_>) -> Result<i64> {
        let now = now_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO export_plans (
                chat_id,
                profile,
                output_dir,
                media_filter,
                scope_hash,
                status,
                per_kind_json,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, 'running', '{}', ?6, ?6)
            "#,
            params![
                plan.chat_id,
                plan.profile,
                plan.output_dir.to_string_lossy().to_string(),
                plan.media_filter,
                plan.scope_hash,
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn complete_export_plan(&mut self, plan: CompleteExportPlan<'_>) -> Result<()> {
        let updated = self.conn.execute(
            r#"
            UPDATE export_plans
            SET
                status = 'complete',
                planned_high_watermark_message_id = ?2,
                planned_backfill_cursor_message_id = ?3,
                planned_backfill_complete = ?4,
                scanned_messages = ?5,
                media_found = ?6,
                queued = ?7,
                estimated_bytes = ?8,
                per_kind_json = ?9,
                updated_at = ?10,
                completed_at = ?10
            WHERE id = ?1
            "#,
            params![
                plan.id,
                plan.planned_high_watermark_message_id,
                plan.planned_backfill_cursor_message_id,
                if plan.planned_backfill_complete {
                    1_i64
                } else {
                    0_i64
                },
                plan.scanned_messages as i64,
                plan.media_found as i64,
                plan.queued as i64,
                plan.estimated_bytes as i64,
                plan.per_kind_json,
                now_rfc3339(),
            ],
        )?;
        if updated != 1 {
            return Err(crate::error::AppError::Runtime(format!(
                "export plan {} was not completed",
                plan.id
            )));
        }
        Ok(())
    }

    pub fn interrupt_export_plan(&mut self, plan_id: i64) -> Result<()> {
        self.update_export_plan_status(plan_id, "interrupted")
    }

    pub fn supersede_export_plan(&mut self, plan_id: i64) -> Result<()> {
        self.update_export_plan_status(plan_id, "superseded")
    }

    pub fn supersede_complete_export_plans_for_chat(
        &mut self,
        chat_id: i64,
        profile: &str,
    ) -> Result<usize> {
        let updated = self.conn.execute(
            r#"
            UPDATE export_plans
            SET status = 'superseded', updated_at = ?3
            WHERE chat_id = ?1
              AND profile = ?2
              AND status = 'complete'
            "#,
            params![chat_id, profile, now_rfc3339()],
        )?;
        Ok(updated)
    }

    fn update_export_plan_status(&mut self, plan_id: i64, status: &str) -> Result<()> {
        let updated = self.conn.execute(
            r#"
            UPDATE export_plans
            SET status = ?2, updated_at = ?3
            WHERE id = ?1
            "#,
            params![plan_id, status, now_rfc3339()],
        )?;
        if updated != 1 {
            return Err(crate::error::AppError::Runtime(format!(
                "export plan {plan_id} was not updated"
            )));
        }
        Ok(())
    }

    pub fn latest_complete_export_plan(
        &self,
        chat_id: i64,
        profile: &str,
        output_dir: &Path,
        media_filter: &str,
        scope_hash: &str,
    ) -> Result<Option<ExportPlanRecord>> {
        self.conn
            .query_row(
                r#"
                SELECT
                    id,
                    chat_id,
                    profile,
                    output_dir,
                    media_filter,
                    scope_hash,
                    planned_high_watermark_message_id,
                    planned_backfill_cursor_message_id,
                    planned_backfill_complete,
                    scanned_messages,
                    media_found,
                    queued,
                    estimated_bytes,
                    per_kind_json
                FROM export_plans
                WHERE chat_id = ?1
                  AND profile = ?2
                  AND output_dir = ?3
                  AND media_filter = ?4
                  AND scope_hash = ?5
                  AND status = 'complete'
                ORDER BY id DESC
                LIMIT 1
                "#,
                params![
                    chat_id,
                    profile,
                    output_dir.to_string_lossy().to_string(),
                    media_filter,
                    scope_hash
                ],
                export_plan_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_media_for_chat(&self, chat_id: i64) -> Result<Vec<StoredMediaRecord>> {
        let mut statement = self.conn.prepare(
            r#"
            SELECT
                chat_id,
                message_id,
                message_date,
                kind,
                telegram_media_key,
                status,
                local_path,
                sha256,
                error_message,
                file_size_bytes
            FROM media_items
            WHERE chat_id = ?1
            ORDER BY message_id DESC
            "#,
        )?;
        let rows = statement.query_map(params![chat_id], stored_media_from_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list_retry_media_for_message(
        &self,
        chat_id: i64,
        message_id: i32,
    ) -> Result<Vec<StoredMediaRecord>> {
        let mut statement = self.conn.prepare(
            r#"
            SELECT
                chat_id,
                message_id,
                message_date,
                kind,
                telegram_media_key,
                status,
                local_path,
                sha256,
                error_message,
                file_size_bytes
            FROM media_items
            WHERE chat_id = ?1
              AND message_id = ?2
              AND status IN ('pending', 'downloading', 'failed')
            ORDER BY telegram_media_key
            "#,
        )?;
        let rows = statement.query_map(params![chat_id, message_id], stored_media_from_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn find_chat(&self, query: &str) -> Result<Option<StoredChatRecord>> {
        if let Ok(chat_id) = query.parse::<i64>() {
            return self.find_chat_by_id(chat_id);
        }

        let username = query.trim().strip_prefix('@').unwrap_or(query.trim());
        self.conn
            .query_row(
                r#"
                SELECT chat_id, title, username, kind
                FROM chats
                WHERE username = ?1 OR title = ?2
                ORDER BY updated_at DESC
                LIMIT 1
                "#,
                params![username, query.trim()],
                stored_chat_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn find_chat_by_id(&self, chat_id: i64) -> Result<Option<StoredChatRecord>> {
        self.conn
            .query_row(
                r#"
                SELECT chat_id, title, username, kind
                FROM chats
                WHERE chat_id = ?1
                "#,
                params![chat_id],
                stored_chat_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn reset_chat_state(&mut self, chat_id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM media_items WHERE chat_id = ?1",
            params![chat_id],
        )?;
        tx.execute(
            "DELETE FROM checkpoints WHERE chat_id = ?1",
            params![chat_id],
        )?;
        tx.execute("DELETE FROM chats WHERE chat_id = ?1", params![chat_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn start_run(
        &mut self,
        operation: &str,
        requested_chat: Option<&str>,
        output_dir: Option<&Path>,
    ) -> Result<i64> {
        let started_at = now_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO export_runs (
                operation,
                requested_chat,
                output_dir,
                status,
                started_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                operation,
                requested_chat,
                output_dir.map(|path| path.to_string_lossy().to_string()),
                RunStatus::Running.as_str(),
                started_at
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_run_success(
        &mut self,
        run_id: i64,
        report: &ExportReport,
        artifact_path: Option<&Path>,
    ) -> Result<()> {
        let updated = self.conn.execute(
            r#"
            UPDATE export_runs
            SET
                chat_id = ?2,
                chat_title = ?3,
                output_dir = ?4,
                status = ?5,
                scanned_messages = ?6,
                media_found = ?7,
                downloaded = ?8,
                skipped_existing = ?9,
                failed = ?10,
                flood_wait_count = ?11,
                flood_sleep_ms_total = ?12,
                last_checkpoint_message_id = ?13,
                artifact_path = ?14,
                error_message = NULL,
                finished_at = ?15
            WHERE id = ?1
            "#,
            params![
                run_id,
                report.chat_id,
                report.chat_title,
                report.output_dir.to_string_lossy().to_string(),
                RunStatus::Succeeded.as_str(),
                report.scanned_messages as i64,
                report.media_found as i64,
                report.downloaded as i64,
                report.skipped_existing as i64,
                report.failed as i64,
                report.flood_wait_count as i64,
                report.flood_sleep_ms_total as i64,
                report.last_checkpoint_message_id,
                artifact_path.map(|path| path.to_string_lossy().to_string()),
                now_rfc3339(),
            ],
        )?;
        if updated != 1 {
            return Err(crate::error::AppError::Runtime(format!(
                "run record {run_id} was not updated on success"
            )));
        }
        Ok(())
    }

    pub fn finish_run_failure(
        &mut self,
        run_id: i64,
        requested_chat: Option<&str>,
        output_dir: Option<&Path>,
        error_message: &str,
        artifact_path: Option<&Path>,
    ) -> Result<()> {
        let updated = self.conn.execute(
            r#"
            UPDATE export_runs
            SET
                requested_chat = COALESCE(requested_chat, ?2),
                output_dir = COALESCE(output_dir, ?3),
                status = ?4,
                error_message = ?5,
                artifact_path = ?6,
                finished_at = ?7
            WHERE id = ?1
            "#,
            params![
                run_id,
                requested_chat,
                output_dir.map(|path| path.to_string_lossy().to_string()),
                RunStatus::Failed.as_str(),
                error_message,
                artifact_path.map(|path| path.to_string_lossy().to_string()),
                now_rfc3339(),
            ],
        )?;
        if updated != 1 {
            return Err(crate::error::AppError::Runtime(format!(
                "run record {run_id} was not updated on failure"
            )));
        }
        Ok(())
    }

    pub fn finish_run_interrupted(
        &mut self,
        run_id: i64,
        report: &ExportReport,
        artifact_path: Option<&Path>,
    ) -> Result<()> {
        let updated = self.conn.execute(
            r#"
            UPDATE export_runs
            SET
                chat_id = ?2,
                chat_title = ?3,
                output_dir = ?4,
                status = ?5,
                scanned_messages = ?6,
                media_found = ?7,
                downloaded = ?8,
                skipped_existing = ?9,
                failed = ?10,
                flood_wait_count = ?11,
                flood_sleep_ms_total = ?12,
                last_checkpoint_message_id = ?13,
                artifact_path = ?14,
                error_message = ?15,
                finished_at = ?16
            WHERE id = ?1
            "#,
            params![
                run_id,
                report.chat_id,
                report.chat_title,
                report.output_dir.to_string_lossy().to_string(),
                RunStatus::Interrupted.as_str(),
                report.scanned_messages as i64,
                report.media_found as i64,
                report.downloaded as i64,
                report.skipped_existing as i64,
                report.failed as i64,
                report.flood_wait_count as i64,
                report.flood_sleep_ms_total as i64,
                report.last_checkpoint_message_id,
                artifact_path.map(|path| path.to_string_lossy().to_string()),
                "export interrupted by signal; checkpoint saved",
                now_rfc3339(),
            ],
        )?;
        if updated != 1 {
            return Err(crate::error::AppError::Runtime(format!(
                "run record {run_id} was not updated on interruption"
            )));
        }
        Ok(())
    }

    pub fn list_runs(&self, limit: usize, failed_only: bool) -> Result<Vec<RunRecord>> {
        let mut sql = String::from(
            r#"
            SELECT
                id,
                operation,
                requested_chat,
                chat_id,
                chat_title,
                output_dir,
                status,
                scanned_messages,
                media_found,
                downloaded,
                skipped_existing,
                failed,
                flood_wait_count,
                flood_sleep_ms_total,
                last_checkpoint_message_id,
                error_message,
                artifact_path,
                started_at,
                finished_at
            FROM export_runs
            "#,
        );
        if failed_only {
            sql.push_str(" WHERE status IN ('failed', 'interrupted')");
        }
        sql.push_str(" ORDER BY started_at DESC LIMIT ?1");

        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params![limit as i64], run_record_from_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }
}

fn save_checkpoint_tx(tx: &Transaction<'_>, checkpoint: &CheckpointState) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO checkpoints (
            chat_id,
            high_watermark_message_id,
            backfill_cursor_message_id,
            backfill_complete,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(chat_id) DO UPDATE SET
            high_watermark_message_id = excluded.high_watermark_message_id,
            backfill_cursor_message_id = excluded.backfill_cursor_message_id,
            backfill_complete = excluded.backfill_complete,
            updated_at = excluded.updated_at
        "#,
        params![
            checkpoint.chat_id,
            checkpoint.high_watermark_message_id,
            checkpoint.backfill_cursor_message_id,
            if checkpoint.backfill_complete {
                1_i64
            } else {
                0_i64
            },
            now_rfc3339(),
        ],
    )?;
    Ok(())
}

fn upsert_media_tx(tx: &Transaction<'_>, record: &PersistedMediaItem) -> Result<()> {
    let now = now_rfc3339();
    tx.execute(
        r#"
        INSERT INTO media_items (
            chat_id,
            message_id,
            message_date,
            kind,
            telegram_media_key,
            mime_type,
            file_size_bytes,
            sha256,
            local_path,
            status,
            error_message,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
        ON CONFLICT(chat_id, message_id, telegram_media_key) DO UPDATE SET
            message_date = excluded.message_date,
            kind = excluded.kind,
            mime_type = excluded.mime_type,
            file_size_bytes = excluded.file_size_bytes,
            sha256 = excluded.sha256,
            local_path = excluded.local_path,
            status = excluded.status,
            error_message = excluded.error_message,
            updated_at = excluded.updated_at
        "#,
        params![
            record.chat_id,
            record.message_id,
            record.message_date.to_rfc3339(),
            record.kind.as_str(),
            record.telegram_media_key,
            record.mime_type,
            record.file_size_bytes,
            record.sha256,
            record.local_path.to_string_lossy().to_string(),
            record.status.as_str(),
            record.error_message,
            now,
        ],
    )?;
    Ok(())
}

fn stored_chat_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredChatRecord> {
    Ok(StoredChatRecord {
        chat_id: row.get(0)?,
        title: row.get(1)?,
        username: row.get(2)?,
        kind: row.get(3)?,
    })
}

fn export_plan_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExportPlanRecord> {
    Ok(ExportPlanRecord {
        id: row.get(0)?,
        chat_id: row.get(1)?,
        profile: row.get(2)?,
        output_dir: PathBuf::from(row.get::<_, String>(3)?),
        media_filter: row.get(4)?,
        scope_hash: row.get(5)?,
        planned_high_watermark_message_id: row.get(6)?,
        planned_backfill_cursor_message_id: row.get(7)?,
        planned_backfill_complete: row.get::<_, i64>(8)? != 0,
        scanned_messages: row.get::<_, i64>(9)? as usize,
        media_found: row.get::<_, i64>(10)? as usize,
        queued: row.get::<_, i64>(11)? as usize,
        estimated_bytes: row.get::<_, i64>(12)? as u64,
        per_kind_json: row.get(13)?,
    })
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatKind, MediaKind, MediaStatus};

    #[test]
    fn persists_checkpoint_roundtrip() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("state.db");
        let mut db = Database::open(&db_path).expect("db");

        let checkpoint = CheckpointState {
            chat_id: 1,
            high_watermark_message_id: Some(42),
            backfill_cursor_message_id: Some(9),
            backfill_complete: false,
        };
        db.save_checkpoint(&checkpoint).expect("save checkpoint");
        let loaded = db
            .load_checkpoint(1)
            .expect("load checkpoint")
            .expect("checkpoint");
        assert_eq!(loaded.high_watermark_message_id, Some(42));
        assert_eq!(loaded.backfill_cursor_message_id, Some(9));
        assert!(!loaded.backfill_complete);

        let chat = ChatSummary {
            id: 1,
            title: "Example".to_string(),
            username: Some("example".to_string()),
            kind: ChatKind::Channel,
            handle: (),
        };
        db.upsert_chat(&chat).expect("upsert chat");

        let media = PersistedMediaItem {
            chat_id: 1,
            message_id: 5,
            message_date: Utc::now(),
            kind: MediaKind::Photo,
            telegram_media_key: "photo:1".to_string(),
            mime_type: Some("image/jpeg".to_string()),
            file_size_bytes: Some(123),
            local_path: PathBuf::from("/tmp/photo.jpg"),
            sha256: Some("deadbeef".to_string()),
            status: MediaStatus::Downloaded,
            error_message: None,
        };
        db.commit_message(&[media], Some(&checkpoint))
            .expect("commit message");
        let loaded_media = db
            .find_media_by_key(1, 5, "photo:1")
            .expect("find media")
            .expect("media");
        assert_eq!(loaded_media.status, MediaStatus::Downloaded);
        assert_eq!(loaded_media.sha256.as_deref(), Some("deadbeef"));

        let schema_version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(schema_version, 3);
    }

    #[test]
    fn keeps_checkpoints_separate_per_chat() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("state.db");
        let mut db = Database::open(&db_path).expect("db");

        db.save_checkpoint(&CheckpointState {
            chat_id: 1,
            high_watermark_message_id: Some(10),
            backfill_cursor_message_id: Some(5),
            backfill_complete: false,
        })
        .expect("save checkpoint 1");

        db.save_checkpoint(&CheckpointState {
            chat_id: 2,
            high_watermark_message_id: Some(20),
            backfill_cursor_message_id: Some(8),
            backfill_complete: true,
        })
        .expect("save checkpoint 2");

        let first = db
            .load_checkpoint(1)
            .expect("load first")
            .expect("first checkpoint");
        let second = db
            .load_checkpoint(2)
            .expect("load second")
            .expect("second checkpoint");

        assert_eq!(first.high_watermark_message_id, Some(10));
        assert_eq!(second.high_watermark_message_id, Some(20));
        assert!(!first.backfill_complete);
        assert!(second.backfill_complete);
    }

    #[test]
    fn resets_chat_state_without_touching_other_chat() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("state.db");
        let mut db = Database::open(&db_path).expect("db");

        db.save_checkpoint(&CheckpointState {
            chat_id: 1,
            high_watermark_message_id: Some(10),
            backfill_cursor_message_id: Some(3),
            backfill_complete: true,
        })
        .expect("checkpoint one");
        db.save_checkpoint(&CheckpointState {
            chat_id: 2,
            high_watermark_message_id: Some(20),
            backfill_cursor_message_id: Some(5),
            backfill_complete: false,
        })
        .expect("checkpoint two");

        db.commit_message(
            &[PersistedMediaItem {
                chat_id: 1,
                message_id: 5,
                message_date: Utc::now(),
                kind: MediaKind::Photo,
                telegram_media_key: "photo:1".to_string(),
                mime_type: Some("image/jpeg".to_string()),
                file_size_bytes: Some(10),
                local_path: PathBuf::from("/tmp/chat1.jpg"),
                sha256: None,
                status: MediaStatus::Failed,
                error_message: Some("boom".to_string()),
            }],
            None,
        )
        .expect("media one");
        db.commit_message(
            &[PersistedMediaItem {
                chat_id: 2,
                message_id: 7,
                message_date: Utc::now(),
                kind: MediaKind::Photo,
                telegram_media_key: "photo:2".to_string(),
                mime_type: Some("image/jpeg".to_string()),
                file_size_bytes: Some(10),
                local_path: PathBuf::from("/tmp/chat2.jpg"),
                sha256: None,
                status: MediaStatus::Downloaded,
                error_message: None,
            }],
            None,
        )
        .expect("media two");

        db.reset_chat_state(1).expect("reset");

        assert!(db.load_checkpoint(1).expect("load one").is_none());
        assert!(db.list_media_for_chat(1).expect("list one").is_empty());
        assert!(db.load_checkpoint(2).expect("load two").is_some());
        assert_eq!(db.list_media_for_chat(2).expect("list two").len(), 1);
    }

    #[test]
    fn records_and_lists_runs() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("state.db");
        let mut db = Database::open(&db_path).expect("db");
        let run_id = db
            .start_run("export", Some("@demo"), Some(tempdir.path()))
            .expect("start run");
        let report = ExportReport {
            chat_id: 1,
            chat_title: "Demo".to_string(),
            scanned_messages: 10,
            media_found: 4,
            downloaded: 3,
            skipped_existing: 1,
            failed: 0,
            last_checkpoint_message_id: Some(10),
            output_dir: tempdir.path().join("downloads"),
            duration_ms: 100,
            per_kind: Default::default(),
            flood_wait_count: 0,
            flood_sleep_ms_total: 0,
        };
        db.finish_run_success(run_id, &report, None)
            .expect("finish run");

        let runs = db.list_runs(5, false).expect("list runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Succeeded);
        assert_eq!(runs[0].downloaded, 3);
    }

    #[test]
    fn migrates_v1_database_to_latest() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let db_path = tempdir.path().join("state.db");
        let conn = Connection::open(&db_path).expect("conn");
        conn.execute_batch(
            r#"
            PRAGMA user_version = 1;
            CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );
            CREATE TABLE chats (
                chat_id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                username TEXT,
                kind TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE checkpoints (
                chat_id INTEGER PRIMARY KEY,
                high_watermark_message_id INTEGER,
                backfill_cursor_message_id INTEGER,
                backfill_complete INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE media_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                chat_id INTEGER NOT NULL,
                message_id INTEGER NOT NULL,
                message_date TEXT NOT NULL,
                kind TEXT NOT NULL,
                telegram_media_key TEXT NOT NULL,
                mime_type TEXT,
                file_size_bytes INTEGER,
                sha256 TEXT,
                local_path TEXT NOT NULL,
                status TEXT NOT NULL,
                error_message TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(chat_id, message_id, telegram_media_key),
                UNIQUE(local_path)
            );
            "#,
        )
        .expect("seed v1 schema");
        drop(conn);

        let db = Database::open(&db_path).expect("migrated db");
        let runs = db.list_runs(10, false).expect("list runs");
        assert!(runs.is_empty());
        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, 3);
    }
}
