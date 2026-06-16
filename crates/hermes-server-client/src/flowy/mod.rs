//! Flowy `/claw` REST API client.

mod response;
mod types;

pub use response::{FlowyEnvelope, handle_http_and_envelope};
pub use types::*;

use hermes_config::ServerConfig;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::ServerClientError;
use crate::session::ServerSession;
use crate::transport::HttpTransport;

fn form_urlencode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Client for Flowy user account, credits, and device APIs.
pub struct FlowyApiClient {
    transport: HttpTransport,
    wechat_transport: HttpTransport,
    llm_transport: HttpTransport,
    config: ServerConfig,
}

impl FlowyApiClient {
    pub fn new(config: &ServerConfig) -> Result<Self, ServerClientError> {
        let transport =
            HttpTransport::from_base_url(&config.base_url, config.llm.request_timeout_seconds)?;
        let wechat_transport = HttpTransport::from_base_url(
            &config.effective_wechat_base_url(),
            config.llm.request_timeout_seconds,
        )?;
        let llm_transport = HttpTransport::from_base_url(
            &config.effective_llm_base_url(),
            config.llm.request_timeout_seconds,
        )?;
        Ok(Self {
            transport,
            wechat_transport,
            llm_transport,
            config: config.clone(),
        })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub async fn send_email_register_code(&self, email: &str) -> Result<String, ServerClientError> {
        let body = SendEmailCodeRequest {
            email: email.to_string(),
            channel: self.config.channel.clone(),
            app: self.config.app.clone(),
        };
        self.post_data("/user/getEmailRegisterValidCode", None, &body)
            .await
    }

    pub async fn login_by_email(
        &self,
        email: &str,
        valid_code: &str,
        valid_code_req_no: &str,
    ) -> Result<String, ServerClientError> {
        let body = LoginByEmailRequest {
            email: email.to_string(),
            valid_code: valid_code.to_string(),
            valid_code_req_no: valid_code_req_no.to_string(),
            invite_code: self.config.invite_code.clone(),
            channel: self.config.channel.clone(),
            device: String::new(),
            app: self.config.app.clone(),
        };
        let env: FlowyEnvelope = self
            .post_envelope("/user/doLoginByEmail", None, &body)
            .await?;
        env.into_jwt_token()
    }

    /// WeChat OAuth code exchange (WxLogin / Open Platform). Uses `wechat_transport` (domestic API root).
    /// Not used for user login — see `exchange_wechat_oauth_code` (scenario A in client-wechat-scan-login-qrcode.md).
    #[allow(dead_code)]
    pub async fn create_wechat_mp_session(
        &self,
    ) -> Result<WeChatMpSessionResponse, ServerClientError> {
        let body = WeChatMpSessionRequest {
            channel: self.config.channel.clone(),
            invite_code: self.config.invite_code.clone(),
        };
        self.post_data_on(
            &self.wechat_transport,
            "/auth/wechat-mp/session",
            None,
            &body,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn poll_wechat_mp_session(
        &self,
        session_id: &str,
    ) -> Result<WeChatMpPollData, ServerClientError> {
        let path = format!("/auth/wechat-mp/session/status?sessionId={session_id}");
        self.get_data_on(&self.wechat_transport, &path, None).await
    }

    pub async fn exchange_wechat_oauth_code(
        &self,
        code: &str,
        state: &str,
    ) -> Result<String, ServerClientError> {
        let mut query = format!(
            "/auth/third/callback?platform=WECHAT&code={}&state={}",
            form_urlencode(code),
            form_urlencode(state),
        );
        if !self.config.channel.trim().is_empty() {
            query.push_str("&channel=");
            query.push_str(&form_urlencode(&self.config.channel));
        }
        if !self.config.invite_code.trim().is_empty() {
            query.push_str("&inviteCode=");
            query.push_str(&form_urlencode(&self.config.invite_code));
        }
        if !self.config.app.trim().is_empty() {
            query.push_str("&app=");
            query.push_str(&form_urlencode(&self.config.app));
        }
        let env = self
            .get_envelope_on(&self.wechat_transport, &query, None)
            .await?;
        env.into_jwt_token()
    }

    pub async fn get_user_me(&self, session: &ServerSession) -> Result<UserMe, ServerClientError> {
        self.get_data("/user/me", Some(session)).await
    }

    pub async fn get_credits_balance(
        &self,
        session: &ServerSession,
    ) -> Result<CreditsBalance, ServerClientError> {
        self.get_data("/credits/balance", Some(session)).await
    }

    pub async fn get_credits_usage_by_type(
        &self,
        session: &ServerSession,
        include_team_seat: bool,
    ) -> Result<CreditsUsageByType, ServerClientError> {
        let flag = if include_team_seat { "1" } else { "0" };
        let path = format!("/credits/usageByType?includeTeamSeat={flag}");
        self.get_data(&path, Some(session)).await
    }

    pub async fn credits_checkin(
        &self,
        session: &ServerSession,
        time_zone: &str,
    ) -> Result<CreditsCheckinResponse, ServerClientError> {
        let body = CreditsCheckinRequest {
            time_zone: time_zone.to_string(),
        };
        self.post_data("/credits/checkin", Some(session), &body)
            .await
    }

    pub async fn send_bind_email_code(
        &self,
        session: &ServerSession,
        email: &str,
    ) -> Result<String, ServerClientError> {
        let body = BindEmailCodeRequest {
            email: email.to_string(),
        };
        self.post_data("/user/getBindEmailValidCode", Some(session), &body)
            .await
    }

    pub async fn bind_email(
        &self,
        session: &ServerSession,
        email: &str,
        valid_code: &str,
        valid_code_req_no: &str,
    ) -> Result<String, ServerClientError> {
        let body = BindEmailRequest {
            email: email.to_string(),
            valid_code: valid_code.to_string(),
            valid_code_req_no: valid_code_req_no.to_string(),
        };
        let env = self
            .post_envelope("/user/bindEmail", Some(session), &body)
            .await?;
        env.into_jwt_token()
    }

    pub async fn report_client_package(
        &self,
        session: &ServerSession,
    ) -> Result<(), ServerClientError> {
        let body = ClientPackageRequest {
            package_type: "stable".to_string(),
            app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            platform: Some(crate::platform::client_platform()),
            client_id: None,
        };
        self.post_no_data("/user/clientPackage", Some(session), &body)
            .await
    }

    pub async fn presence_heartbeat(
        &self,
        session: &ServerSession,
    ) -> Result<(), ServerClientError> {
        let body = PresenceHeartbeatRequest {
            platform: Some(crate::platform::client_platform()),
            app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            client_id: None,
        };
        self.post_no_data("/presence/heartbeat", Some(session), &body)
            .await
    }

    pub async fn device_activate(
        &self,
        session: &ServerSession,
        body: &DeviceActivateRequest,
    ) -> Result<(), ServerClientError> {
        self.post_no_data("/device/activate", Some(session), body)
            .await
    }

    pub async fn get_available_models_claw(
        &self,
        session: &ServerSession,
        category: Option<i32>,
    ) -> Result<AvailableModelsClaw, ServerClientError> {
        let category = category.unwrap_or(1);
        let path = format!("/model/availableListClaw?category={category}");
        self.get_data(&path, Some(session)).await
    }

    pub async fn report_chat_session(
        &self,
        session: &ServerSession,
        session_id: &str,
    ) -> Result<ChatSessionReportResponse, ServerClientError> {
        let body = ChatSessionReportRequest {
            session_id: session_id.trim().to_string(),
        };
        self.post_data_on(
            &self.llm_transport,
            "/chat/session",
            Some(session),
            &body,
        )
        .await
    }

    pub fn llm_transport(&self) -> &HttpTransport {
        &self.llm_transport
    }

    async fn get_data<T: DeserializeOwned>(
        &self,
        path: &str,
        session: Option<&ServerSession>,
    ) -> Result<T, ServerClientError> {
        self.get_data_on(&self.transport, path, session).await
    }

    async fn get_data_on<T: DeserializeOwned>(
        &self,
        transport: &HttpTransport,
        path: &str,
        session: Option<&ServerSession>,
    ) -> Result<T, ServerClientError> {
        let env = self.get_envelope_on(transport, path, session).await?;
        env.into_data()
    }

    async fn post_data<T, B: Serialize>(
        &self,
        path: &str,
        session: Option<&ServerSession>,
        body: &B,
    ) -> Result<T, ServerClientError>
    where
        T: DeserializeOwned,
    {
        let env = self.post_envelope(path, session, body).await?;
        env.into_data()
    }

    async fn post_data_on<T, B: Serialize>(
        &self,
        transport: &HttpTransport,
        path: &str,
        session: Option<&ServerSession>,
        body: &B,
    ) -> Result<T, ServerClientError>
    where
        T: DeserializeOwned,
    {
        let env = self
            .post_envelope_on(transport, path, session, body)
            .await?;
        env.into_data()
    }

    async fn post_no_data<B: Serialize>(
        &self,
        path: &str,
        session: Option<&ServerSession>,
        body: &B,
    ) -> Result<(), ServerClientError> {
        let env = self.post_envelope(path, session, body).await?;
        env.ensure_ok_no_data()
    }

    async fn get_envelope_on(
        &self,
        transport: &HttpTransport,
        path: &str,
        session: Option<&ServerSession>,
    ) -> Result<FlowyEnvelope, ServerClientError> {
        let resp = transport.get(path, session).await?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| ServerClientError::Http(e.to_string()))?;
        handle_http_and_envelope(status, &body)
    }

    async fn post_envelope<B: Serialize>(
        &self,
        path: &str,
        session: Option<&ServerSession>,
        body: &B,
    ) -> Result<FlowyEnvelope, ServerClientError> {
        self.post_envelope_on(&self.transport, path, session, body)
            .await
    }

    async fn post_envelope_on<B: Serialize>(
        &self,
        transport: &HttpTransport,
        path: &str,
        session: Option<&ServerSession>,
        body: &B,
    ) -> Result<FlowyEnvelope, ServerClientError> {
        let json = serde_json::to_value(body)
            .map_err(|e| ServerClientError::InvalidResponse(e.to_string()))?;
        let resp = transport.post_json(path, session, json).await?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| ServerClientError::Http(e.to_string()))?;
        handle_http_and_envelope(status, &body)
    }
}

#[cfg(test)]
mod api_tests {
    use super::*;
    use hermes_config::ServerConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config(base_url: &str) -> ServerConfig {
        ServerConfig {
            base_url: base_url.to_string(),
            channel: "flowy".to_string(),
            app: "flowymes".to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn email_login_roundtrip() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/getEmailRegisterValidCode"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"code":200,"msg":"ok","data":"req-no-123"}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/doLoginByEmail"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"code":200,"msg":"Login successful","data":"jwt-token-abc"}"#,
                ),
            )
            .mount(&server)
            .await;

        let config = test_config(&server.uri());
        let api = FlowyApiClient::new(&config).expect("client");
        let req_no = api
            .send_email_register_code("user@example.com")
            .await
            .expect("send code");
        assert_eq!(req_no, "req-no-123");
        let jwt = api
            .login_by_email("user@example.com", "123456", &req_no)
            .await
            .expect("login");
        assert_eq!(jwt, "jwt-token-abc");
    }

    #[tokio::test]
    async fn exchange_wechat_oauth_code() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/auth/third/callback"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"code":200,"msg":"Login successful","data":"jwt-wechat-open"}"#,
            ))
            .mount(&server)
            .await;

        let mut config = test_config(&server.uri());
        config.wechat_base_url = server.uri();
        let api = FlowyApiClient::new(&config).expect("client");
        let jwt = api
            .exchange_wechat_oauth_code("oauth-code-1", "state-xyz")
            .await
            .expect("exchange");
        assert_eq!(jwt, "jwt-wechat-open");
    }
}
