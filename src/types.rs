use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Photo,
    ImageDocument,
    Video,
    Animation,
    Audio,
    Voice,
    Document,
}

impl MediaKind {
    pub const ALL: [Self; 7] = [
        Self::Photo,
        Self::ImageDocument,
        Self::Video,
        Self::Animation,
        Self::Audio,
        Self::Voice,
        Self::Document,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::ImageDocument => "image_doc",
            Self::Video => "video",
            Self::Animation => "animation",
            Self::Audio => "audio",
            Self::Voice => "voice",
            Self::Document => "document",
        }
    }

    pub fn bucket_dir(self) -> &'static str {
        match self {
            Self::Photo | Self::ImageDocument => "photos",
            Self::Video => "videos",
            Self::Animation => "animations",
            Self::Audio | Self::Voice => "audio",
            Self::Document => "files",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "photo" => Some(Self::Photo),
            "image_doc" | "image-document" | "image_document" => Some(Self::ImageDocument),
            "video" => Some(Self::Video),
            "animation" | "gif" => Some(Self::Animation),
            "audio" => Some(Self::Audio),
            "voice" | "voice_note" => Some(Self::Voice),
            "document" | "file" => Some(Self::Document),
            _ => None,
        }
    }

    pub fn parse_csv(value: &str) -> Result<BTreeSet<Self>, String> {
        let mut items = BTreeSet::new();
        for raw in value.split(',') {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let kind = Self::parse(trimmed)
                .ok_or_else(|| format!("unsupported media kind `{trimmed}`"))?;
            items.insert(kind);
        }

        if items.is_empty() {
            return Err("media filter cannot be empty".to_string());
        }

        Ok(items)
    }
}

impl Display for MediaKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatKind {
    User,
    Group,
    Supergroup,
    Channel,
}

impl Display for ChatKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Group => f.write_str("group"),
            Self::Supergroup => f.write_str("supergroup"),
            Self::Channel => f.write_str("channel"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatSummary<H> {
    pub id: i64,
    pub title: String,
    pub username: Option<String>,
    pub kind: ChatKind,
    pub handle: H,
}

#[derive(Debug, Clone)]
pub struct MediaDescriptor<H> {
    pub kind: MediaKind,
    pub telegram_media_key: String,
    pub mime_type: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub original_name: Option<String>,
    pub handle: H,
}

#[derive(Debug, Clone)]
pub struct ScannedMessage<H> {
    pub message_id: i32,
    pub date: DateTime<Utc>,
    pub media: Vec<MediaDescriptor<H>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilenameMode {
    Stable,
    OriginalIfAvailable,
}

impl FilenameMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" => Some(Self::Stable),
            "original_if_available" => Some(Self::OriginalIfAvailable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub chat: String,
    pub out_dir: PathBuf,
    pub resume: bool,
    pub verbose_progress: bool,
    pub media_filter: BTreeSet<MediaKind>,
    pub since_id: Option<i32>,
    pub until_id: Option<i32>,
    pub date_from: Option<NaiveDate>,
    pub date_to: Option<NaiveDate>,
    pub limit: Option<usize>,
    pub rescan: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaStatus {
    Pending,
    Downloading,
    Downloaded,
    Failed,
    SkippedExisting,
}

impl MediaStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Downloading => "downloading",
            Self::Downloaded => "downloaded",
            Self::Failed => "failed",
            Self::SkippedExisting => "skipped_existing",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "downloading" => Some(Self::Downloading),
            "downloaded" => Some(Self::Downloaded),
            "failed" => Some(Self::Failed),
            "skipped_existing" => Some(Self::SkippedExisting),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PacingStats {
    pub flood_wait_count: u64,
    pub flood_sleep_ms_total: u64,
    pub cooldown_active: bool,
}

#[derive(Debug, Clone)]
pub struct CheckpointState {
    pub chat_id: i64,
    pub high_watermark_message_id: Option<i32>,
    pub backfill_cursor_message_id: Option<i32>,
    pub backfill_complete: bool,
}
