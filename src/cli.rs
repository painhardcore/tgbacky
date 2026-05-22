use std::io::{self, Write};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::Directive;

use crate::commands;
use crate::config::{
    AppConfig, ConfigOverrides, DownloadConcurrencyOrigin, current_profile, set_current_profile,
};
use crate::error::{AppError, Result};
use crate::secrets::{
    CredentialSaveError, CredentialStorageKind, TelegramCredentials, check_api_credential_profile,
    clear_current_api_profile_if_matches, credential_storage_status, current_api_profile,
    delete_local_telegram_credentials, delete_telegram_credentials, list_api_credential_profiles,
    save_telegram_credentials_to_keychain, save_telegram_credentials_to_local_file,
    set_current_api_profile, verify_keychain_credentials,
};
use crate::telegram::{RealTelegramGateway, TelegramGateway};
use crate::types::MediaKind;

#[derive(Debug, Parser)]
#[command(
    name = "tgbacky",
    version = env!("CARGO_PKG_VERSION"),
    long_version = env!("TGBACKY_LONG_VERSION"),
    about = "Backup Telegram media locally with durable resume state"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Args, Default)]
struct ProfileArgs {
    #[arg(long)]
    profile: Option<String>,
    #[arg(long = "session")]
    session_path: Option<PathBuf>,
    #[arg(long = "db")]
    db_path: Option<PathBuf>,
    #[arg(long = "download-dir")]
    download_dir: Option<PathBuf>,
    #[arg(long = "artifacts-dir")]
    run_artifact_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Args, Default)]
struct TelegramApiArgs {
    #[arg(long = "api-profile")]
    api_profile: Option<String>,
    #[arg(long)]
    api_id: Option<i32>,
    #[arg(long)]
    api_hash: Option<String>,
}

#[derive(Debug, Clone, Args, Default)]
struct RuntimeTuningArgs {
    #[arg(long, value_name = "MS")]
    delay_ms: Option<u64>,
    #[arg(long, value_name = "SECS")]
    flood_sleep_threshold_secs: Option<u64>,
    #[arg(long, value_name = "MS")]
    jitter_ms: Option<u64>,
    #[arg(long, value_name = "N")]
    retry_count: Option<u32>,
    #[arg(long, value_name = "MS")]
    retry_backoff_ms: Option<u64>,
    #[arg(long, value_name = "SECS")]
    download_stall_timeout_secs: Option<u64>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Auth(AuthArgs),
    Api {
        #[command(subcommand)]
        command: ApiCommand,
    },
    Profiles {
        #[command(subcommand)]
        command: ProfilesCommand,
    },
    Chats {
        #[command(subcommand)]
        command: ChatsCommand,
    },
    Runs {
        #[command(subcommand)]
        command: RunsCommand,
    },
    Recover {
        #[command(subcommand)]
        command: RecoverCommand,
    },
    Doctor(DoctorArgs),
    Verify(VerifyArgs),
    Export(Box<ExportArgs>),
}

#[derive(Debug, Subcommand)]
enum ApiCommand {
    #[command(about = "List saved Telegram API credential sets")]
    List,
    #[command(about = "Add or replace a Telegram API credential set")]
    Add(ApiAddArgs),
    #[command(about = "Make an API credential set the default")]
    Use { name: String },
    #[command(about = "Check API credential storage without printing secrets")]
    Check { name: Option<String> },
    #[command(about = "Delete a Telegram API credential set")]
    Delete(ApiDeleteArgs),
}

#[derive(Debug, Args)]
struct ApiAddArgs {
    #[arg(long, default_value = "default")]
    name: String,
    #[arg(long)]
    api_id: Option<i32>,
    #[arg(long)]
    api_hash: Option<String>,
    #[arg(long)]
    default: bool,
}

#[derive(Debug, Args)]
struct ApiDeleteArgs {
    name: String,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct AuthArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[command(flatten)]
    telegram_api: TelegramApiArgs,
    #[command(flatten)]
    runtime: RuntimeTuningArgs,
}

#[derive(Debug, Subcommand)]
enum ChatsCommand {
    List(ChatsListArgs),
    Reset(ChatsResetArgs),
}

#[derive(Debug, Subcommand)]
enum ProfilesCommand {
    List,
    Current,
    Use {
        profile: String,
    },
    Delete {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
struct ChatsListArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[command(flatten)]
    telegram_api: TelegramApiArgs,
    #[command(flatten)]
    runtime: RuntimeTuningArgs,
}

#[derive(Debug, Args)]
struct ChatsResetArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[command(flatten)]
    telegram_api: TelegramApiArgs,
    #[command(flatten)]
    runtime: RuntimeTuningArgs,
    #[arg(long, allow_negative_numbers = true)]
    chat: String,
    #[arg(long)]
    keep_files: bool,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Subcommand)]
enum RunsCommand {
    List(RunsListArgs),
}

#[derive(Debug, Args)]
struct RunsListArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[arg(long, default_value_t = 10)]
    limit: usize,
    #[arg(long)]
    failed_only: bool,
}

