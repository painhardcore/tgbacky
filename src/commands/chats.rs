use std::collections::BTreeSet;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::app::open_export_context;
use crate::config::AppConfig;
use crate::error::Result;
use crate::fsutil::{cleanup_file_if_exists, temp_sidecar_path};
use crate::telegram::{RealTelegramGateway, TelegramGateway};
use crate::types::ChatKind;

pub async fn list(config: &AppConfig) -> Result<()> {
    let progress = command_progress("connecting to Telegram");
    let gateway = match RealTelegramGateway::new(config).await {
        Ok(gateway) => gateway,
        Err(error) => {
            progress.finish_and_clear();
            return Err(error);
        }
    };
    progress.set_message("loading chats from Telegram");
    let chats = match gateway.list_chats().await {
        Ok(chats) => chats,
        Err(error) => {
            progress.finish_and_clear();
            return Err(error);
        }
    };
    progress.finish_and_clear();

    println!(
        "{:>16}  {:<12}  {:<24}  title",
        "chat_id", "type", "username"
    );
    let mut hidden_titles = 0usize;
    for chat in chats {
        let title = display_title(chat.kind, chat.id, &chat.title);
        if chat.title.trim().is_empty() {
            hidden_titles += 1;
        }
        println!(
            "{:>16}  {:<12}  {:<24}  {}",
            chat.id,
            chat.kind,
            display_username(chat.username.as_deref()),
            title
        );
    }
    if hidden_titles > 0 {
        println!();
        println!("Note: {hidden_titles} chat(s) have no display name from Telegram; use chat_id.");
    }
    Ok(())
}

pub async fn reset(
    config: &AppConfig,
    chat_query: &str,
    keep_files: bool,
    yes: bool,
) -> Result<()> {
    let progress = command_progress("connecting to Telegram");
    let mut context = match open_export_context(config).await {
        Ok(context) => context,
        Err(error) => {
            progress.finish_and_clear();
            return Err(error);
        }
    };
    progress.set_message("resolving chat");
    let chat = match context.gateway.resolve_chat(chat_query).await {
        Ok(chat) => chat,
        Err(error) => {
            progress.finish_and_clear();
            return Err(error);
        }
    };
    progress.set_message("loading local chat state");
    let media = match context.database.list_media_for_chat(chat.id) {
        Ok(media) => media,
        Err(error) => {
            progress.finish_and_clear();
            return Err(error);
        }
    };
    let checkpoint = match context.database.load_checkpoint(chat.id) {
        Ok(checkpoint) => checkpoint,
        Err(error) => {
            progress.finish_and_clear();
            return Err(error);
        }
    };
    progress.finish_and_clear();

    if !yes {
        println!(
            "Reset chat `{}` ({})?",
            display_title(chat.kind, chat.id, &chat.title),
            chat.id
        );
        println!("- checkpoint present: {}", yes_no(checkpoint.is_some()));
        println!("- tracked media rows: {}", media.len());
        println!("- tracked files will be deleted: {}", yes_no(!keep_files));
        print!("Type `yes` to continue: ");
        use std::io::{self, Write};
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if answer.trim() != "yes" {
            println!("Chat reset cancelled.");
            return Ok(());
        }
    }

    let mut removed_files = 0usize;
    if !keep_files {
        let mut tracked_paths = BTreeSet::new();
        for record in &media {
            tracked_paths.insert(record.local_path.clone());
            tracked_paths.insert(temp_sidecar_path(
                &record.local_path,
                &config.temp_extension,
            ));
        }

        for path in tracked_paths {
            if path.exists() {
                cleanup_file_if_exists(&path).await?;
                removed_files += 1;
            }
        }
    }

    context.database.reset_chat_state(chat.id)?;
    println!(
        "Reset chat `{}` ({})",
        display_title(chat.kind, chat.id, &chat.title),
        chat.id
    );
    println!("Removed media rows : {}", media.len());
    println!("Removed checkpoint : {}", yes_no(checkpoint.is_some()));
    println!("Removed files      : {}", removed_files);
    println!("Next export for this chat will start from scratch.");
    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn command_progress(message: &'static str) -> ProgressBar {
    let progress = ProgressBar::new_spinner();
    let style = ProgressStyle::with_template("{spinner} {msg}")
        .map(|style| style.tick_strings(&["-", "\\", "|", "/"]))
        .unwrap_or_else(|_| ProgressStyle::default_spinner());
    progress.set_style(style);
    progress.enable_steady_tick(Duration::from_millis(120));
    progress.set_message(message);
    progress
}

fn display_username(username: Option<&str>) -> String {
    username
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| "-".to_string())
}

fn display_title(kind: ChatKind, id: i64, title: &str) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return title.to_string();
    }

    match kind {
        ChatKind::User => format!("(no title; user id {id})"),
        ChatKind::Group => format!("(no title; group id {id})"),
        ChatKind::Supergroup => format!("(no title; supergroup id {id})"),
        ChatKind::Channel => format!("(no title; channel id {id})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_title_names_missing_user() {
        assert_eq!(
            display_title(ChatKind::User, 42, ""),
            "(no title; user id 42)"
        );
    }

    #[test]
    fn display_username_hides_empty_values() {
        assert_eq!(display_username(None), "-");
        assert_eq!(display_username(Some("")), "-");
        assert_eq!(display_username(Some("alice")), "alice");
    }
}
