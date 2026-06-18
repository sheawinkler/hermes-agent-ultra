//! Auth flow orchestration across login methods.

use std::path::PathBuf;
use std::sync::Arc;

use hermes_config::ServerConfig;
use tracing::{debug, warn};

use super::email_otp::EmailOtpAuthProvider;
use super::provider::{AuthContext, AuthProvider};
use super::types::{AuthPollResult, AuthUserInput, LoginMethod, PendingLogin};
use super::wechat_qr::WeChatQrAuthProvider;
use crate::activation::DeviceActivation;
use crate::error::ServerClientError;
use crate::flowy::{CreditsBalance, CreditsCheckinResponse, FlowyApiClient, UserMe};
use crate::profile::ProfileStore;
use crate::session::{ServerSession, ServerTokens, TokenSource};

/// Coordinates remote server login flows.
pub struct AuthManager {
    config: ServerConfig,
    api: FlowyApiClient,
    session: ServerSession,
    hermes_home: PathBuf,
    profile_store: ProfileStore,
    providers: Vec<Arc<dyn AuthProvider>>,
}

impl AuthManager {
    pub fn new(
        config: ServerConfig,
        hermes_home: impl AsRef<std::path::Path>,
    ) -> Result<Self, ServerClientError> {
        if !config.api_ready() {
            return Err(ServerClientError::MissingBaseUrl);
        }
        let hermes_home = hermes_home.as_ref().to_path_buf();
        let api = FlowyApiClient::new(&config)?;
        let session = ServerSession::from_config(&config, &hermes_home);
        let profile_store = ProfileStore::new(&hermes_home);
        let providers: Vec<Arc<dyn AuthProvider>> = vec![
            Arc::new(WeChatQrAuthProvider),
            Arc::new(EmailOtpAuthProvider),
        ];
        Ok(Self {
            config,
            api,
            session,
            hermes_home,
            profile_store,
            providers,
        })
    }

    pub fn session(&self) -> &ServerSession {
        &self.session
    }

    pub fn api(&self) -> &FlowyApiClient {
        &self.api
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn resolve_method(&self, override_method: Option<LoginMethod>) -> LoginMethod {
        override_method.unwrap_or_else(|| self.config.auth.preferred_method.into())
    }

    fn provider_for(&self, method: LoginMethod) -> Option<&Arc<dyn AuthProvider>> {
        self.providers.iter().find(|p| p.method() == method)
    }

    fn auth_context(&self) -> AuthContext<'_> {
        AuthContext {
            api: &self.api,
            config: &self.config,
        }
    }

    pub async fn start_login(
        &self,
        method: LoginMethod,
    ) -> Result<PendingLogin, ServerClientError> {
        let provider = self.provider_for(method).ok_or_else(|| {
            ServerClientError::NotConfigured(format!("login method {}", method.as_str()))
        })?;
        provider.start(&self.auth_context()).await
    }

    pub async fn continue_login(
        &self,
        pending: &PendingLogin,
        input: AuthUserInput,
    ) -> Result<AuthPollResult, ServerClientError> {
        let provider = self.provider_for(pending.method).ok_or_else(|| {
            ServerClientError::NotConfigured(format!("login method {}", pending.method.as_str()))
        })?;
        let result = provider
            .poll_or_submit(&self.auth_context(), pending, input)
            .await?;
        if let AuthPollResult::Success(tokens) = &result {
            self.finish_login(tokens.clone()).await?;
        }
        Ok(result)
    }

    async fn finish_login(&self, tokens: ServerTokens) -> Result<(), ServerClientError> {
        self.session.save_tokens(tokens).await?;
        let profile = self.api.get_user_me(&self.session).await?;
        self.profile_store.save(&profile).await?;
        debug!(user_id = profile.id, "cached user profile after login");

        let activation = DeviceActivation::new(&self.hermes_home);
        if let Err(err) = activation
            .try_activate_for_user(&self.api, &self.session, profile.id)
            .await
        {
            warn!(error = %err, "device activation failed after login");
        }

        if let Err(err) = self.api.report_client_package(&self.session).await {
            warn!(error = %err, "client package report failed after login");
        }
        Ok(())
    }

    pub async fn logout(&self) -> Result<bool, ServerClientError> {
        let removed = self.session.logout().await?;
        if removed {
            let _ = self.profile_store.clear().await;
        }
        Ok(removed)
    }

    pub async fn whoami(&self) -> Result<WhoamiStatus, ServerClientError> {
        let source = self.session.token_source().await;
        let tokens = self.session.load_tokens().await?;
        let cached_profile = self.profile_store.load().await?;
        Ok(WhoamiStatus {
            source,
            tokens,
            cached_profile,
            server_enabled: self.config.enabled,
            base_url: self.config.base_url.clone(),
        })
    }

    pub async fn fetch_profile(&self) -> Result<UserMe, ServerClientError> {
        let profile = self.api.get_user_me(&self.session).await?;
        self.profile_store.save(&profile).await?;
        Ok(profile)
    }

    /// Best-effort activation for the current user and app version (no-op if already reported).
    pub async fn ensure_device_activation(&self) -> Result<bool, ServerClientError> {
        let status = self.whoami().await?;
        if !status.is_logged_in() {
            return Ok(false);
        }
        let profile = self.fetch_profile().await?;
        DeviceActivation::new(&self.hermes_home)
            .try_activate_for_user(&self.api, &self.session, profile.id)
            .await
    }

    pub async fn cached_profile(&self) -> Result<Option<UserMe>, ServerClientError> {
        self.profile_store.load().await
    }

    pub async fn credits_balance(&self) -> Result<CreditsBalance, ServerClientError> {
        self.api.get_credits_balance(&self.session).await
    }

    pub async fn credits_checkin(
        &self,
        time_zone: &str,
    ) -> Result<CreditsCheckinResponse, ServerClientError> {
        self.api.credits_checkin(&self.session, time_zone).await
    }

    pub async fn send_bind_email_code(&self, email: &str) -> Result<String, ServerClientError> {
        self.api.send_bind_email_code(&self.session, email).await
    }

    pub async fn bind_email(
        &self,
        email: &str,
        valid_code: &str,
        valid_code_req_no: &str,
    ) -> Result<String, ServerClientError> {
        let jwt = self
            .api
            .bind_email(&self.session, email, valid_code, valid_code_req_no)
            .await?;
        let tokens = ServerTokens::from_jwt(jwt);
        self.session.save_tokens(tokens).await?;
        let profile = self.api.get_user_me(&self.session).await?;
        self.profile_store.save(&profile).await?;
        Ok(profile.display_name())
    }

    pub async fn list_claw_models(
        &self,
        category: Option<i32>,
    ) -> Result<Vec<crate::flowy::ClawModelEntry>, ServerClientError> {
        let models = self
            .api
            .get_available_models_claw(&self.session, category)
            .await?;
        Ok(models.cloud)
    }
}

#[derive(Debug, Clone)]
pub struct WhoamiStatus {
    pub source: TokenSource,
    pub tokens: Option<ServerTokens>,
    pub cached_profile: Option<UserMe>,
    pub server_enabled: bool,
    pub base_url: String,
}

impl WhoamiStatus {
    pub fn is_logged_in(&self) -> bool {
        self.tokens
            .as_ref()
            .map(|t| !t.access_token.is_empty())
            .unwrap_or(false)
    }

    pub fn token_expired(&self) -> bool {
        self.tokens
            .as_ref()
            .map(|t| t.is_expired(0))
            .unwrap_or(false)
    }
}
