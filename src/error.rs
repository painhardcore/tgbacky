use grammers_client::InvocationError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("invalid arguments: {0}")]
    InvalidArgument(String),
    #[error("authentication failed: {0}")]
    Authentication(String),
    #[error("chat resolution failed: {0}")]
    ChatResolution(String),
    #[error("unsupported input: {0}")]
    Unsupported(String),
    #[error("telegram session storage error: {0}")]
    Session(String),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Filesystem(#[from] std::io::Error),
    #[error("runtime error: {0}")]
    Runtime(String),
    #[error("{0}")]
    Interrupted(String),
    #[error("telegram request failed: {0}")]
    Telegram(#[from] InvocationError),
    #[error(
        "telegram asked to wait {seconds}s during {operation}; exceeds configured auto-sleep threshold"
    )]
    FloodWaitExceeded { operation: String, seconds: i32 },
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;

impl AppError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidArgument(_) | Self::Config(_) | Self::Unsupported(_) => 2,
            Self::Session(_) => 1,
            Self::Authentication(_) => 3,
            Self::ChatResolution(_) => 4,
            Self::Database(_) => 5,
            Self::Filesystem(_) => 6,
            Self::Interrupted(_) => 130,
            Self::Runtime(_)
            | Self::Telegram(_)
            | Self::FloodWaitExceeded { .. }
            | Self::Serialization(_) => 1,
        }
    }
}
