use rusqlite::Transaction;

use super::Migration;

pub(super) fn migrations() -> &'static [Migration] {
    &[
        Migration {
            version: 1,
            apply: apply_migration_v1,
        },
        Migration {
            version: 2,
            apply: apply_migration_v2,
        },
        Migration {
            version: 3,
            apply: apply_migration_v3,
        },
    ]
}

fn apply_migration_v3(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS export_plans (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            chat_id INTEGER NOT NULL,
            profile TEXT NOT NULL,
            output_dir TEXT NOT NULL,
            media_filter TEXT NOT NULL,
            scope_hash TEXT NOT NULL,
            status TEXT NOT NULL,
            planned_high_watermark_message_id INTEGER,
            planned_backfill_cursor_message_id INTEGER,
            planned_backfill_complete INTEGER NOT NULL DEFAULT 0,
            scanned_messages INTEGER NOT NULL DEFAULT 0,
            media_found INTEGER NOT NULL DEFAULT 0,
            queued INTEGER NOT NULL DEFAULT 0,
            estimated_bytes INTEGER NOT NULL DEFAULT 0,
            per_kind_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            completed_at TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_export_plans_lookup
            ON export_plans(chat_id, profile, output_dir, media_filter, scope_hash, status, id DESC);
        CREATE INDEX IF NOT EXISTS idx_export_plans_status
            ON export_plans(status, updated_at DESC);
        "#,
    )
}

fn apply_migration_v1(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS chats (
            chat_id INTEGER PRIMARY KEY,
            title TEXT NOT NULL,
            username TEXT,
            kind TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS checkpoints (
            chat_id INTEGER PRIMARY KEY,
            high_watermark_message_id INTEGER,
            backfill_cursor_message_id INTEGER,
            backfill_complete INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS media_items (
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

        CREATE INDEX IF NOT EXISTS idx_media_chat_message
            ON media_items(chat_id, message_id);
        CREATE INDEX IF NOT EXISTS idx_media_sha256
            ON media_items(sha256);
        CREATE INDEX IF NOT EXISTS idx_media_status
            ON media_items(status);
        "#,
    )
}

fn apply_migration_v2(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS export_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            operation TEXT NOT NULL,
            requested_chat TEXT,
            chat_id INTEGER,
            chat_title TEXT,
            output_dir TEXT,
            status TEXT NOT NULL,
            scanned_messages INTEGER NOT NULL DEFAULT 0,
            media_found INTEGER NOT NULL DEFAULT 0,
            downloaded INTEGER NOT NULL DEFAULT 0,
            skipped_existing INTEGER NOT NULL DEFAULT 0,
            failed INTEGER NOT NULL DEFAULT 0,
            flood_wait_count INTEGER NOT NULL DEFAULT 0,
            flood_sleep_ms_total INTEGER NOT NULL DEFAULT 0,
            last_checkpoint_message_id INTEGER,
            error_message TEXT,
            artifact_path TEXT,
            started_at TEXT NOT NULL,
            finished_at TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_export_runs_started_at
            ON export_runs(started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_export_runs_status
            ON export_runs(status, started_at DESC);
        "#,
    )
}
