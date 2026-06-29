use thiserror::Error;

use crate::OauthProvider;

#[derive(Debug, Error)]
pub enum OauthError {
    #[error("oauth flow cancelled")]
    Cancelled,
}

pub struct OauthFlow {
    pub provider: OauthProvider,
}

impl OauthFlow {
    pub fn new(provider: OauthProvider) -> Self {
        Self { provider }
    }

    pub async fn start(&self) -> Result<String, OauthError> {
        let _ = self.provider;
        Err(OauthError::Cancelled)
    }
}