#[derive(Debug, Subcommand)]
enum RecoverCommand {
    StaleParts(RecoverArgs),
}

#[derive(Debug, Args)]
struct RecoverArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    delete: bool,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[command(flatten)]
    telegram_api: TelegramApiArgs,
    #[command(flatten)]
    runtime: RuntimeTuningArgs,
    #[arg(long)]
    live: bool,
}

#[derive(Debug, Args)]
struct VerifyArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[command(flatten)]
    telegram_api: TelegramApiArgs,
    #[command(flatten)]
    runtime: RuntimeTuningArgs,
    #[arg(long, allow_negative_numbers = true)]
    chat: String,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    deep: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ExportArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    #[command(flatten)]
    telegram_api: TelegramApiArgs,
    #[command(flatten)]
    runtime: RuntimeTuningArgs,
    #[command(subcommand)]
    command: Option<ExportSubcommand>,
    #[arg(long, allow_negative_numbers = true, action = clap::ArgAction::Append)]
    chat: Vec<String>,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long, hide = true)]
    resume: bool,
    #[arg(long)]
    since_id: Option<i32>,
    #[arg(long)]
    until_id: Option<i32>,
    #[arg(long = "only", value_name = "KINDS")]
    only_media: Option<String>,
    #[arg(long = "skip", value_name = "KINDS")]
    skip_media: Option<String>,
    #[arg(long)]
    date_from: Option<chrono::NaiveDate>,
    #[arg(long)]
    date_to: Option<chrono::NaiveDate>,
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long, value_name = "N")]
    workers: Option<usize>,
    #[arg(long)]
    verbose_progress: bool,
    #[arg(long)]
    json_report: bool,
    #[arg(long, alias = "full-rescan")]
    rescan: bool,
}

#[derive(Debug, Subcommand)]
enum ExportSubcommand {
    Plan(ExportPlanArgs),
}

