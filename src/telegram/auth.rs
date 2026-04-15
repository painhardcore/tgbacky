use crate::error::{AppError, Result};
use crate::pacing::PaceBucket;
use crate::secrets::TelegramCredentials;
use crate::telegram::{AuthStep, PendingAuth, RealTelegramGateway};
use grammers_client::SignInError;

pub(super) async fn start_auth_impl(gateway: &RealTelegramGateway, phone: &str) -> Result<()> {
    let phone = phone.trim().to_string();
    let TelegramCredentials { api_hash, .. } = gateway.config.telegram_credentials()?;
    let token = gateway
        .invoke_with_policy(PaceBucket::Request, "request login code", || {
            gateway.client.request_login_code(&phone, &api_hash)
        })
        .await?;
    *gateway.pending_auth.lock().await = Some(PendingAuth::Login(token));
    Ok(())
}

pub(super) async fn submit_code_impl(
    gateway: &RealTelegramGateway,
    code: &str,
) -> Result<AuthStep> {
    let login_token = {
        let mut guard = gateway.pending_auth.lock().await;
        match guard.take() {
            Some(PendingAuth::Login(token)) => token,
            Some(PendingAuth::Password(token)) => {
                *guard = Some(PendingAuth::Password(token));
                return Err(AppError::Authentication(
                    "password is required before another code can be submitted".to_string(),
                ));
            }
            None => {
                return Err(AppError::Authentication(
                    "no pending authentication flow".to_string(),
                ));
            }
        }
    };

    match gateway.client.sign_in(&login_token, code.trim()).await {
        Ok(_) => Ok(AuthStep::Authorized),
        Err(SignInError::PasswordRequired(password_token)) => {
            let hint = password_token.hint().map(ToString::to_string);
            *gateway.pending_auth.lock().await =
                Some(PendingAuth::Password(Box::new(password_token)));
            Ok(AuthStep::PasswordRequired { hint })
        }
        Err(SignInError::InvalidCode) => Err(AppError::Authentication(
            "invalid Telegram login code".to_string(),
        )),
        Err(SignInError::SignUpRequired) => Err(AppError::Authentication(
            "Telegram reports this account needs sign-up, which is unsupported here".to_string(),
        )),
        Err(SignInError::InvalidPassword(_)) => Err(AppError::Authentication(
            "Telegram rejected the code".to_string(),
        )),
        Err(SignInError::Other(error)) => Err(error.into()),
    }
}

pub(super) async fn submit_password_impl(
    gateway: &RealTelegramGateway,
    password: &str,
) -> Result<()> {
    let password_token = {
        let mut guard = gateway.pending_auth.lock().await;
        match guard.take() {
            Some(PendingAuth::Password(token)) => *token,
            Some(PendingAuth::Login(token)) => {
                *guard = Some(PendingAuth::Login(token));
                return Err(AppError::Authentication(
                    "a login code must be submitted before password".to_string(),
                ));
            }
            None => {
                return Err(AppError::Authentication(
                    "no pending password step".to_string(),
                ));
            }
        }
    };

    match gateway
        .client
        .check_password(password_token, password.trim())
        .await
    {
        Ok(_) => Ok(()),
        Err(SignInError::InvalidPassword(token)) => {
            *gateway.pending_auth.lock().await = Some(PendingAuth::Password(Box::new(token)));
            Err(AppError::Authentication(
                "invalid Telegram 2FA password".to_string(),
            ))
        }
        Err(SignInError::PasswordRequired(token)) => {
            *gateway.pending_auth.lock().await = Some(PendingAuth::Password(Box::new(token)));
            Err(AppError::Authentication(
                "Telegram still requires a 2FA password".to_string(),
            ))
        }
        Err(SignInError::InvalidCode) => Err(AppError::Authentication(
            "Telegram rejected the password flow".to_string(),
        )),
        Err(SignInError::SignUpRequired) => Err(AppError::Authentication(
            "Telegram reports this account needs sign-up, which is unsupported here".to_string(),
        )),
        Err(SignInError::Other(error)) => Err(error.into()),
    }
}
