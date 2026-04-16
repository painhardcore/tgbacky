use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use keyring::Entry;
use keyring::credential::CredentialPersistence;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

const SERVICE_NAME: &str = "tgbacky";
const DEFAULT_API_PROFILE: &str = "default";
const CURRENT_API_PROFILE_FILE: &str = "current-api-profile";
const API_PROFILES_FILE: &str = "api-profiles";
const KEYCHAIN_CREDENTIALS_FIELD: &str = "credentials";
const LEGACY_API_ID_FIELD: &str = "api_id";
const LEGACY_API_HASH_FIELD: &str = "api_hash";

#[derive(Debug, Clone)]
pub struct TelegramCredentials {
    pub api_id: i32,
    pub api_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStorageKind {
    Keychain,
    LocalFile,
}

impl CredentialStorageKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Keychain => "OS keychain",
            Self::LocalFile => "local API file",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStorageStatus {
    None,
    Keychain,
    LocalFile,
    KeychainAndLocalFile,
}

impl CredentialStorageStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Keychain => "keychain",
            Self::LocalFile => "local-file",
            Self::KeychainAndLocalFile => "keychain+local-file",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSaveError {
    KeychainUnavailable(String),
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredTelegramCredentials {
    api_id: i32,
    api_hash: String,
}

pub fn load_telegram_credentials(profile: &str) -> Result<Option<TelegramCredentials>> {
    load_telegram_credentials_with_status(profile).map(|(credentials, _)| credentials)
}

pub fn load_telegram_credentials_with_status(
    profile: &str,
) -> Result<(Option<TelegramCredentials>, CredentialStorageStatus)> {
    let local_file = local_credentials_path(profile)?.exists();
    match load_telegram_credentials_from_keychain(profile) {
        Ok(Some(credentials)) => Ok((
            Some(credentials),
            storage_status_from_presence(true, local_file),
        )),
        Ok(None) | Err(KeychainAccessError::Unavailable(_)) => {
            let credentials = load_telegram_credentials_from_local_file(profile)?;
            Ok((credentials, storage_status_from_presence(false, local_file)))
        }
        Err(KeychainAccessError::Failure(message)) => Err(AppError::Config(message)),
    }
}

pub fn default_api_profile_name() -> &'static str {
    DEFAULT_API_PROFILE
}

pub fn resolve_api_profile(override_name: Option<String>) -> Result<String> {
    if let Some(name) = override_name {
        return sanitize_api_profile(&name);
    }
    if let Some(raw) = optional_env("tgbacky_API_PROFILE")? {
        return sanitize_api_profile(&raw);
    }
    if let Some(name) = current_api_profile()? {
        return Ok(name);
    }
    Ok(DEFAULT_API_PROFILE.to_string())
}

pub fn current_api_profile() -> Result<Option<String>> {
    let path = current_api_profile_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)?;
    let name = sanitize_api_profile_name(raw.trim());
    if name.is_empty() {
        return Ok(None);
    }
    Ok(Some(name))
}

pub fn set_current_api_profile(name: &str) -> Result<()> {
    let name = sanitize_api_profile(name)?;
    let path = current_api_profile_path()?;
    let parent = path.parent().ok_or_else(|| {
        AppError::Filesystem(std::io::Error::other(format!(
            "current API profile path {} has no parent directory",
            path.display()
        )))
    })?;
    fs::create_dir_all(parent)?;
    fs::write(path, format!("{name}\n"))?;
    Ok(())
}