#[derive(Debug, Args)]
struct ExportPlanArgs {
    #[arg(long, allow_negative_numbers = true)]
    chat: String,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long = "only", value_name = "KINDS")]
    only_media: Option<String>,
    #[arg(long = "skip", value_name = "KINDS")]
    skip_media: Option<String>,
    #[arg(long)]
    since_id: Option<i32>,
    #[arg(long)]
    until_id: Option<i32>,
    #[arg(long)]
    date_from: Option<chrono::NaiveDate>,
    #[arg(long)]
    date_to: Option<chrono::NaiveDate>,
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long, alias = "full-rescan")]
    rescan: bool,
    #[arg(long)]
    save_queue: bool,
    #[arg(long)]
    json: bool,
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Auth(args) => {
            let overrides = build_config_overrides(&args.profile, Some(&args.telegram_api));
            let mut config = AppConfig::load(overrides.clone())?;
            apply_runtime_tuning(&mut config, &args.runtime)?;
            init_tracing(&config);
            auth_command(config, overrides, args.profile.profile.is_some()).await
        }
        Command::Api { command } => match command {
            ApiCommand::List => api_list(),
            ApiCommand::Add(args) => api_add(args),
            ApiCommand::Use { name } => api_use(&name),
            ApiCommand::Check { name } => api_check(name.as_deref()),
            ApiCommand::Delete(args) => api_delete(&args.name, args.yes),
        },
        Command::Profiles {
            command: ProfilesCommand::List,
        } => commands::profiles::list(),
        Command::Profiles {
            command: ProfilesCommand::Current,
        } => commands::profiles::current(),
        Command::Profiles {
            command: ProfilesCommand::Use { profile },
        } => commands::profiles::use_profile(&profile),
        Command::Profiles {
            command: ProfilesCommand::Delete { profile, yes },
        } => commands::profiles::delete(&profile, yes),
        Command::Chats {
            command: ChatsCommand::List(args),
        } => {
            let overrides = build_config_overrides(&args.profile, Some(&args.telegram_api));
            let mut config = AppConfig::load(overrides.clone())?;
            apply_runtime_tuning(&mut config, &args.runtime)?;
            init_tracing(&config);
            let config = ensure_authorized_session(
                config,
                overrides,
                "chats list",
                args.profile.profile.is_some(),
            )
            .await?;
            commands::chats::list(&config).await
        }
        Command::Chats {
            command: ChatsCommand::Reset(args),
        } => {
            let overrides = build_config_overrides(&args.profile, Some(&args.telegram_api));
            let mut config = AppConfig::load(overrides.clone())?;
            apply_runtime_tuning(&mut config, &args.runtime)?;
            init_tracing(&config);
            let config = ensure_authorized_session(
                config,
                overrides,
                "chats reset",
                args.profile.profile.is_some(),
            )
            .await?;
            commands::chats::reset(&config, &args.chat, args.keep_files, args.yes).await
        }
        Command::Runs {
            command: RunsCommand::List(args),
        } => {
            let config = AppConfig::load(build_config_overrides(&args.profile, None))?;
            init_tracing(&config);
            commands::runs::list(&config, args.limit, args.failed_only).await
        }
        Command::Recover {
            command: RecoverCommand::StaleParts(args),
        } => {
            let config = AppConfig::load(build_config_overrides(&args.profile, None))?;
            init_tracing(&config);
            commands::recover::stale_parts(&config, args.out, args.delete).await
        }
        Command::Doctor(args) => {
            let overrides = build_config_overrides(&args.profile, Some(&args.telegram_api));
            let mut config = AppConfig::load(overrides)?;
            apply_runtime_tuning(&mut config, &args.runtime)?;
            init_tracing(&config);
            commands::doctor::run(&config, commands::doctor::DoctorCommand { live: args.live })
                .await
        }
        Command::Verify(args) => {
            let overrides = build_config_overrides(&args.profile, Some(&args.telegram_api));
            let mut config = AppConfig::load(overrides)?;
            apply_runtime_tuning(&mut config, &args.runtime)?;
            init_tracing(&config);
            commands::verify::run(
                &config,
                commands::verify::VerifyCommand {
                    chat: args.chat,
                    out_dir: args.out,
                    deep: args.deep,
                    json: args.json,
                },
            )
            .await
        }
        Command::Export(args) => {
            let overrides = build_config_overrides(&args.profile, Some(&args.telegram_api));
            let mut config = AppConfig::load(overrides.clone())?;
            apply_runtime_tuning(&mut config, &args.runtime)?;
            init_tracing(&config);
            let mut config = ensure_authorized_session(
                config,
                overrides,
                "export",
                args.profile.profile.is_some(),
            )
            .await?;
            if let Some(ExportSubcommand::Plan(plan_args)) = args.command {
                let media_filter = resolve_media_filter(
                    &config,
                    plan_args.only_media.as_deref(),
                    plan_args.skip_media.as_deref(),
                )?;
                return commands::export::plan(
                    &config,
                    commands::export::ExportPlanCommand {
                        chat: plan_args.chat,
                        out_dir: plan_args.out.unwrap_or_else(|| config.download_dir.clone()),
                        media_filter,
                        since_id: plan_args.since_id,
                        until_id: plan_args.until_id,
                        date_from: plan_args.date_from,
                        date_to: plan_args.date_to,
                        limit: plan_args.limit,
                        rescan: plan_args.rescan,
                        save_queue: plan_args.save_queue,
                        json: plan_args.json,
                    },
                )
                .await;
            }
            if let Some(workers) = args.workers {
                if workers == 0 {
                    return Err(AppError::InvalidArgument(
                        "--workers must be greater than zero".to_string(),
                    ));
                }
                config.download_concurrency = workers;
                config.download_concurrency_origin = DownloadConcurrencyOrigin::Cli;
            }
            let media_filter = resolve_media_filter(
                &config,
                args.only_media.as_deref(),
                args.skip_media.as_deref(),
            )?;
            commands::export::run(
                &config,
                commands::export::ExportCommand {
                    chats: args.chat,
                    out_dir: args.out.unwrap_or_else(|| config.download_dir.clone()),
                    resume: args.resume,
                    verbose_progress: args.verbose_progress,
                    media_filter,
                    since_id: args.since_id,
                    until_id: args.until_id,
                    date_from: args.date_from,
                    date_to: args.date_to,
                    limit: args.limit,
                    json_report: args.json_report,
                    rescan: args.rescan,
                },
            )
            .await
        }
    }
}

fn build_config_overrides(
    profile: &ProfileArgs,
    telegram_api: Option<&TelegramApiArgs>,
) -> ConfigOverrides {
    let mut overrides = ConfigOverrides {
        profile: profile.profile.clone(),
        session_path: profile.session_path.clone(),
        db_path: profile.db_path.clone(),
        download_dir: profile.download_dir.clone(),
        run_artifact_dir: profile.run_artifact_dir.clone(),
        ..ConfigOverrides::default()
    };
    if let Some(telegram_api) = telegram_api {
        overrides.api_profile = telegram_api.api_profile.clone();
        overrides.api_id = telegram_api.api_id;
        overrides.api_hash = telegram_api.api_hash.clone();
    }
    overrides
}

fn apply_runtime_tuning(config: &mut AppConfig, runtime: &RuntimeTuningArgs) -> Result<()> {
    if let Some(delay_ms) = runtime.delay_ms {
        if delay_ms == 0 {
            return Err(AppError::InvalidArgument(
                "--delay-ms must be greater than zero".to_string(),
            ));
        }
        config.request_delay_ms = delay_ms;
        config.download_delay_ms = delay_ms;
    }
    if let Some(flood_sleep_threshold_secs) = runtime.flood_sleep_threshold_secs {
        config.flood_sleep_threshold_secs = flood_sleep_threshold_secs;
    }
    if let Some(jitter_ms) = runtime.jitter_ms {
        config.jitter_ms = jitter_ms;
    }
    if let Some(retry_count) = runtime.retry_count {
        config.retry_count = retry_count;
    }
    if let Some(retry_backoff_ms) = runtime.retry_backoff_ms {
        if retry_backoff_ms == 0 && config.retry_count > 0 {
            return Err(AppError::InvalidArgument(
                "--retry-backoff-ms must be greater than zero when retries are enabled".to_string(),
            ));
        }
        config.retry_backoff_ms = retry_backoff_ms;
    }
    if let Some(download_stall_timeout_secs) = runtime.download_stall_timeout_secs {
        if download_stall_timeout_secs == 0 {
            return Err(AppError::InvalidArgument(
                "--download-stall-timeout-secs must be greater than zero".to_string(),
            ));
        }
        config.download_stall_timeout_secs = download_stall_timeout_secs;
    }
    Ok(())
}

