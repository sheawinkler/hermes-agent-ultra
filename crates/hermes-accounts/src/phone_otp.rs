use thiserror::Error;

#[derive(Debug, Error)]
pub enum PhoneOtpError {
    #[error("rate limited")]
    RateLimited,
    #[error("invalid otp")]
    InvalidOtp,
}

pub struct PhoneOtpFlow {
    pub phone: String,
}

impl PhoneOtpFlow {
    pub fn new(phone: String) -> Self {
        Self { phone }
    }

    pub async fn request_otp(&self) -> Result<(), PhoneOtpError> {
        let _ = &self.phone;
        Ok(())
    }

    pub async fn verify_otp(&self, _code: &str) -> Result<(), PhoneOtpError> {
        Ok(())
    }
}
