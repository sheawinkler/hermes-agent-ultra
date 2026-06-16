//! Auth provider trait — one implementation per server login method.

use async_trait::async_trait;

use hermes_config::ServerConfig;

use crate::auth::types::{AuthPollResult, AuthUserInput, LoginMethod, PendingLogin};
use crate::error::ServerClientError;
use crate::flowy::FlowyApiClient;

/// Shared context for auth provider HTTP calls.
pub struct AuthContext<'a> {
    pub api: &'a FlowyApiClient,
    pub config: &'a ServerConfig,
}

/// Pluggable login method (WeChat QR, email OTP, …).
#[async_trait]
pub trait AuthProvider: Send + Sync {
    fn method(&self) -> LoginMethod;

    async fn start(&self, ctx: &AuthContext<'_>) -> Result<PendingLogin, ServerClientError>;

    async fn poll_or_submit(
        &self,
        ctx: &AuthContext<'_>,
        pending: &PendingLogin,
        input: AuthUserInput,
    ) -> Result<AuthPollResult, ServerClientError>;
}