async fn ensure_authorized_session(
    config: AppConfig,
    mut overrides: ConfigOverrides,
    requested_command: &str,
    explicit_profile: bool,
) -> Result<AppConfig> {
    let config = maybe_choose_profile_for_onboarding(config, &mut overrides, explicit_profile)?;
    let mut prompted_credentials = false;
    let credentials = match config.telegram_credentials() {
        Ok(credentials) => credentials,
        Err(_) => {
            println!(
                "No Telegram API credentials are configured for API profile `{}` yet.",
                config.api_profile
            );
            println!(
                "`tgbacky {requested_command}` needs a Telegram login before it can continue."
            );
            if !prompt_yes_no("Add Telegram API credentials now? [Y/n]: ", true)? {
                return Err(AppError::Authentication(
                    "Telegram API credentials are not configured; run `tgbacky api add` when you are ready"
                        .to_string(),
                ));
            }
            prompted_credentials = true;
            prompt_telegram_credentials()?
        }
    };

    let auth_config = config.with_telegram_credentials(credentials.clone())?;
    let gateway = RealTelegramGateway::new(&auth_config).await?;

    if gateway.is_authorized().await? {
        if prompted_credentials {
            let storage = persist_telegram_credentials(&auth_config, &credentials)?;
            println!(
                "Found an already authorized Telegram session for profile `{}`.",
                auth_config.profile
            );
            print_credentials_storage_message(&auth_config, storage);
        }
        set_current_profile(&auth_config.profile)?;
        return Ok(auth_config);
    }

    println!(
        "No authorized Telegram user is currently signed in for profile `{}`.",
        auth_config.profile
    );
    if !prompted_credentials
        && !prompt_yes_no(
            "Start Telegram login now so this command can continue? [Y/n]: ",
            true,
        )?
    {
        return Err(AppError::Authentication(format!(
            "Telegram login is required for profile `{}`; run `tgbacky auth --profile {}` to sign in",
            auth_config.profile, auth_config.profile
        )));
    }

    let storage = run_interactive_auth_flow(&auth_config, &gateway, &credentials).await?;
    print_auth_success(&auth_config, storage);
    print_post_auth_help(&auth_config);
    print_onboarding_summary(&auth_config, Some(requested_command));
    set_current_profile(&auth_config.profile)?;
    println!("Continuing with your original `{requested_command}` command...");
    Ok(auth_config)
}

fn resolve_media_filter(
    config: &AppConfig,
    only_media: Option<&str>,
    skip_media: Option<&str>,
) -> Result<std::collections::BTreeSet<MediaKind>> {
    let mut filter = if let Some(raw) = only_media {
        MediaKind::parse_csv(raw).map_err(|error| {
            AppError::InvalidArgument(format!(
                "invalid --only value: {error}; expected photo,image_doc,video,animation,audio,voice,document"
            ))
        })?
    } else {
        config.media_filter.clone()
    };

    if let Some(raw) = skip_media {
        let skipped = MediaKind::parse_csv(raw).map_err(|error| {
            AppError::InvalidArgument(format!(
                "invalid --skip value: {error}; expected photo,image_doc,video,animation,audio,voice,document"
            ))
        })?;
        for kind in skipped {
            filter.remove(&kind);
        }
    }

    if filter.is_empty() {
        return Err(AppError::InvalidArgument(
            "effective media filter is empty; choose at least one media kind".to_string(),
        ));
    }

    Ok(filter)
}

fn init_tracing(config: &AppConfig) {
    let mut filter = if config.verbose_dependency_logs {
        EnvFilter::default().add_directive(Directive::from(config.log_level))
    } else {
        let mut filter = EnvFilter::default().add_directive(Directive::from(
            tracing_subscriber::filter::LevelFilter::ERROR,
        ));
        let app_directive = format!(
            "{}={}",
            env!("CARGO_PKG_NAME"),
            format_level(config.log_level)
        );
        if let Ok(directive) = app_directive.parse() {
            filter = filter.add_directive(directive);
        }
        filter
    };

    if !config.verbose_dependency_logs {
        for directive in [
            "grammers_client=error",
            "grammers_mtsender=error",
            "grammers_mtproto=error",
            "grammers_session=error",
            "libsql=error",
        ] {
            if let Ok(directive) = directive.parse() {
                filter = filter.add_directive(directive);
            }
        }
    }

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .try_init();
}

