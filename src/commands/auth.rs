use crate::error::Result;
use crate::telegram::{AuthStep, TelegramGateway};

pub async fn request_code<G: TelegramGateway>(gateway: &G, phone: &str) -> Result<()> {
    gateway.start_auth(phone).await?;
    Ok(())
}

pub async fn submit_code<G: TelegramGateway>(gateway: &G, code: &str) -> Result<AuthStep> {
    gateway.submit_code(code).await
}

pub async fn complete_password<G: TelegramGateway>(gateway: &G, password: &str) -> Result<()> {
    gateway.submit_password(password).await
}
