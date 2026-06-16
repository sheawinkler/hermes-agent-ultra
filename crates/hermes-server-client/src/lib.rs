//! Remote LLM server client — authentication and OpenAI-compatible inference gateway.
//!
//! Agent business logic (AgentLoop, tools, sessions) stays local; this crate only
//! talks to the server for login and LLM HTTP calls.

pub mod activation;
pub mod auth;
pub mod doctor;
pub mod error;
pub mod flowy;
pub mod llm;
pub mod paths;
pub mod platform;
pub mod profile;
pub mod session;
pub mod transport;

pub use activation::DeviceActivation;
pub use auth::{
    AuthManager, AuthPollResult, AuthUserInput, LoginMethod, PendingLogin, WhoamiStatus,
};
pub use doctor::{DoctorReport, run_doctor};
pub use error::ServerClientError;
pub use flowy::{
    ClawModelEntry, CreditsBalance, CreditsCheckinResponse, FlowyApiClient, UserMe,
};
pub use llm::ServerLlmProvider;
pub use profile::ProfileStore;
pub use session::{SERVER_TOKEN_PROVIDER, ServerSession, ServerTokens, TokenSource};
pub use transport::HttpTransport;

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::ServerConfig;

    #[test]
    fn login_method_parse_aliases() {
        assert_eq!(LoginMethod::parse("wechat"), Some(LoginMethod::WechatQr));
        assert_eq!(LoginMethod::parse("email_otp"), Some(LoginMethod::EmailOtp));
        assert!(LoginMethod::parse("unknown").is_none());
    }

    #[tokio::test]
    async fn auth_manager_missing_base_url() {
        let config = ServerConfig::default();
        let result = AuthManager::new(config, std::env::temp_dir());
        assert!(matches!(result, Err(ServerClientError::MissingBaseUrl)));
    }
}