fn format_level(level: tracing_subscriber::filter::LevelFilter) -> &'static str {
    match level {
        tracing_subscriber::filter::LevelFilter::OFF => "off",
        tracing_subscriber::filter::LevelFilter::ERROR => "error",
        tracing_subscriber::filter::LevelFilter::WARN => "warn",
        tracing_subscriber::filter::LevelFilter::INFO => "info",
        tracing_subscriber::filter::LevelFilter::DEBUG => "debug",
        tracing_subscriber::filter::LevelFilter::TRACE => "trace",
    }
}

fn api_list() -> Result<()> {
    let entries = list_api_credential_profiles()?;
    println!("{:<20}  {:<18}  default", "name", "storage");
    for entry in entries {
        println!(
            "{:<20}  {:<18}  {}",
            entry.name,
            entry.status.label(),
            yes_no(entry.is_default)
        );
    }
    Ok(())
}

fn api_add(args: ApiAddArgs) -> Result<()> {
    let credentials = credentials_from_args_or_prompt(args.api_id, args.api_hash)?;
    let storage = persist_api_credentials(&args.name, &credentials)?;
    let default_was_set = args.default || current_api_profile()?.is_none();
    if default_was_set {
        set_current_api_profile(&args.name)?;
    }
    println!("Saved API credentials `{}`.", args.name);
    print_api_credentials_storage_message(&args.name, storage);
    if default_was_set {
        println!("Default API profile set to `{}`.", args.name);
    }
    Ok(())
}

fn api_use(name: &str) -> Result<()> {
    let status = credential_storage_status(name)?;
    if status == crate::secrets::CredentialStorageStatus::None {
        return Err(AppError::InvalidArgument(format!(
            "API profile `{name}` has no stored credentials; run `tgbacky api add --name {name}` first"
        )));
    }
    set_current_api_profile(name)?;
    println!("Default API profile set to `{name}`.");
    println!("Storage           : {}", status.label());
    Ok(())
}

fn api_check(name: Option<&str>) -> Result<()> {
    let name = match name {
        Some(name) => name.to_string(),
        None => current_api_profile()?.unwrap_or_else(|| "default".to_string()),
    };
    let check = check_api_credential_profile(&name)?;
    println!("API profile       : {name}");
    println!("Storage           : {}", check.status.label());
    println!("Keychain backend  : {}", check.keychain_backend);
    println!("Keychain readable : {}", yes_no(check.keychain_readable));
    println!("Keychain detail   : {}", check.keychain_message);
    println!("Local file        : {}", check.local_path.display());
    println!("Local file exists : {}", yes_no(check.local_path.exists()));
    Ok(())
}

fn api_delete(name: &str, yes: bool) -> Result<()> {
    let status = credential_storage_status(name)?;
    if status == crate::secrets::CredentialStorageStatus::None {
        println!("API profile `{name}` has no stored credentials.");
        return Ok(());
    }

    if !yes {
        println!("Delete API credentials `{name}`?");
        println!("Storage           : {}", status.label());
        print!("Type `yes` to continue: ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if answer.trim() != "yes" {
            println!("API credential deletion cancelled.");
            return Ok(());
        }
    }

    delete_telegram_credentials(name)?;
    clear_current_api_profile_if_matches(name)?;
    if current_api_profile()?.is_none()
        && let Some(next) = list_api_credential_profiles()?
            .into_iter()
            .find(|entry| entry.status != crate::secrets::CredentialStorageStatus::None)
    {
        set_current_api_profile(&next.name)?;
        println!("Default API profile moved to `{}`.", next.name);
    }
    println!("Deleted API credentials `{name}`.");
    Ok(())
}

async fn auth_command(
    config: AppConfig,
    mut overrides: ConfigOverrides,
    explicit_profile: bool,
) -> Result<()> {
    let config = maybe_choose_profile_for_onboarding(config, &mut overrides, explicit_profile)?;
    let credentials = match config.telegram_credentials() {
        Ok(credentials) => credentials,
        Err(_) => prompt_telegram_credentials()?,
    };
    let auth_config = config.with_telegram_credentials(credentials.clone())?;
    let gateway = RealTelegramGateway::new(&auth_config).await?;

    if gateway.is_authorized().await? {
        let storage = persist_telegram_credentials(&auth_config, &credentials)?;
        println!(
            "Telegram session for profile `{}` is already authorized at {}.",
            auth_config.profile,
            auth_config.session_path.display()
        );
        print_credentials_storage_message(&auth_config, storage);
        set_current_profile(&auth_config.profile)?;
        return Ok(());
    }

    let storage = run_interactive_auth_flow(&auth_config, &gateway, &credentials).await?;
    print_auth_success(&auth_config, storage);
    print_post_auth_help(&auth_config);
    print_onboarding_summary(&auth_config, None);
    set_current_profile(&auth_config.profile)?;
    Ok(())
}

fn maybe_choose_profile_for_onboarding(
    config: AppConfig,
    overrides: &mut ConfigOverrides,
    explicit_profile: bool,
) -> Result<AppConfig> {
    let needs_onboarding = config.telegram_credentials().is_err();
    if explicit_profile
        || !needs_onboarding
        || config.profile != "default"
        || current_profile()?.is_some()
    {
        return Ok(config);
    }

    println!("No profile name was provided, so `tgbacky` would use `default`.");
    let chosen = prompt("Profile name [default]: ")?;
    let chosen = chosen.trim();
    if chosen.is_empty() || chosen == "default" {
        return Ok(config);
    }

    overrides.profile = Some(chosen.to_string());
    AppConfig::load(overrides.clone())
}

