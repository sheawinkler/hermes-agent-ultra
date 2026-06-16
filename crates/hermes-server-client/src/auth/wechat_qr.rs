//! WeChat Open Platform QR scan login (official `open.weixin.qq.com` QR + Flowy token exchange).

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use super::provider::{AuthContext, AuthProvider};
use super::types::{AuthPollResult, AuthUserInput, LoginMethod, PendingLogin};
use super::wechat_open::{
    WeChatOpenSession, WeChatPollStatus, build_wechat_redirect_uri, poll_wechat_open_session,
    start_wechat_open_session,
};
use crate::error::ServerClientError;
use crate::session::ServerTokens;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WeChatQrProviderState {
    session: WeChatOpenSession,
    /// WxLogin long-poll `last=` query param after scan (WeChat uses 404).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_poll_errcode: Option<u32>,
}

pub struct WeChatQrAuthProvider;

#[async_trait]
impl AuthProvider for WeChatQrAuthProvider {
    fn method(&self) -> LoginMethod {
        LoginMethod::WechatQr
    }

    async fn start(&self, ctx: &AuthContext<'_>) -> Result<PendingLogin, ServerClientError> {
        let app_id = ctx.config.effective_wechat_app_id();
        let redirect_uri = build_wechat_redirect_uri(&ctx.config.effective_wechat_base_url());
        tracing::info!(
            app_id = %app_id,
            channel = %ctx.config.channel,
            redirect_uri = %redirect_uri,
            "starting WeChat WxLogin session"
        );
        let session = start_wechat_open_session(&app_id, &redirect_uri).await?;
        let state_json = serde_json::to_string(&WeChatQrProviderState {
            session: session.clone(),
            last_poll_errcode: None,
        })
        .map_err(|e| ServerClientError::InvalidResponse(e.to_string()))?;

        Ok(PendingLogin {
            method: LoginMethod::WechatQr,
            message: "Scan the WeChat QR code with WeChat on your phone to sign in.".into(),
            qr_content: Some(session.qr_scan_payload.clone()),
            qr_image_url: Some(session.qr_image_url.clone()),
            expires_at: Some(Utc::now() + Duration::seconds(300)),
            provider_state: Some(state_json),
        })
    }

    async fn poll_or_submit(
        &self,
        ctx: &AuthContext<'_>,
        pending: &PendingLogin,
        input: AuthUserInput,
    ) -> Result<AuthPollResult, ServerClientError> {
        let _ = input;
        let mut provider_state: WeChatQrProviderState = pending
            .provider_state
            .as_deref()
            .ok_or_else(|| {
                ServerClientError::InvalidResponse("WeChat login missing provider state".into())
            })
            .and_then(|raw| {
                serde_json::from_str(raw).map_err(|e| {
                    ServerClientError::InvalidResponse(e.to_string())
                })
            })?;

        if let Some(expires_at) = pending.expires_at
            && Utc::now() >= expires_at
        {
            return Ok(AuthPollResult::Failed(
                "WeChat QR session expired — start login again".into(),
            ));
        }

        let poll = poll_wechat_open_session(
            &provider_state.session.uuid,
            provider_state.last_poll_errcode,
        )
        .await?;

        if let Some(errcode) = poll.errcode {
            provider_state.last_poll_errcode = Some(errcode);
        }

        let state_json = serde_json::to_string(&provider_state)
            .map_err(|e| ServerClientError::InvalidResponse(e.to_string()))?;

        match poll.status {
            WeChatPollStatus::Waiting => Ok(AuthPollResult::Pending(PendingLogin {
                provider_state: Some(state_json),
                ..pending.clone()
            })),
            WeChatPollStatus::Scanned => Ok(AuthPollResult::Pending(PendingLogin {
                message: "QR scanned — confirm login on your phone.".into(),
                provider_state: Some(state_json),
                ..pending.clone()
            })),
            WeChatPollStatus::Expired => Ok(AuthPollResult::Failed(
                "WeChat QR session expired — start login again".into(),
            )),
            WeChatPollStatus::Denied => Ok(AuthPollResult::Failed(
                "WeChat login cancelled on your phone".into(),
            )),
            WeChatPollStatus::Authorized { code } => {
                let jwt = ctx
                    .api
                    .exchange_wechat_oauth_code(&code, &provider_state.session.state)
                    .await?;
                Ok(AuthPollResult::Success(ServerTokens::from_jwt(jwt)))
            }
        }
    }
}
