mod auth;
mod chats;
mod download;

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use grammers_client::client::{ClientConfiguration, LoginToken, NoRetries, PasswordToken};
use grammers_client::{Client, InvocationError};
use grammers_mtsender::SenderPool;
use grammers_session::Session;
use grammers_session::storages::SqliteSession;
use grammers_session::types::PeerRef;
use tokio::sync::Mutex;

use crate::config::AppConfig;
use crate::error::{AppError, Result};
use crate::pacing::{PaceBucket, Pacer};
use crate::secrets::TelegramCredentials;
use crate::shutdown::ShutdownFlag;
use crate::types::{ChatSummary, PacingStats, ScannedMessage};

#[derive(Debug, Clone)]
pub enum AuthStep {
    Authorized,
    PasswordRequired { hint: Option<String> },
}

#[derive(Clone)]
pub enum RealMediaHandle {
    Photo(grammers_client::media::Photo),
    Document(grammers_client::media::Document),
}

enum PendingAuth {
    Login(LoginToken),
    Password(Box<PasswordToken>),
}

#[async_trait]
pub trait TelegramGateway {
    type ChatHandle: Clone + Send + Sync + 'static;
    type MediaHandle: Clone + Send + Sync + 'static;

    async fn is_authorized(&self) -> Result<bool>;
    async fn start_auth(&self, phone: &str) -> Result<()>;
    async fn submit_code(&self, code: &str) -> Result<AuthStep>;
    async fn submit_password(&self, password: &str) -> Result<()>;
    async fn list_chats(&self) -> Result<Vec<ChatSummary<Self::ChatHandle>>>;
    async fn resolve_chat(&self, query: &str) -> Result<ChatSummary<Self::ChatHandle>>;
    async fn fetch_history_batch(
        &self,
        chat: &Self::ChatHandle,
        offset_id: Option<i32>,
        batch_size: usize,
    ) -> Result<Vec<ScannedMessage<Self::MediaHandle>>>;
    async fn fetch_messages_by_ids(
        &self,
        chat: &Self::ChatHandle,
        message_ids: &[i32],
    ) -> Result<Vec<ScannedMessage<Self::MediaHandle>>>;
    async fn download_media_to_path(
        &self,
        media: &Self::MediaHandle,
        path: &Path,
        shutdown: &ShutdownFlag,
    ) -> Result<()>;
    async fn pacing_stats(&self) -> PacingStats;
}

pub struct RealTelegramGateway {
    config: AppConfig,
    session: Arc<dyn Session>,
    client: Client,
    pending_auth: Mutex<Option<PendingAuth>>,
    pacer: Pacer,
    _runner_task: tokio::task::JoinHandle<()>,
}

impl RealTelegramGateway {
    pub async fn new(config: &AppConfig) -> Result<Self> {
        let TelegramCredentials { api_id, .. } = config.telegram_credentials()?;
        if let Some(parent) = config.session_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let session = Arc::new(
            SqliteSession::open(&config.session_path)
                .await
                .map_err(|error| AppError::Session(error.to_string()))?,
        );
        let SenderPool { runner, handle, .. } = SenderPool::new(Arc::clone(&session), api_id);
        let client = Client::with_configuration(
            handle,
            ClientConfiguration {
                retry_policy: Box::new(NoRetries),
                auto_cache_peers: true,
            },
        );
        let runner_task = tokio::spawn(async move {
            let _ = runner.run().await;
        });

        Ok(Self {
            config: config.clone(),
            session,
            client,
            pending_auth: Mutex::new(None),
            pacer: Pacer::new(
                config.request_delay_ms,
                config.download_delay_ms,
                config.jitter_ms,
                config.flood_sleep_threshold_secs,
            ),
            _runner_task: runner_task,
        })
    }

    async fn invoke_with_policy<T, F, Fut>(
        &self,
        bucket: PaceBucket,
        operation: &str,
        mut action: F,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<T, InvocationError>>,
    {
        let mut attempt = 0;
        loop {
            self.pacer.wait_for_turn(bucket).await;
            match action().await {
                Ok(value) => return Ok(value),
                Err(InvocationError::Rpc(error)) if error.code == 420 => {
                    let seconds = error.value.unwrap_or(0) as i32;
                    self.pacer
                        .sleep_on_flood_wait(operation, seconds, attempt)
                        .await?;
                    attempt += 1;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

#[async_trait]
impl TelegramGateway for RealTelegramGateway {
    type ChatHandle = PeerRef;
    type MediaHandle = RealMediaHandle;

    async fn is_authorized(&self) -> Result<bool> {
        self.invoke_with_policy(PaceBucket::Request, "authorization check", || {
            self.client.is_authorized()
        })
        .await
    }

    async fn start_auth(&self, phone: &str) -> Result<()> {
        auth::start_auth_impl(self, phone).await
    }

    async fn submit_code(&self, code: &str) -> Result<AuthStep> {
        auth::submit_code_impl(self, code).await
    }

    async fn submit_password(&self, password: &str) -> Result<()> {
        auth::submit_password_impl(self, password).await
    }

    async fn list_chats(&self) -> Result<Vec<ChatSummary<Self::ChatHandle>>> {
        chats::list_chats_impl(self).await
    }

    async fn resolve_chat(&self, query: &str) -> Result<ChatSummary<Self::ChatHandle>> {
        chats::resolve_chat_impl(self, query).await
    }

    async fn fetch_history_batch(
        &self,
        chat: &Self::ChatHandle,
        offset_id: Option<i32>,
        batch_size: usize,
    ) -> Result<Vec<ScannedMessage<Self::MediaHandle>>> {
        chats::fetch_history_batch_impl(self, chat, offset_id, batch_size).await
    }

    async fn fetch_messages_by_ids(
        &self,
        chat: &Self::ChatHandle,
        message_ids: &[i32],
    ) -> Result<Vec<ScannedMessage<Self::MediaHandle>>> {
        chats::fetch_messages_by_ids_impl(self, chat, message_ids).await
    }

    async fn download_media_to_path(
        &self,
        media: &Self::MediaHandle,
        path: &Path,
        shutdown: &ShutdownFlag,
    ) -> Result<()> {
        download::download_media_to_path_impl(self, media, path, shutdown).await
    }

    async fn pacing_stats(&self) -> PacingStats {
        download::pacing_stats_impl(self).await
    }
}