fn print_auth_success(config: &AppConfig, storage: PersistedCredentialStorage) {
    println!(
        "Authorization completed successfully for profile `{}`.",
        config.profile
    );
    println!("Session saved to {}.", config.session_path.display());
    print_credentials_storage_message(config, storage);
}

fn persist_telegram_credentials(
    config: &AppConfig,
    credentials: &TelegramCredentials,
) -> Result<PersistedCredentialStorage> {
    let storage = persist_api_credentials(&config.api_profile, credentials)?;
    if current_api_profile()?.is_none() {
        set_current_api_profile(&config.api_profile)?;
    }
    Ok(storage)
}

fn persist_api_credentials(
    api_profile: &str,
    credentials: &TelegramCredentials,
) -> Result<PersistedCredentialStorage> {
    match save_telegram_credentials_to_keychain(api_profile, credentials) {
        Ok(CredentialStorageKind::Keychain) => {
            if verify_keychain_credentials(api_profile, credentials).is_ok() {
                delete_local_telegram_credentials(api_profile)?;
                Ok(PersistedCredentialStorage::Keychain)
            } else {
                let reason = verify_keychain_credentials(api_profile, credentials)
                    .err()
                    .unwrap_or_else(|| "unknown keychain readback failure".to_string());
                println!(
                    "OS keychain accepted the write, but tgbacky could not read it back for API profile `{api_profile}`."
                );
                println!("Keychain detail: {reason}");
                save_plaintext_api_credentials_after_warning(api_profile, credentials)
            }
        }
        Ok(CredentialStorageKind::LocalFile) => {
            save_plaintext_api_credentials_after_warning(api_profile, credentials)
        }
        Err(CredentialSaveError::KeychainUnavailable(message)) => {
            println!("{message}");
            println!(
                "Telegram API credentials were not saved to the OS keychain for API profile `{api_profile}`."
            );
            save_plaintext_api_credentials_after_warning(api_profile, credentials)
        }
    }
}

fn save_plaintext_api_credentials_after_warning(
    api_profile: &str,
    credentials: &TelegramCredentials,
) -> Result<PersistedCredentialStorage> {
    println!("You can continue in one of two ways:");
    println!("- use tgbacky_API_ID/tgbacky_API_HASH or --api-id/--api-hash on future runs");
    println!("- save the credentials in a local plaintext API file with restricted permissions");
    if prompt_yes_no(
        "Save Telegram API credentials in a local plaintext file instead? [y/N]: ",
        false,
    )? {
        let path = save_telegram_credentials_to_local_file(api_profile, credentials)?;
        Ok(PersistedCredentialStorage::LocalFile(path))
    } else {
        Ok(PersistedCredentialStorage::NotSaved)
    }
}

async fn run_interactive_auth_flow(
    config: &AppConfig,
    gateway: &RealTelegramGateway,
    credentials: &TelegramCredentials,
) -> Result<PersistedCredentialStorage> {
    let phone = prompt("Phone number (international format): ")?;
    commands::auth::request_code(gateway, &phone).await?;
    let code = prompt("Login code: ")?;
    match commands::auth::submit_code(gateway, &code).await? {
        crate::telegram::AuthStep::Authorized => persist_telegram_credentials(config, credentials),
        crate::telegram::AuthStep::PasswordRequired { hint } => {
            let label = hint
                .filter(|value| !value.is_empty())
                .map(|value| format!("2FA password (hint: {value}): "))
                .unwrap_or_else(|| "2FA password: ".to_string());
            let password = rpassword::prompt_password(label)
                .map_err(|error| AppError::Authentication(error.to_string()))?;
            commands::auth::complete_password(gateway, &password).await?;
            persist_telegram_credentials(config, credentials)
        }
    }
}

fn print_credentials_storage_message(config: &AppConfig, storage: PersistedCredentialStorage) {
    print_api_credentials_storage_message(&config.api_profile, storage)
}

fn print_api_credentials_storage_message(api_profile: &str, storage: PersistedCredentialStorage) {
    match storage {
        PersistedCredentialStorage::Keychain => {
            println!("Telegram API credentials saved to OS keychain as `{api_profile}`.");
        }
        PersistedCredentialStorage::LocalFile(path) => {
            println!(
                "Telegram API credentials saved to local file for `{api_profile}`: {}.",
                path.display()
            );
            println!("This file is sensitive because it stores the API hash in plaintext.");
        }
        PersistedCredentialStorage::NotSaved => {
            println!(
                "Telegram API credentials were not saved locally. Reuse tgbacky_API_ID/tgbacky_API_HASH or --api-id/--api-hash on future runs."
            );
        }
    }
}

