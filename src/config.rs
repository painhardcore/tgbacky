use std::collections::BTreeSet;
use std::env;
use std::num::ParseIntError;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use tracing_subscriber::filter::LevelFilter;

use crate::error::{AppError, Result};
use crate::secrets::{
    CredentialStorageStatus, TelegramCredentials, load_telegram_credentials_with_status,
    resolve_api_profile,
};
use crate::types::{FilenameMode, MediaKind};

const CURRENT_PROFILE_FILE: &str = "current-profile";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadConcurrencyOrigin {
    Auto,
    Cli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    Explicit,
    Env,
    Current,
    Default,
}

impl ProfileSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Env => "env",
            Self::Current => "current",
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    Flags,
    Env,
    Stored(CredentialStorageStatus),
    Runtime,
    None,
}

impl CredentialSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Flags => "flags",
            Self::Env => "env",
            Self::Stored(status) => status.label(),
            Self::Runtime => "runtime",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConfigOverrides {
    pub profile: Option<String>,
    pub api_profile: Option<String>,
    pub api_id: Option<i32>,
    pub api_hash: Option<String>,
    pub session_path: Option<PathBuf>,
    pub db_path: Option<PathBuf>,
    pub download_dir: Option<PathBuf>,
    pub run_artifact_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub profile: String,
    pub api_profile: String,
    pub profile_source: ProfileSource,
    pub credential_source: CredentialSource,
    pub api_id: Option<i32>,
    pub api_hash: Option<String>,
    pub session_path: PathBuf,
    pub db_path: PathBuf,
    pub download_dir: PathBuf,
    pub run_artifact_dir: PathBuf,
    pub log_level: LevelFilter,
    pub retry_count: u32,
    pub retry_backoff_ms: u64,
    pub download_stall_timeout_secs: u64,
    pub media_filter: BTreeSet<MediaKind>,
    pub filename_mode: FilenameMode,
    pub temp_extension: String,
    pub request_delay_ms: u64,
    pub download_delay_ms: u64,
    pub flood_sleep_threshold_secs: u64,
    pub jitter_ms: u64,
    pub download_concurrency: usize,
    pub download_concurrency_origin: DownloadConcurrencyOrigin,
    pub cleanup_stale_parts_on_start: bool,
    pub stale_part_min_age_hours: u64,
    pub verbose_dependency_logs: bool,
}

