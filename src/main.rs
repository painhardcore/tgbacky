use std::process::ExitCode;

// grammers-session uses libsql for session persistence, and that backend expects
// a single-thread Tokio runtime. tgbacky does sequential work anyway, so we keep
// the main runtime current-thread and push isolated blocking work to spawn_blocking.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match tgbacky::cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(tgbacky::error::AppError::Interrupted(_)) => ExitCode::from(130),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit_code())
        }
    }
}
