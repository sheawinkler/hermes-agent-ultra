//! Email OTP login via Flowy `/user/getEmailRegisterValidCode` + `/user/doLoginByEmail`.

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use super::provider::{AuthContext, AuthProvider};
use super::types::{AuthPollResult, AuthUserInput, LoginMethod, PendingLogin};
use crate::error::ServerClientError;
use crate::session::ServerTokens;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmailOtpState {
    email: String,
    valid_code_req_no: String,
}

pub struct EmailOtpAuthProvider;

#[async_trait]
impl AuthProvider for EmailOtpAuthProvider {
    fn method(&self) -> LoginMethod {
        LoginMethod::EmailOtp
    }

    async fn start(&self, _ctx: &AuthContext<'_>) -> Result<PendingLogin, ServerClientError> {
        Ok(PendingLogin {
            method: LoginMethod::EmailOtp,
            message: "Enter your email address to receive a verification code.".into(),
            qr_content: None,
            qr_image_url: None,
            expires_at: None,
            provider_state: None,
        })
    }

    async fn poll_or_submit(
        &self,
        ctx: &AuthContext<'_>,
        pending: &PendingLogin,
        input: AuthUserInput,
    ) -> Result<AuthPollResult, ServerClientError> {
        match input {
            AuthUserInput::Email { address } => {
                let email = address.trim().to_string();
                if email.is_empty() || !email.contains('@') {
                    return Ok(AuthPollResult::Failed("invalid email address".into()));
                }
                let valid_code_req_no = ctx.api.send_email_register_code(&email).await?;
                let state = EmailOtpState {
                    email,
                    valid_code_req_no,
                };
                let expires_at =
                    Utc::now() + Duration::seconds(ctx.config.auth.otp_ttl_seconds as i64);
                Ok(AuthPollResult::Pending(PendingLogin {
                    method: LoginMethod::EmailOtp,
                    message: "Verification code sent. Enter the 6-digit code from your email."
                        .into(),
                    qr_content: None,
                    qr_image_url: None,
                    expires_at: Some(expires_at),
                    provider_state: Some(
                        serde_json::to_string(&state)
                            .map_err(|e| ServerClientError::InvalidResponse(e.to_string()))?,
                    ),
                }))
            }
            AuthUserInput::OtpCode { code } => {
                let state: EmailOtpState = pending
                    .provider_state
                    .as_deref()
                    .ok_or_else(|| {
                        ServerClientError::InvalidResponse(
                            "email login missing provider state — submit email first".into(),
                        )
                    })
                    .and_then(|raw| {
                        serde_json::from_str(raw)
                            .map_err(|e| ServerClientError::InvalidResponse(e.to_string()))
                    })?;
                let code = code.trim().to_string();
                if code.len() < 4 {
                    return Ok(AuthPollResult::Failed("verification code too short".into()));
                }
                let jwt = ctx
                    .api
                    .login_by_email(&state.email, &code, &state.valid_code_req_no)
                    .await?;
                Ok(AuthPollResult::Success(ServerTokens::from_jwt(jwt)))
            }
            AuthUserInput::Poll => Ok(AuthPollResult::Pending(pending.clone())),
        }
    }
}
