use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmailAuthError {
    #[error("invalid otp")]
    InvalidOtp,
}

pub struct EmailAuthFlow {
    pub email: String,
}

impl EmailAuthFlow {
    pub fn new(email: String) -> Self {
        Self { email }
    }

    pub async fn request_otp(&self) -> Result<(), EmailAuthError> {
        let _ = &self.email;
        Ok(())
    }

    pub async fn verify_otp(&self, _code: &str) -> Result<(), EmailAuthError> {
        Ok(())
    }
}