fn print_post_auth_help(config: &AppConfig) {
    println!("Useful next commands:");
    println!("- switch to a different account: tgbacky auth --profile other");
    println!("- list configured profiles: tgbacky profiles list");
    println!(
        "- delete this profile and its local data: tgbacky profiles delete --profile {} --yes",
        config.profile
    );
}

fn print_onboarding_summary(config: &AppConfig, requested_command: Option<&str>) {
    println!("Profile summary:");
    println!("- profile: {}", config.profile);
    println!("- session: {}", config.session_path.display());
    println!("- state db: {}", config.db_path.display());
    println!("- downloads: {}", config.download_dir.display());
    match requested_command {
        Some(command) => {
            println!("- next step: continue `tgbacky {command}` with this profile");
        }
        None => {
            println!(
                "- next step: use this profile with commands like `tgbacky export --profile {} --chat @example`",
                config.profile
            );
        }
    }
}

fn prompt_yes_no(label: &str, default_yes: bool) -> Result<bool> {
    let answer = prompt(label)?;
    if answer.is_empty() {
        return Ok(default_yes);
    }
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Err(AppError::InvalidArgument(
            "please answer `yes` or `no`".to_string(),
        )),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[derive(Debug)]
enum PersistedCredentialStorage {
    Keychain,
    LocalFile(PathBuf),
    NotSaved,
}

fn prompt_telegram_credentials() -> Result<TelegramCredentials> {
    println!("Telegram API credentials are required for user-account login.");
    println!("Get them from Telegram here:");
    println!("- https://my.telegram.org");
    println!("- sign in with your Telegram account");
    println!("- open \"API development tools\"");
    println!("- create an application and copy the shown api_id and api_hash");
    println!("Official Telegram docs: https://core.telegram.org/api/obtaining_api_id");
    let api_id = prompt("Telegram API ID: ")?.parse::<i32>().map_err(|_| {
        AppError::InvalidArgument("Telegram API ID must be a positive integer".to_string())
    })?;
    if api_id <= 0 {
        return Err(AppError::InvalidArgument(
            "Telegram API ID must be a positive integer".to_string(),
        ));
    }
    let api_hash = rpassword::prompt_password("Telegram API hash (input hidden): ")
        .map_err(|error| AppError::Authentication(error.to_string()))?;
    if api_hash.trim().is_empty() {
        return Err(AppError::InvalidArgument(
            "Telegram API hash cannot be empty".to_string(),
        ));
    }
    Ok(TelegramCredentials {
        api_id,
        api_hash: api_hash.trim().to_string(),
    })
}

fn credentials_from_args_or_prompt(
    api_id: Option<i32>,
    api_hash: Option<String>,
) -> Result<TelegramCredentials> {
    match (api_id, api_hash) {
        (Some(api_id), Some(api_hash)) => {
            if api_id <= 0 {
                return Err(AppError::InvalidArgument(
                    "Telegram API ID must be a positive integer".to_string(),
                ));
            }
            if api_hash.trim().is_empty() {
                return Err(AppError::InvalidArgument(
                    "Telegram API hash cannot be empty".to_string(),
                ));
            }
            Ok(TelegramCredentials {
                api_id,
                api_hash: api_hash.trim().to_string(),
            })
        }
        (None, None) => prompt_telegram_credentials(),
        _ => Err(AppError::InvalidArgument(
            "--api-id and --api-hash must be provided together".to_string(),
        )),
    }
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use clap::Parser;
    use tracing_subscriber::filter::LevelFilter;

    use super::{Cli, Command, ExportSubcommand, resolve_media_filter};
    use crate::config::{AppConfig, CredentialSource, DownloadConcurrencyOrigin, ProfileSource};
    use crate::types::{FilenameMode, MediaKind};

    fn test_config() -> AppConfig {
        AppConfig {
            profile: "default".to_string(),
            api_profile: "default".to_string(),
            profile_source: ProfileSource::Default,
            credential_source: CredentialSource::Flags,
            api_id: Some(1),
            api_hash: Some("hash".to_string()),
            session_path: PathBuf::from("data/session.db"),
            db_path: PathBuf::from("data/state.db"),
            download_dir: PathBuf::from("downloads"),
            run_artifact_dir: PathBuf::from("artifacts"),
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
            download_concurrency: 2,
            download_concurrency_origin: DownloadConcurrencyOrigin::Auto,
            cleanup_stale_parts_on_start: false,
            stale_part_min_age_hours: 12,
            verbose_dependency_logs: false,
        }
    }

    #[test]
    fn resolves_only_filter_override() {
        let filter =
            resolve_media_filter(&test_config(), Some("photo,video"), None).expect("filter");
        assert_eq!(filter, BTreeSet::from([MediaKind::Photo, MediaKind::Video]));
    }

    #[test]
    fn resolves_skip_filter_override() {
        let filter =
            resolve_media_filter(&test_config(), None, Some("document,audio")).expect("filter");
        assert!(!filter.contains(&MediaKind::Document));
        assert!(!filter.contains(&MediaKind::Audio));
        assert!(filter.contains(&MediaKind::Photo));
    }

    #[test]
    fn export_accepts_negative_numeric_chat_id() {
        let cli = Cli::try_parse_from([
            "tgbacky",
            "export",
            "--chat",
            "-1001406612170",
            "--out",
            "downloads",
        ])
        .expect("parse export args");

        let Command::Export(args) = cli.command else {
            panic!("expected export command");
        };
        assert_eq!(args.chat, vec!["-1001406612170"]);
    }

    #[test]
    fn export_keeps_options_after_negative_numeric_chat_id() {
        let cli = Cli::try_parse_from([
            "tgbacky",
            "export",
            "--chat",
            "-1001406612170",
            "--workers",
            "1",
            "--out",
            "downloads",
            "--json-report",
        ])
        .expect("parse export args");

        let Command::Export(args) = cli.command else {
            panic!("expected export command");
        };
        assert_eq!(args.chat, vec!["-1001406612170"]);
        assert_eq!(args.workers, Some(1));
        assert_eq!(args.out, Some(PathBuf::from("downloads")));
        assert!(args.json_report);
    }

    #[test]
    fn export_accepts_repeated_chats() {
        let cli = Cli::try_parse_from([
            "tgbacky",
            "export",
            "--chat",
            "@one",
            "--chat",
            "-1001406612170",
            "--chat",
            "Family, Photos",
            "--out",
            "downloads",
        ])
        .expect("parse export args");

        let Command::Export(args) = cli.command else {
            panic!("expected export command");
        };
        assert_eq!(args.chat, vec!["@one", "-1001406612170", "Family, Photos"]);
        assert_eq!(args.out, Some(PathBuf::from("downloads")));
    }

    #[test]
    fn export_rejects_missing_chat_value_without_swallowing_next_flag() {
        assert!(
            Cli::try_parse_from(["tgbacky", "export", "--chat", "--out", "downloads"]).is_err()
        );
    }

    #[test]
    fn export_still_accepts_username_chat() {
        let cli = Cli::try_parse_from(["tgbacky", "export", "--chat", "@example"])
            .expect("parse export args");

        let Command::Export(args) = cli.command else {
            panic!("expected export command");
        };
        assert_eq!(args.chat, vec!["@example"]);
    }

    #[test]
    fn export_still_accepts_exact_title_chat() {
        let cli = Cli::try_parse_from(["tgbacky", "export", "--chat", "Family Photos"])
            .expect("parse export args");

        let Command::Export(args) = cli.command else {
            panic!("expected export command");
        };
        assert_eq!(args.chat, vec!["Family Photos"]);
    }

    #[test]
    fn export_plan_accepts_negative_numeric_chat_id() {
        let cli = Cli::try_parse_from([
            "tgbacky",
            "export",
            "plan",
            "--chat",
            "-1001406612170",
            "--out",
            "downloads",
        ])
        .expect("parse export plan args");

        let Command::Export(args) = cli.command else {
            panic!("expected export command");
        };
        let Some(ExportSubcommand::Plan(plan_args)) = args.command else {
            panic!("expected export plan command");
        };
        assert_eq!(plan_args.chat, "-1001406612170");
    }

    #[test]
    fn export_plan_keeps_flags_after_negative_numeric_chat_id() {
        let cli = Cli::try_parse_from([
            "tgbacky",
            "export",
            "plan",
            "--chat",
            "-1001406612170",
            "--out",
            "downloads",
            "--save-queue",
        ])
        .expect("parse export plan args");

        let Command::Export(args) = cli.command else {
            panic!("expected export command");
        };
        let Some(ExportSubcommand::Plan(plan_args)) = args.command else {
            panic!("expected export plan command");
        };
        assert_eq!(plan_args.chat, "-1001406612170");
        assert_eq!(plan_args.out, Some(PathBuf::from("downloads")));
        assert!(plan_args.save_queue);
    }

    #[test]
    fn verify_accepts_negative_numeric_chat_id() {
        let cli = Cli::try_parse_from([
            "tgbacky",
            "verify",
            "--chat",
            "-1001406612170",
            "--out",
            "downloads",
        ])
        .expect("parse verify args");

        let Command::Verify(args) = cli.command else {
            panic!("expected verify command");
        };
        assert_eq!(args.chat, "-1001406612170");
    }

    #[test]
    fn verify_rejects_missing_chat_value_without_swallowing_next_flag() {
        assert!(
            Cli::try_parse_from(["tgbacky", "verify", "--chat", "--out", "downloads"]).is_err()
        );
    }

    #[test]
    fn chats_reset_accepts_negative_numeric_chat_id() {
        let cli = Cli::try_parse_from([
            "tgbacky",
            "chats",
            "reset",
            "--chat",
            "-1001406612170",
            "--yes",
        ])
        .expect("parse chats reset args");

        let Command::Chats {
            command: super::ChatsCommand::Reset(args),
        } = cli.command
        else {
            panic!("expected chats reset command");
        };
        assert_eq!(args.chat, "-1001406612170");
    }

    #[test]
    fn chats_reset_rejects_missing_chat_value_without_swallowing_next_flag() {
        assert!(Cli::try_parse_from(["tgbacky", "chats", "reset", "--chat", "--yes"]).is_err());
    }
}
