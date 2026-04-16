use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rusqlite::types::Type;

use crate::types::{MediaKind, MediaStatus};

use super::{RunRecord, RunStatus, StoredMediaRecord};

pub(super) fn stored_media_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredMediaRecord> {
    let message_date_raw: String = row.get(2)?;
    let message_date = DateTime::parse_from_rfc3339(&message_date_raw)
        .map(|date| date.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
        })?;
    let kind_raw: String = row.get(3)?;
    let kind = MediaKind::parse(&kind_raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            Type::Text,
            format!("unsupported media kind `{kind_raw}`").into(),
        )
    })?;
    let status: String = row.get(5)?;
    Ok(StoredMediaRecord {
        chat_id: row.get(0)?,
        message_id: row.get(1)?,
        message_date,
        kind,
        telegram_media_key: row.get(4)?,
        status: MediaStatus::parse(&status).unwrap_or(MediaStatus::Failed),
        local_path: PathBuf::from(row.get::<_, String>(6)?),
        sha256: row.get(7)?,
        error_message: row.get(8)?,
        file_size_bytes: row.get(9)?,
    })
}

pub(super) fn run_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    let status: String = row.get(6)?;
    Ok(RunRecord {
        id: row.get(0)?,
        operation: row.get(1)?,
        requested_chat: row.get(2)?,
        chat_id: row.get(3)?,
        chat_title: row.get(4)?,
        output_dir: row.get::<_, Option<String>>(5)?.map(PathBuf::from),
        status: RunStatus::parse(&status),
        scanned_messages: row.get::<_, i64>(7)? as usize,
        media_found: row.get::<_, i64>(8)? as usize,
        downloaded: row.get::<_, i64>(9)? as usize,
        skipped_existing: row.get::<_, i64>(10)? as usize,
        failed: row.get::<_, i64>(11)? as usize,
        flood_wait_count: row.get::<_, i64>(12)? as u64,
        flood_sleep_ms_total: row.get::<_, i64>(13)? as u64,
        last_checkpoint_message_id: row.get(14)?,
        error_message: row.get(15)?,
        artifact_path: row.get::<_, Option<String>>(16)?.map(PathBuf::from),
        started_at: row.get(17)?,
        finished_at: row.get(18)?,
    })
}
