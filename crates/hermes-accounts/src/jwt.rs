use chrono::{DateTime, Utc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JwtError {
    #[error("token expired")]
    Expired,
}

#[derive(Debug, Clone)]
pub struct JwtTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
}

pub fn refresh_if_needed(tokens: &JwtTokens, now: DateTime<Utc>) -> Result<(), JwtError> {
    if now >= tokens.expires_at {
        return Err(JwtError::Expired);
    }
    Ok(())
}