impl AppConfig {
    pub fn load(overrides: ConfigOverrides) -> Result<Self> {
        let _ = dotenvy::dotenv();

        let (profile, profile_source) = resolve_profile(overrides.profile)?;
        let api_profile = resolve_api_profile(overrides.api_profile)?;
        let profile_layout = ProfileLayout::new(&profile)?;
        let media_filter = if let Some(raw) = get_optional("tgbacky_MEDIA_FILTER")? {
            parse_media_filter(&raw)?
        } else {
            let mut defaults = BTreeSet::from(MediaKind::ALL);
            if let Some(raw) = get_optional("tgbacky_SAVE_IMAGE_DOCUMENTS")?
                && !parse_bool("tgbacky_SAVE_IMAGE_DOCUMENTS", &raw)?
            {
                defaults.remove(&MediaKind::ImageDocument);
            }
            defaults
        };

        let flag_credentials = if let (Some(api_id), Some(api_hash)) =
            (overrides.api_id, overrides.api_hash.clone())
        {
            Some(TelegramCredentials { api_id, api_hash })
        } else {
            None
        };
        let env_credentials = load_credentials_from_env()?;
        let (resolved_credentials, credential_source) = if let Some(credentials) = flag_credentials
        {
            (Some(credentials), CredentialSource::Flags)
        } else if let Some(credentials) = env_credentials {
            (Some(credentials), CredentialSource::Env)
        } else {
            let (stored_credentials, stored_status) =
                load_telegram_credentials_with_status(&api_profile)?;
            match stored_credentials {
                Some(credentials) => (Some(credentials), CredentialSource::Stored(stored_status)),
                None => (None, CredentialSource::None),
            }
        };

        let (api_id, api_hash) = if let Some(credentials) = resolved_credentials {
            (Some(credentials.api_id), Some(credentials.api_hash))
        } else {
            (None, None)
        };

        let config = Self {
            profile,
            api_profile,
            profile_source,
            credential_source,
            api_id,
            api_hash,
            session_path: resolve_path_override(
                overrides.session_path,
                "tgbacky_SESSION_PATH",
                &profile_layout.session_path,
            )?,
            db_path: resolve_path_override(
                overrides.db_path,
                "tgbacky_DB_PATH",
                &profile_layout.db_path,
            )?,
            download_dir: resolve_path_override(
                overrides.download_dir,
                "tgbacky_DOWNLOAD_DIR",
                &profile_layout.download_dir,
            )?,
            run_artifact_dir: resolve_path_override(
                overrides.run_artifact_dir,
                "tgbacky_RUN_ARTIFACT_DIR",
                &profile_layout.run_artifact_dir,
            )?,
            log_level: parse_log_level(
                get_optional("tgbacky_LOG_LEVEL")?
                    .as_deref()
                    .unwrap_or("error"),
            )?,
            retry_count: parse_optional("tgbacky_RETRY_COUNT")?.unwrap_or(3),
            retry_backoff_ms: parse_optional("tgbacky_RETRY_BACKOFF_MS")?.unwrap_or(1_000),
            download_stall_timeout_secs: parse_optional("tgbacky_DOWNLOAD_STALL_TIMEOUT_SECS")?
                .unwrap_or(120),
            media_filter,
            filename_mode: FilenameMode::parse(
                get_optional("tgbacky_FILENAME_MODE")?
                    .as_deref()
                    .unwrap_or("stable"),
            )
            .ok_or_else(|| {
                AppError::Config(
                    "tgbacky_FILENAME_MODE must be `stable` or `original_if_available`".to_string(),
                )
            })?,
            temp_extension: normalize_temp_extension(
                get_optional("tgbacky_TEMP_EXTENSION")?.unwrap_or_else(|| ".part".to_string()),
            ),
            request_delay_ms: parse_optional("tgbacky_REQUEST_DELAY_MS")?.unwrap_or(400),
            download_delay_ms: parse_optional("tgbacky_DOWNLOAD_DELAY_MS")?.unwrap_or(850),
            flood_sleep_threshold_secs: parse_optional("tgbacky_FLOOD_SLEEP_THRESHOLD_SECS")?
                .unwrap_or(0),
            jitter_ms: parse_optional("tgbacky_JITTER_MS")?.unwrap_or(250),
            download_concurrency: default_download_concurrency(),
            download_concurrency_origin: DownloadConcurrencyOrigin::Auto,
            cleanup_stale_parts_on_start: parse_optional_bool(
                "tgbacky_CLEANUP_STALE_PARTS_ON_START",
            )?
            .unwrap_or(false),
            stale_part_min_age_hours: parse_optional("tgbacky_STALE_PART_MIN_AGE_HOURS")?
                .unwrap_or(12),
            verbose_dependency_logs: parse_optional_bool("tgbacky_VERBOSE_DEPENDENCY_LOGS")?
                .unwrap_or(false),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn telegram_credentials(&self) -> Result<TelegramCredentials> {
        let api_id = self.api_id.ok_or_else(|| {
            AppError::Authentication(format!(
                "Telegram API credentials are not configured for API profile `{}`; run `tgbacky auth` or `tgbacky api add`",
                self.api_profile
            ))
        })?;
        let api_hash = self.api_hash.clone().ok_or_else(|| {
            AppError::Authentication(format!(
                "Telegram API credentials are not configured for API profile `{}`; run `tgbacky auth` or `tgbacky api add`",
                self.api_profile
            ))
        })?;
        Ok(TelegramCredentials { api_id, api_hash })
    }

    pub fn with_telegram_credentials(&self, credentials: TelegramCredentials) -> Result<Self> {
        let mut config = self.clone();
        config.api_id = Some(credentials.api_id);
        config.api_hash = Some(credentials.api_hash);
        config.credential_source = CredentialSource::Runtime;
        config.validate()?;
        Ok(config)
    }

    pub fn with_session_path(&self, session_path: PathBuf) -> Result<Self> {
        let mut config = self.clone();
        config.session_path = session_path;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let session_path = normalize_path_for_validation(&self.session_path)?;
        let db_path = normalize_path_for_validation(&self.db_path)?;
        let download_dir = normalize_path_for_validation(&self.download_dir)?;
        let artifact_dir = normalize_path_for_validation(&self.run_artifact_dir)?;

        if let Some(api_id) = self.api_id
            && api_id <= 0
        {
            return Err(AppError::Config(
                "tgbacky_API_ID must be a positive integer".to_string(),
            ));
        }
        if let Some(api_hash) = self.api_hash.as_deref()
            && api_hash.trim().is_empty()
        {
            return Err(AppError::Config(
                "tgbacky_API_HASH cannot be empty".to_string(),
            ));
        }
        if session_path == db_path {
            return Err(AppError::Config(
                "tgbacky_SESSION_PATH and tgbacky_DB_PATH must be different files".to_string(),
            ));
        }
        if path_contains(&download_dir, &session_path)
            || path_contains(&download_dir, &db_path)
            || path_contains(&artifact_dir, &session_path)
            || path_contains(&artifact_dir, &db_path)
        {
            return Err(AppError::Config(
                "download/artifact directories cannot overlap the session or state database files"
                    .to_string(),
            ));
        }
        if path_contains(&download_dir, &artifact_dir)
            || path_contains(&artifact_dir, &download_dir)
        {
            return Err(AppError::Config(
                "tgbacky_DOWNLOAD_DIR and tgbacky_RUN_ARTIFACT_DIR must not overlap".to_string(),
            ));
        }
        if self.temp_extension.contains(std::path::MAIN_SEPARATOR) {
            return Err(AppError::Config(
                "tgbacky_TEMP_EXTENSION cannot contain path separators".to_string(),
            ));
        }
        if self.request_delay_ms == 0 || self.download_delay_ms == 0 {
            return Err(AppError::Config(
                "request and download delays must be greater than zero".to_string(),
            ));
        }
        if self.download_concurrency == 0 {
            return Err(AppError::Config(
                "download worker count must be greater than zero".to_string(),
            ));
        }
        if self.retry_backoff_ms == 0 && self.retry_count > 0 {
            return Err(AppError::Config(
                "tgbacky_RETRY_BACKOFF_MS must be greater than zero when retries are enabled"
                    .to_string(),
            ));
        }
        if self.download_stall_timeout_secs == 0 {
            return Err(AppError::Config(
                "tgbacky_DOWNLOAD_STALL_TIMEOUT_SECS must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

pub fn default_download_concurrency() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4)
}

pub fn auto_scan_ahead_messages(download_concurrency: usize) -> usize {
    download_concurrency.saturating_mul(32).max(64)
}

fn resolve_profile(profile_override: Option<String>) -> Result<(String, ProfileSource)> {
    resolve_profile_from_values(
        profile_override,
        get_optional("tgbacky_PROFILE")?,
        load_current_profile()?,
    )
}

fn resolve_profile_from_values(
    profile_override: Option<String>,
    env_profile: Option<String>,
    current_profile: Option<String>,
) -> Result<(String, ProfileSource)> {
    let (profile, source) = if let Some(profile) = profile_override {
        (profile, ProfileSource::Explicit)
    } else if let Some(profile) = env_profile {
        (profile, ProfileSource::Env)
    } else if let Some(profile) = current_profile {
        (profile, ProfileSource::Current)
    } else {
        ("default".to_string(), ProfileSource::Default)
    };
    let profile = sanitize_profile_name(&profile);
    if profile.is_empty() {
        return Err(AppError::Config("profile name cannot be empty".to_string()));
    }
    Ok((profile, source))
}

fn sanitize_profile_name(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut prev_sep = false;
    for ch in raw.trim().chars() {
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
    output.trim_matches('_').to_string()
}

#[derive(Debug, Clone)]
pub struct ProfileLayout {
    pub root_dir: PathBuf,
    pub session_path: PathBuf,
    pub db_path: PathBuf,
    pub download_dir: PathBuf,
    pub run_artifact_dir: PathBuf,
}

impl ProfileLayout {
    fn new(profile: &str) -> Result<Self> {
        let profile_root = project_data_dir()?.join("profiles").join(profile);
        Ok(Self {
            root_dir: profile_root.clone(),
            session_path: profile_root.join("session.db"),
            db_path: profile_root.join("state.db"),
            download_dir: profile_root.join("downloads"),
            run_artifact_dir: profile_root.join("run-artifacts"),
        })
    }
}

pub fn sanitize_profile(raw: &str) -> Result<String> {
    let profile = sanitize_profile_name(raw);
    if profile.is_empty() {
        return Err(AppError::InvalidArgument(
            "profile name cannot be empty".to_string(),
        ));
    }
    Ok(profile)
}

pub fn profile_layout(profile: &str) -> Result<ProfileLayout> {
    ProfileLayout::new(&sanitize_profile(profile)?)
}

pub fn profiles_root() -> Result<PathBuf> {
    Ok(project_data_dir()?.join("profiles"))
}

pub fn current_profile() -> Result<Option<String>> {
    load_current_profile()
}

pub fn set_current_profile(profile: &str) -> Result<()> {
    let profile = sanitize_profile(profile)?;
    let path = current_profile_path()?;
    let parent = path.parent().ok_or_else(|| {
        AppError::Filesystem(std::io::Error::other(format!(
            "current profile path {} has no parent directory",
            path.display()
        )))
    })?;
    std::fs::create_dir_all(parent)?;
    std::fs::write(path, format!("{profile}\n"))?;
    Ok(())
}

pub fn clear_current_profile_if_matches(profile: &str) -> Result<()> {
    let profile = sanitize_profile(profile)?;
    if load_current_profile()?.as_deref() == Some(profile.as_str()) {
        let path = current_profile_path()?;
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn load_current_profile() -> Result<Option<String>> {
    let path = current_profile_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let profile = sanitize_profile_name(raw.trim());
    if profile.is_empty() {
        return Ok(None);
    }
    Ok(Some(profile))
}

fn current_profile_path() -> Result<PathBuf> {
    Ok(project_data_dir()?.join(CURRENT_PROFILE_FILE))
}

fn project_data_dir() -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("com", "tgbacky", "tgbacky").ok_or_else(|| {
        AppError::Config("could not determine a default application data directory".to_string())
    })?;
    Ok(project_dirs.data_local_dir().to_path_buf())
}

fn resolve_path_override(
    flag_value: Option<PathBuf>,
    env_name: &str,
    default: &Path,
) -> Result<PathBuf> {
    if let Some(path) = flag_value {
        return Ok(path);
    }
    if let Some(path) = parse_path(env_name)? {
        return Ok(path);
    }
    Ok(default.to_path_buf())
}

fn normalize_path_for_validation(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
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

fn path_contains(container: &Path, candidate: &Path) -> bool {
    candidate == container || candidate.starts_with(container)
}

fn normalize_temp_extension(value: String) -> String {
    if value.starts_with('.') {
        value
    } else {
        format!(".{value}")
    }
}

fn get_optional(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(AppError::Config(format!(
            "env var {name} contains invalid unicode"
        ))),
    }
}

fn parse_optional<T>(name: &str) -> Result<Option<T>>
where
    T: std::str::FromStr<Err = ParseIntError>,
{
    get_optional(name)?
        .map(|value| {
            value
                .parse::<T>()
                .map_err(|_| AppError::Config(format!("env var {name} is invalid")))
        })
        .transpose()
}

fn parse_path(name: &str) -> Result<Option<PathBuf>> {
    Ok(get_optional(name)?.map(PathBuf::from))
}

fn parse_optional_bool(name: &str) -> Result<Option<bool>> {
    get_optional(name)?
        .map(|value| parse_bool(name, &value))
        .transpose()
}

fn parse_bool(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(AppError::Config(format!(
            "env var {name} must be `true` or `false`"
        ))),
    }
}

fn parse_log_level(value: &str) -> Result<LevelFilter> {
    match value.trim().to_ascii_lowercase().as_str() {
        "error" => Ok(LevelFilter::ERROR),
        "warn" => Ok(LevelFilter::WARN),
        "info" => Ok(LevelFilter::INFO),
        "debug" => Ok(LevelFilter::DEBUG),
        "trace" => Ok(LevelFilter::TRACE),
        _ => Err(AppError::Config(
            "tgbacky_LOG_LEVEL must be one of error,warn,info,debug,trace".to_string(),
        )),
    }
}

fn parse_media_filter(value: &str) -> Result<BTreeSet<MediaKind>> {
    MediaKind::parse_csv(value).map_err(|error| {
        AppError::Config(format!(
            "invalid tgbacky_MEDIA_FILTER: {error}; expected one of photo,image_doc,video,animation,audio,voice,document"
        ))
    })
}

fn load_credentials_from_env() -> Result<Option<TelegramCredentials>> {
    let api_id = parse_optional("tgbacky_API_ID")?;
    let api_hash = get_optional("tgbacky_API_HASH")?;
    match (api_id, api_hash) {
        (Some(api_id), Some(api_hash)) => Ok(Some(TelegramCredentials { api_id, api_hash })),
        (None, None) => Ok(None),
        _ => Err(AppError::Config(
            "tgbacky_API_ID and tgbacky_API_HASH must be provided together".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> AppConfig {
        AppConfig {
            profile: "default".to_string(),
            api_profile: "default".to_string(),
            profile_source: ProfileSource::Default,
            credential_source: CredentialSource::Flags,
            api_id: Some(1),
            api_hash: Some("hash".to_string()),
            session_path: PathBuf::from("./session.db"),
            db_path: PathBuf::from("./state.db"),
            download_dir: PathBuf::from("./downloads"),
            run_artifact_dir: PathBuf::from("./artifacts"),
            log_level: LevelFilter::ERROR,
            retry_count: 1,
            retry_backoff_ms: 1,
            download_stall_timeout_secs: 1,
            media_filter: BTreeSet::from(MediaKind::ALL),
            filename_mode: FilenameMode::Stable,
            temp_extension: ".part".to_string(),
            request_delay_ms: 1,
            download_delay_ms: 1,
            flood_sleep_threshold_secs: 5,
            jitter_ms: 0,
            download_concurrency: 3,
            download_concurrency_origin: DownloadConcurrencyOrigin::Auto,
            cleanup_stale_parts_on_start: false,
            stale_part_min_age_hours: 12,
            verbose_dependency_logs: false,
        }
    }

    #[test]
    fn parses_media_filter() {
        let parsed = parse_media_filter("photo,video,voice").expect("media filter");
        assert!(parsed.contains(&MediaKind::Photo));
        assert!(parsed.contains(&MediaKind::Video));
        assert!(parsed.contains(&MediaKind::Voice));
        assert!(!parsed.contains(&MediaKind::Document));
    }

    #[test]
    fn normalizes_temp_extension() {
        assert_eq!(normalize_temp_extension("part".to_string()), ".part");
        assert_eq!(normalize_temp_extension(".tmp".to_string()), ".tmp");
    }

    #[test]
    fn rejects_same_session_and_state_path() {
        let mut config = base_config();
        config.session_path = PathBuf::from("./same.db");
        config.db_path = PathBuf::from("./same.db");
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_overlapping_download_and_artifact_dirs() {
        let mut config = base_config();
        config.download_dir = PathBuf::from("./data");
        config.run_artifact_dir = PathBuf::from("./data/artifacts");
        assert!(config.validate().is_err());
    }

    #[test]
    fn auto_download_concurrency_is_positive() {
        assert!(default_download_concurrency() > 0);
    }

    #[test]
    fn auto_scan_window_scales_with_workers() {
        assert_eq!(auto_scan_ahead_messages(1), 64);
        assert_eq!(auto_scan_ahead_messages(4), 128);
    }

    #[test]
    fn override_session_path_is_revalidated() {
        let config = base_config();
        assert!(
            config
                .with_session_path(PathBuf::from("./state.db"))
                .is_err()
        );
    }

    #[test]
    fn sanitizes_profile_name() {
        assert_eq!(sanitize_profile_name("Personal Work"), "personal_work");
        assert_eq!(sanitize_profile_name("___"), "");
    }

    #[test]
    fn resolves_profile_source_precedence() {
        assert_eq!(
            resolve_profile_from_values(
                Some("cli".to_string()),
                Some("env".to_string()),
                Some("current".to_string()),
            )
            .expect("profile"),
            ("cli".to_string(), ProfileSource::Explicit)
        );
        assert_eq!(
            resolve_profile_from_values(None, Some("env".to_string()), Some("current".to_string()))
                .expect("profile"),
            ("env".to_string(), ProfileSource::Env)
        );
        assert_eq!(
            resolve_profile_from_values(None, None, Some("current".to_string())).expect("profile"),
            ("current".to_string(), ProfileSource::Current)
        );
        assert_eq!(
            resolve_profile_from_values(None, None, None).expect("profile"),
            ("default".to_string(), ProfileSource::Default)
        );
    }
}
