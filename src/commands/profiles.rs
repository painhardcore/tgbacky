use std::fs;

use crate::config::{
    clear_current_profile_if_matches, current_profile, profile_layout, profiles_root,
    set_current_profile,
};
use crate::error::Result;
use crate::secrets::{credential_storage_status, current_api_profile};

pub fn list() -> Result<()> {
    let root = profiles_root()?;
    let active_profile = current_profile()?;
    println!("Profiles root      : {}", root.display());

    let mut found_any = false;
    if root.exists() {
        let mut entries = fs::read_dir(&root)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false))
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        entries.sort();

        for profile in entries {
            found_any = true;
            let layout = profile_layout(&profile)?;
            let active_marker = if active_profile.as_deref() == Some(profile.as_str()) {
                " *"
            } else {
                ""
            };
            println!(
                "{}{}  session={} state={} downloads={}",
                profile,
                active_marker,
                yes_no(layout.session_path.exists()),
                yes_no(layout.db_path.exists()),
                yes_no(layout.download_dir.exists())
            );
        }
    }

    if !found_any {
        println!("No profiles found.");
    }

    Ok(())
}

pub fn current() -> Result<()> {
    let Some(profile) = current_profile()? else {
        println!("Current profile   : none");
        println!();
        println!("Use `tgbacky profiles use <profile>` to pick one.");
        println!("Or run `tgbacky auth --profile <profile>` to create/sign in.");
        return Ok(());
    };

    let layout = profile_layout(&profile)?;
    let api_profile = current_api_profile()?.unwrap_or_else(|| "default".to_string());
    let credential_status = credential_storage_status(&api_profile)?;
    println!("Current profile   : {profile}");
    println!("Profile root      : {}", layout.root_dir.display());
    println!("Session           : {}", layout.session_path.display());
    println!("State DB          : {}", layout.db_path.display());
    println!("Downloads         : {}", layout.download_dir.display());
    println!("Artifacts         : {}", layout.run_artifact_dir.display());
    println!("API profile       : {api_profile}");
    println!("API credentials   : {}", credential_status.label());
    Ok(())
}

pub fn use_profile(profile: &str) -> Result<()> {
    let profile = crate::config::sanitize_profile(profile)?;
    let layout = profile_layout(&profile)?;
    let api_profile = current_api_profile()?.unwrap_or_else(|| "default".to_string());
    let credential_status = credential_storage_status(&api_profile)?;

    set_current_profile(&profile)?;

    println!("Current profile set to `{profile}`.");
    println!("Profile root      : {}", layout.root_dir.display());
    println!("Session           : {}", layout.session_path.display());
    println!("State DB          : {}", layout.db_path.display());
    println!("Downloads         : {}", layout.download_dir.display());
    println!("API profile       : {api_profile}");
    println!("API credentials   : {}", credential_status.label());

    if !layout.root_dir.exists() {
        println!();
        println!("Warning: this profile directory does not exist yet.");
    }
    if !layout.session_path.exists() {
        println!("Warning: no Telegram session DB exists for this profile yet.");
    }
    if credential_status.label() == "none" {
        println!("Warning: no default Telegram API credentials are stored yet.");
    }

    println!();
    println!("Next auth command:");
    println!("tgbacky auth --profile {profile}");
    Ok(())
}

pub fn delete(profile: &str, yes: bool) -> Result<()> {
    let profile = crate::config::sanitize_profile(profile)?;
    let layout = profile_layout(&profile)?;

    if !yes {
        println!("Delete profile `{profile}`?");
        println!("This removes:");
        println!("- {}", layout.session_path.display());
        println!("- {}", layout.db_path.display());
        println!("- {}", layout.run_artifact_dir.display());
        println!("- {}", layout.download_dir.display());
        println!("- profile root {}", layout.root_dir.display());
        println!("It does not remove global API credentials.");
        print!("Type `yes` to continue: ");
        use std::io::{self, Write};
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if answer.trim() != "yes" {
            println!("Profile deletion cancelled.");
            return Ok(());
        }
    }

    clear_current_profile_if_matches(&profile)?;
    if layout.root_dir.exists() {
        fs::remove_dir_all(&layout.root_dir)?;
    }

    println!("Deleted profile `{profile}`.");
    println!("Removed profile-managed local data.");
    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