pub fn clear_current_api_profile_if_matches(name: &str) -> Result<()> {
    let name = sanitize_api_profile(name)?;
    if current_api_profile()?.as_deref() == Some(name.as_str()) {
        let path = current_api_profile_path()?;
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ApiCredentialProfile {
    pub name: String,
    pub status: CredentialStorageStatus,
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub struct ApiCredentialCheck {
    pub status: CredentialStorageStatus,
    pub local_path: PathBuf,
    pub keychain_backend: &'static str,
    pub keychain_readable: bool,
    pub keychain_message: String,
}

pub fn check_api_credential_profile(profile: &str) -> Result<ApiCredentialCheck> {
    let local_path = local_credentials_path(profile)?;
    let (keychain_readable, keychain_message) =
        match load_telegram_credentials_from_keychain(profile) {
            Ok(Some(_)) => (true, "keychain entry readable".to_string()),
            Ok(None) => (false, "keychain entry not found".to_string()),
            Err(error) => (false, error.message()),
        };
    let status = storage_status_from_presence(keychain_readable, local_path.exists());
    Ok(ApiCredentialCheck {
        status,
        local_path,
        keychain_backend: keychain_backend_label(),
        keychain_readable,
        keychain_message,
    })
}

pub fn verify_keychain_credentials(
    profile: &str,
    expected: &TelegramCredentials,
) -> std::result::Result<(), String> {
    match load_telegram_credentials_from_keychain(profile) {
        Ok(Some(TelegramCredentials { api_id, api_hash }))
            if api_id == expected.api_id && api_hash == expected.api_hash =>
        {
            Ok(())
        }
        Ok(Some(_)) => Err("keychain entry was readable, but value did not match".to_string()),
        Ok(None) => Err("keychain entry was not found after write".to_string()),
        Err(error) => Err(error.message()),
    }
}

pub fn list_api_credential_profiles() -> Result<Vec<ApiCredentialProfile>> {
    let current = current_api_profile()?.unwrap_or_else(|| DEFAULT_API_PROFILE.to_string());
    let mut names = std::collections::BTreeSet::new();

    if let Ok(has_default) = has_keychain_credentials(DEFAULT_API_PROFILE)
        && has_default
    {
        names.insert(DEFAULT_API_PROFILE.to_string());
    }
    for name in read_registered_api_profiles()? {
        names.insert(name);
    }

    let root = api_credentials_root()?;
    if root.exists() {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some("json")
                && let Some(stem) = entry.path().file_stem().and_then(|value| value.to_str())
            {
                let name = sanitize_api_profile_name(stem);
                if !name.is_empty() {
                    names.insert(name);
                }
            }
        }
    }

    names.insert(current.clone());
    let mut entries = Vec::new();
    for name in names {
        let status = credential_storage_status(&name)?;
        if status == CredentialStorageStatus::None && name != current {
            continue;
        }
        entries.push(ApiCredentialProfile {
            is_default: name == current,
            name,
            status,
        });
    }
    Ok(entries)
}

pub fn save_telegram_credentials_to_keychain(
    profile: &str,
    credentials: &TelegramCredentials,
) -> std::result::Result<CredentialStorageKind, CredentialSaveError> {
    ensure_persistent_keychain_backend(profile)?;
    let payload = StoredTelegramCredentials {
        api_id: credentials.api_id,
        api_hash: credentials.api_hash.clone(),
    };
    let serialized = serde_json::to_string(&payload).map_err(|error| {
        CredentialSaveError::KeychainUnavailable(format!(
            "could not serialize Telegram API credentials for API profile `{profile}`: {error}"
        ))
    })?;
    set_secret(profile, KEYCHAIN_CREDENTIALS_FIELD, &serialized)?;
    register_api_profile(profile)
        .map_err(|error| CredentialSaveError::KeychainUnavailable(error.to_string()))?;
    Ok(CredentialStorageKind::Keychain)
}

pub fn save_telegram_credentials_to_local_file(
    profile: &str,
    credentials: &TelegramCredentials,
) -> Result<PathBuf> {
    let path = local_credentials_path(profile)?;
    write_local_credentials_file(&path, credentials)?;
    register_api_profile(profile)?;
    Ok(path)
}

pub fn credential_storage_status(profile: &str) -> Result<CredentialStorageStatus> {
    let keychain = match has_keychain_credentials(profile) {
        Ok(value) => value,
        Err(KeychainAccessError::Unavailable(_)) => false,
        Err(KeychainAccessError::Failure(message)) => {
            return Err(AppError::Runtime(message));
        }
    };
    let local_file = local_credentials_path(profile)?.exists();
    Ok(storage_status_from_presence(keychain, local_file))
}

fn storage_status_from_presence(keychain: bool, local_file: bool) -> CredentialStorageStatus {
    match (keychain, local_file) {
        (true, true) => CredentialStorageStatus::KeychainAndLocalFile,
        (true, false) => CredentialStorageStatus::Keychain,
        (false, true) => CredentialStorageStatus::LocalFile,
        (false, false) => CredentialStorageStatus::None,
    }
}

pub fn has_telegram_credentials(profile: &str) -> Result<bool> {
    Ok(credential_storage_status(profile)? != CredentialStorageStatus::None)
}

pub fn delete_telegram_credentials(profile: &str) -> Result<()> {
    delete_local_telegram_credentials(profile)?;

    delete_secret(profile, KEYCHAIN_CREDENTIALS_FIELD)?;
    delete_secret(profile, LEGACY_API_ID_FIELD)?;
    delete_secret(profile, LEGACY_API_HASH_FIELD)?;
    unregister_api_profile(profile)?;
    Ok(())
}

pub fn delete_local_telegram_credentials(profile: &str) -> Result<()> {
    let local_path = local_credentials_path(profile)?;
    if local_path.exists() {
        fs::remove_file(&local_path)?;
    }
    Ok(())
}

pub fn local_credentials_path(profile: &str) -> Result<PathBuf> {
    let profile = sanitize_api_profile(profile)?;
    Ok(api_credentials_root()?.join(format!("{profile}.json")))
}

pub fn keychain_backend_label() -> &'static str {
    match keyring::default::default_credential_builder().persistence() {
        CredentialPersistence::EntryOnly => "entry-only",
        CredentialPersistence::ProcessOnly => "process-only",
        CredentialPersistence::UntilReboot => "until-reboot",
        CredentialPersistence::UntilDelete => "until-delete",
        _ => "unknown",
    }
}

pub fn keychain_backend_is_persistent() -> bool {
    matches!(
        keyring::default::default_credential_builder().persistence(),
        CredentialPersistence::UntilDelete
    )
}

fn ensure_persistent_keychain_backend(
    profile: &str,
) -> std::result::Result<(), CredentialSaveError> {
    if keychain_backend_is_persistent() {
        return Ok(());
    }

    Err(CredentialSaveError::KeychainUnavailable(format!(
        "OS keychain backend is not persistent for API profile `{profile}` (backend: {}). Native keyring support is missing for this build.",
        keychain_backend_label()
    )))
}

fn load_telegram_credentials_from_keychain(
    profile: &str,
) -> std::result::Result<Option<TelegramCredentials>, KeychainAccessError> {
    if let Some(value) = get_secret(profile, KEYCHAIN_CREDENTIALS_FIELD)? {
        let stored: StoredTelegramCredentials =
            serde_json::from_str(&value).map_err(|error| {
                KeychainAccessError::Failure(format!(
                    "stored Telegram API credentials in OS keychain for profile `{profile}` are invalid: {error}"
                ))
            })?;
        let api_id = parse_api_id(
            &stored.api_id.to_string(),
            &format!("stored api_id in the OS keychain for profile `{profile}`"),
        )
        .map_err(|error| KeychainAccessError::Failure(error.to_string()))?;
        if stored.api_hash.trim().is_empty() {
            return Err(KeychainAccessError::Failure(format!(
                "stored api_hash in the OS keychain for profile `{profile}` is empty"
            )));
        }
        return Ok(Some(TelegramCredentials {
            api_id,
            api_hash: stored.api_hash,
        }));
    }

    let api_id = match get_secret(profile, LEGACY_API_ID_FIELD)? {
        Some(value) => value,
        None => return Ok(None),
    };
    let api_hash = match get_secret(profile, LEGACY_API_HASH_FIELD)? {
        Some(value) => value,
        None => return Ok(None),
    };
    if api_hash.trim().is_empty() {
        return Err(KeychainAccessError::Failure(format!(
            "stored api_hash in the OS keychain for profile `{profile}` is empty"
        )));
    }

    let api_id = parse_api_id(
        &api_id,
        &format!("stored api_id in the OS keychain for profile `{profile}`"),
    )
    .map_err(|error| KeychainAccessError::Failure(error.to_string()))?;
    Ok(Some(TelegramCredentials { api_id, api_hash }))
}

fn load_telegram_credentials_from_local_file(profile: &str) -> Result<Option<TelegramCredentials>> {
    let path = local_credentials_path(profile)?;
    if !path.exists() {
        return Ok(None);
    }

    let stored = read_local_credentials_file(&path)?;
    Ok(Some(TelegramCredentials {
        api_id: stored.api_id,
        api_hash: stored.api_hash,
    }))
}

fn has_keychain_credentials(profile: &str) -> std::result::Result<bool, KeychainAccessError> {
    if get_secret(profile, KEYCHAIN_CREDENTIALS_FIELD)?.is_some() {
        return Ok(true);
    }
    Ok(get_secret(profile, LEGACY_API_ID_FIELD)?.is_some()
        && get_secret(profile, LEGACY_API_HASH_FIELD)?.is_some())
}

fn read_local_credentials_file(path: &Path) -> Result<StoredTelegramCredentials> {
    let contents = fs::read_to_string(path)?;
    let stored: StoredTelegramCredentials = serde_json::from_str(&contents).map_err(|error| {
        AppError::Config(format!(
            "local Telegram credentials file at {} is invalid: {error}",
            path.display()
        ))
    })?;
    if stored.api_hash.trim().is_empty() {
        return Err(AppError::Config(format!(
            "local Telegram credentials file at {} contains an empty api_hash",
            path.display()
        )));
    }
    let api_id = parse_api_id(
        &stored.api_id.to_string(),
        &format!("local Telegram credentials file at {}", path.display()),
    )?;
    Ok(StoredTelegramCredentials {
        api_id,
        api_hash: stored.api_hash,
    })
}

fn write_local_credentials_file(path: &Path, credentials: &TelegramCredentials) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        AppError::Filesystem(std::io::Error::other(format!(
            "credentials path {} has no parent directory",
            path.display()
        )))
    })?;
    fs::create_dir_all(parent)?;

    let payload = StoredTelegramCredentials {
        api_id: credentials.api_id,
        api_hash: credentials.api_hash.clone(),
    };
    let serialized = serde_json::to_string_pretty(&payload)?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, serialized)?;
    set_private_permissions(&temp_path)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temp_path, path)?;
    set_private_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn parse_api_id(raw: &str, source: &str) -> Result<i32> {
    let api_id = raw.parse::<i32>().map_err(|_| {
        AppError::Config(format!(
            "{source} is invalid; run `tgbacky auth` again or set tgbacky_API_ID/tgbacky_API_HASH explicitly"
        ))
    })?;
    if api_id <= 0 {
        return Err(AppError::Config(format!("{source} must be positive")));
    }
    Ok(api_id)
}

