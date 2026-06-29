use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConsentGateError {
    #[error("consent required for provider {0}")]
    Missing(String),
}

pub struct ConsentGate;

impl ConsentGate {
    pub fn check(provider_id: &str, granted: bool) -> Result<(), ConsentGateError> {
        if granted {
            Ok(())
        } else {
            Err(ConsentGateError::Missing(provider_id.to_string()))
        }
    }
}
