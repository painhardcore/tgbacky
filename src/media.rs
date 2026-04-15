use std::path::Path;

use grammers_client::media::{Document, Photo};
use grammers_client::tl;
use sha2::{Digest, Sha256};

use crate::types::{FilenameMode, MediaKind};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DocumentHints {
    pub has_video_attr: bool,
    pub has_audio_attr: bool,
    pub voice: bool,
    pub animated: bool,
    pub sticker: bool,
}

pub fn classify_photo(_: &Photo) -> MediaKind {
    MediaKind::Photo
}

pub fn classify_document(document: &Document) -> Option<MediaKind> {
    let mime_type = document.mime_type();
    let mut hints = DocumentHints::default();

    if let Some(tl::enums::Document::Document(raw_document)) = &document.raw.document {
        for attribute in &raw_document.attributes {
            match attribute {
                tl::enums::DocumentAttribute::Audio(audio) => {
                    hints.has_audio_attr = true;
                    hints.voice = audio.voice;
                }
                tl::enums::DocumentAttribute::Video(_) => hints.has_video_attr = true,
                tl::enums::DocumentAttribute::Animated => hints.animated = true,
                tl::enums::DocumentAttribute::Sticker(_) => hints.sticker = true,
                _ => {}
            }
        }
    }

    classify_document_hints(mime_type, hints)
}

pub fn classify_document_hints(mime_type: Option<&str>, hints: DocumentHints) -> Option<MediaKind> {
    if hints.sticker {
        return None;
    }

    if let Some(mime) = mime_type
        && mime.starts_with("image/")
    {
        return Some(MediaKind::ImageDocument);
    }

    if hints.voice {
        return Some(MediaKind::Voice);
    }

    if hints.has_audio_attr {
        return Some(MediaKind::Audio);
    }

    if hints.animated || mime_type == Some("image/gif") {
        return Some(MediaKind::Animation);
    }

    if hints.has_video_attr {
        return Some(MediaKind::Video);
    }

    Some(MediaKind::Document)
}

pub fn stable_media_key_for_photo(photo: &Photo) -> String {
    format!("photo:{}", photo.id())
}

pub fn stable_media_key_for_document(document: &Document) -> String {
    format!("document:{}", document.id())
}

pub fn stable_suffix(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    hex::encode(&digest[..8])
}

pub fn extension_from_name(name: Option<&str>) -> Option<String> {
    let ext = Path::new(name?).extension()?.to_str()?.to_ascii_lowercase();
    if ext.is_empty() {
        return None;
    }
    let sanitized: String = ext
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect();
    if sanitized.is_empty() {
        return None;
    }
    Some(normalize_common_extension(&sanitized))
}

pub fn extension_from_mime(mime_type: Option<&str>) -> Option<String> {
    let mime = mime_type?;
    mime_guess::get_mime_extensions_str(mime)
        .and_then(|values| values.first().copied())
        .map(normalize_common_extension)
}

pub fn default_extension(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Photo | MediaKind::ImageDocument => "jpg",
        MediaKind::Video => "mp4",
        MediaKind::Animation => "gif",
        MediaKind::Audio => "mp3",
        MediaKind::Voice => "ogg",
        MediaKind::Document => "bin",
    }
}

pub fn choose_extension(
    original_name: Option<&str>,
    mime_type: Option<&str>,
    kind: MediaKind,
) -> String {
    if kind == MediaKind::Photo {
        return "jpg".to_string();
    }

    extension_from_name(original_name)
        .or_else(|| extension_from_mime(mime_type))
        .unwrap_or_else(|| default_extension(kind).to_string())
}

pub fn normalize_common_extension(extension: &str) -> String {
    match extension.trim().to_ascii_lowercase().as_str() {
        "jpeg" | "jfif" => "jpg".to_string(),
        other => other.to_string(),
    }
}

pub fn build_filename(
    message_id: i32,
    date: chrono::NaiveDate,
    kind: MediaKind,
    original_name: Option<&str>,
    stable_key: &str,
    extension: &str,
    filename_mode: FilenameMode,
) -> String {
    let suffix = stable_suffix(stable_key);
    match filename_mode {
        FilenameMode::Stable => {
            format!(
                "{message_id}_{}_{}_{}.{}",
                date,
                kind.as_str(),
                suffix,
                extension
            )
        }
        FilenameMode::OriginalIfAvailable => {
            if let Some(name) = original_name {
                let stem = sanitize_file_stem(name);
                if !stem.is_empty() {
                    return format!(
                        "{message_id}_{}_{}_{}_{}.{}",
                        date,
                        kind.as_str(),
                        stem,
                        suffix,
                        extension
                    );
                }
            }
            format!(
                "{message_id}_{}_{}_{}.{}",
                date,
                kind.as_str(),
                suffix,
                extension
            )
        }
    }
}

pub fn sanitize_file_stem(value: &str) -> String {
    let stem = Path::new(value)
        .file_stem()
        .and_then(|item| item.to_str())
        .unwrap_or_default();
    let mut output = String::with_capacity(stem.len());
    let mut prev_sep = false;
    for ch in stem.chars() {
        let replacement = if ch.is_ascii_alphanumeric() {
            prev_sep = false;
            Some(ch.to_ascii_lowercase())
        } else if !prev_sep {
            prev_sep = true;
            Some('_')
        } else {
            None
        };
        if let Some(ch) = replacement {
            output.push(ch);
        }
    }
    output.trim_matches('_').chars().take(40).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_documents() {
        assert_eq!(
            classify_document_hints(
                Some("image/png"),
                DocumentHints {
                    ..DocumentHints::default()
                },
            ),
            Some(MediaKind::ImageDocument)
        );
        assert_eq!(
            classify_document_hints(
                Some("application/octet-stream"),
                DocumentHints {
                    voice: true,
                    has_audio_attr: true,
                    ..DocumentHints::default()
                },
            ),
            Some(MediaKind::Voice)
        );
        assert_eq!(
            classify_document_hints(
                Some("video/mp4"),
                DocumentHints {
                    has_video_attr: true,
                    ..DocumentHints::default()
                },
            ),
            Some(MediaKind::Video)
        );
    }

    #[test]
    fn builds_stable_filename() {
        let file = build_filename(
            42,
            chrono::NaiveDate::from_ymd_opt(2026, 1, 2).expect("date"),
            MediaKind::Photo,
            Some("hello world.png"),
            "document:abc",
            "png",
            FilenameMode::OriginalIfAvailable,
        );
        assert!(file.contains("hello_world"));
        assert!(file.ends_with(".png"));
    }

    #[test]
    fn chooses_original_document_extension_first() {
        assert_eq!(
            choose_extension(
                Some("archive.weirdzip"),
                Some("application/zip"),
                MediaKind::Document,
            ),
            "weirdzip"
        );
    }

    #[test]
    fn stable_suffix_is_longer() {
        assert_eq!(stable_suffix("document:abc").len(), 16);
    }

    #[test]
    fn normalizes_jpegish_extensions() {
        assert_eq!(normalize_common_extension("jpeg"), "jpg");
        assert_eq!(normalize_common_extension("jfif"), "jpg");
        assert_eq!(normalize_common_extension("png"), "png");
    }
}