fn get_secret(
    profile: &str,
    field: &str,
) -> std::result::Result<Option<String>, KeychainAccessError> {
    let entry = entry(profile, field)?;
    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(map_keyring_error(
            profile,
            "read Telegram API credentials from the OS keychain",
            error,
        )),
    }
}

fn set_secret(
    profile: &str,
    field: &str,
    value: &str,
) -> std::result::Result<(), CredentialSaveError> {
    let entry = entry(profile, field)
        .map_err(|error| CredentialSaveError::KeychainUnavailable(error.message()))?;
    entry.set_password(value).map_err(|error| {
        CredentialSaveError::KeychainUnavailable(human_keyring_message(
            profile,
            "store Telegram API credentials in the OS keychain",
            error,
        ))
    })
}

fn delete_secret(profile: &str, field: &str) -> Result<()> {
    let entry = entry(profile, field).map_err(|error| AppError::Runtime(error.message()))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(AppError::Runtime(human_keyring_message(
            profile,
            "delete Telegram API credentials from the OS keychain",
            error,
        ))),
    }
}

fn entry(profile: &str, field: &str) -> std::result::Result<Entry, KeychainAccessError> {
    let profile = sanitize_api_profile_name(profile);
    Entry::new(SERVICE_NAME, &format!("api:{profile}:{field}")).map_err(|error| {
        KeychainAccessError::Unavailable(format!(
            "OS keychain is unavailable for API profile `{profile}`: {error}"
        ))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeychainAccessError {
    Unavailable(String),
    Failure(String),
}

impl KeychainAccessError {
    fn message(&self) -> String {
        match self {
            Self::Unavailable(message) | Self::Failure(message) => message.clone(),
        }
    }
}

fn map_keyring_error(profile: &str, action: &str, error: keyring::Error) -> KeychainAccessError {
    match error {
        keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_) => {
            KeychainAccessError::Unavailable(human_keyring_message(profile, action, error))
        }
        _ => KeychainAccessError::Failure(human_keyring_message(profile, action, error)),
    }
}

fn human_keyring_message(profile: &str, action: &str, error: keyring::Error) -> String {
    format!(
        "OS keychain is unavailable for API profile `{profile}` while trying to {action}: {error}"
    )
}

fn api_credentials_root() -> Result<PathBuf> {
    Ok(project_data_dir()?.join("api-credentials"))
}

fn current_api_profile_path() -> Result<PathBuf> {
    Ok(project_data_dir()?.join(CURRENT_API_PROFILE_FILE))
}

fn api_profiles_path() -> Result<PathBuf> {
    Ok(project_data_dir()?.join(API_PROFILES_FILE))
}

fn project_data_dir() -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("com", "tgbacky", "tgbacky").ok_or_else(|| {
        AppError::Config("could not determine a default application data directory".to_string())
    })?;
    Ok(project_dirs.data_local_dir().to_path_buf())
}

fn sanitize_api_profile(raw: &str) -> Result<String> {
    let profile = sanitize_api_profile_name(raw);
    if profile.is_empty() {
        return Err(AppError::InvalidArgument(
            "API profile name cannot be empty".to_string(),
        ));
    }
    Ok(profile)
}

fn sanitize_api_profile_name(raw: &str) -> String {
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

fn optional_env(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(AppError::Config(format!(
            "env var {name} contains invalid unicode"
        ))),
    }
}

fn register_api_profile(name: &str) -> Result<()> {
    let name = sanitize_api_profile(name)?;
    let mut profiles = read_registered_api_profiles()?;
    profiles.insert(name);
    write_registered_api_profiles(&profiles)
}

fn unregister_api_profile(name: &str) -> Result<()> {
    let name = sanitize_api_profile(name)?;
    let mut profiles = read_registered_api_profiles()?;
    profiles.remove(&name);
    write_registered_api_profiles(&profiles)
}

fn read_registered_api_profiles() -> Result<std::collections::BTreeSet<String>> {
    let path = api_profiles_path()?;
    if !path.exists() {
        return Ok(std::collections::BTreeSet::new());
    }
    let raw = fs::read_to_string(path)?;
    Ok(raw
        .lines()
        .map(sanitize_api_profile_name)
        .filter(|name| !name.is_empty())
        .collect())
}

fn write_registered_api_profiles(profiles: &std::collections::BTreeSet<String>) -> Result<()> {
    let path = api_profiles_path()?;
    let parent = path.parent().ok_or_else(|| {
        AppError::Filesystem(std::io::Error::other(format!(
            "API profiles path {} has no parent directory",
            path.display()
        )))
    })?;
    fs::create_dir_all(parent)?;
    let mut body = profiles.iter().cloned().collect::<Vec<_>>().join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    fs::write(path, body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        StoredTelegramCredentials, TelegramCredentials, read_local_credentials_file,
        write_local_credentials_file,
    };
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    use super::{keychain_backend_is_persistent, keychain_backend_label};

    #[test]
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn native_keyring_backend_persists_credentials() {
        assert!(
            keychain_backend_is_persistent(),
            "keyring backend is `{}`; enable native keyring feature for this OS",
            keychain_backend_label()
        );
    }

    #[test]
    fn local_credentials_file_round_trip() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("credentials.json");
        let credentials = TelegramCredentials {
            api_id: 123456,
            api_hash: "abcd1234".to_string(),
        };

        write_local_credentials_file(&path, &credentials).expect("write credentials");
        let stored = read_local_credentials_file(&path).expect("read credentials");

        assert_eq!(stored.api_id, credentials.api_id);
        assert_eq!(stored.api_hash, credentials.api_hash);
    }

    #[test]
    fn local_credentials_file_rejects_invalid_api_id() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("credentials.json");
        std::fs::write(
            &path,
            serde_json::to_string(&StoredTelegramCredentials {
                api_id: 0,
                api_hash: "abcd1234".to_string(),
            })
            .expect("serialize"),
        )
        .expect("write");

        let error = read_local_credentials_file(&path).expect_err("invalid api id should fail");
        assert!(error.to_string().contains("must be positive"));
    }
}
