use hermes_accounts::{QuotaState, Tier};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum QuotaError {
    #[error("quota exceeded")]
    Exceeded,
}

pub struct QuotaEngine {
    tier: Tier,
    state: QuotaState,
}

impl QuotaEngine {
    pub fn new(tier: Tier, state: QuotaState) -> Self {
        Self { tier, state }
    }

    pub fn deduct_tokens(&mut self, input: u64, output: u64) -> Result<(), QuotaError> {
        if input > self.state.tokens_remaining_input || output > self.state.tokens_remaining_output
        {
            return Err(QuotaError::Exceeded);
        }
        self.state.tokens_remaining_input -= input;
        self.state.tokens_remaining_output -= output;
        let _ = self.tier;
        Ok(())
    }
}
