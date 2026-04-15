use rusqlite::OpenFlags;

use crate::config::AppConfig;
use crate::error::{AppError, Result};
use crate::secrets::{
    CredentialStorageStatus, credential_storage_status, keychain_backend_is_persistent,
    keychain_backend_label,
};
use crate::telegram::{RealTelegramGateway, TelegramGateway};

pub struct DoctorCommand {
    pub live: bool,
}

pub async fn run(config: &AppConfig, command: DoctorCommand) -> Result<()> {
    let mut local_failures = Vec::new();
    let mut local_warnings = Vec::new();

    println!("tgbacky doctor");
    println!("Version           : {}", env!("TGBACKY_LONG_VERSION"));
    println!(
        "Platform          : {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!(
        "Profile           : {} ({})",
        config.profile,
        config.profile_source.label()
    );
    println!("Profile root      : {}", profile_root(config).display());
    println!("Session           : {}", config.session_path.display());
    println!("State DB          : {}", config.db_path.display());
    println!("Downloads         : {}", config.download_dir.display());
    println!("Artifacts         : {}", config.run_artifact_dir.display());

    let credential_status = match credential_storage_status(&config.api_profile) {
        Ok(status) => status,
        Err(error) => {
            local_failures.push(format!("credentials are not readable: {error}"));
            CredentialStorageStatus::None
        }
    };
    println!("API profile       : {}", config.api_profile);
    println!("API credentials   : {}", credential_status.label());
    println!("Keychain backend  : {}", keychain_backend_label());
    println!();

    check_exists("session DB", &config.session_path, &mut local_failures);
    check_state_db(config, &mut local_failures);
    check_optional_directory("downloads dir", &config.download_dir, &mut local_warnings);
    check_optional_directory(
        "artifacts dir",
        &config.run_artifact_dir,
        &mut local_warnings,
    );

    if credential_status == CredentialStorageStatus::None
        && config.credential_source.label() == "none"
    {
        local_failures.push(format!(
            "no Telegram API credentials configured for API profile `{}`",
            config.api_profile
        ));
    }
    if !keychain_backend_is_persistent()
        && credential_status != CredentialStorageStatus::LocalFile
        && config.credential_source.label() == "none"
    {
        local_warnings.push(format!(
            "OS keychain backend is not persistent ({})",
            keychain_backend_label()
        ));
    }

    if !local_failures.is_empty() {
        println!();
        println!("Doctor result     : local checks failed");
        for failure in &local_failures {
            println!("- {failure}");
        }
        return Err(AppError::Config(
            "doctor found local configuration failures".to_string(),
        ));
    }

    if !local_warnings.is_empty() {
        println!();
        println!("Doctor warnings   :");
        for warning in &local_warnings {
            println!("- {warning}");
        }
    }

    if command.live {
        println!();
        println!("Live check        : enabled");
        let gateway = RealTelegramGateway::new(config).await?;
        if gateway.is_authorized().await? {
            println!("Telegram auth     : authorized");
        } else {
            println!("Telegram auth     : not authorized");
            return Err(AppError::Authentication(format!(
                "profile `{}` has a session, but it is not authorized",
                config.profile
            )));
        }
    } else {
        println!("Live check        : skipped (use `tgbacky doctor --live`)");
    }

    println!();
    if local_warnings.is_empty() {
        println!("Doctor result     : ok");
    } else {
        println!("Doctor result     : ok (with warnings)");
    }
    Ok(())
}

fn profile_root(config: &AppConfig) -> std::path::PathBuf {
    config
        .session_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| ".".into())
}

fn check_exists(label: &str, path: &std::path::Path, failures: &mut Vec<String>) {
    if path.exists() {
        println!("{label:<18}: ok");
    } else {
        println!("{label:<18}: missing");
        failures.push(format!("{label} does not exist at {}", path.display()));
    }
}

fn check_state_db(config: &AppConfig, failures: &mut Vec<String>) {
    if !config.db_path.exists() {
        println!("state DB          : missing");
        failures.push(format!(
            "state DB does not exist at {}",
            config.db_path.display()
        ));
        return;
    }

    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    match rusqlite::Connection::open_with_flags(&config.db_path, flags).and_then(|connection| {
        connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
    }) {
        Ok(version) => println!("state DB          : ok (schema v{version})"),
        Err(error) => {
            println!("state DB          : unreadable");
            failures.push(format!(
                "state DB at {} is not readable: {error}",
                config.db_path.display()
            ));
        }
    }
}

fn check_optional_directory(label: &str, path: &std::path::Path, warnings: &mut Vec<String>) {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.permissions().readonly() => {
            println!("{label:<18}: ok");
        }
        Ok(metadata) if metadata.is_dir() => {
            println!("{label:<18}: read-only");
            warnings.push(format!("{label} at {} is read-only", path.display()));
        }
        Ok(_) => {
            println!("{label:<18}: not a directory");
            warnings.push(format!("{label} at {} is not a directory", path.display()));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("{label:<18}: not created yet");
        }
        Err(error) => {
            println!("{label:<18}: unreadable");
            warnings.push(format!(
                "{label} at {} is unreadable: {error}",
                path.display()
            ));
        }
    }
}
