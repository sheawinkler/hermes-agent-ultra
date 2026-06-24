use std::time::Instant;

/// Wake gate orthogonal to conversation SessionState (Listening / Thinking / Speaking).
#[derive(Debug, Clone)]
pub enum WakePhase {
    /// Waiting for sherpa-onnx KWS; ASR paused.
    Dormant,
    /// KWS hit; user must speak within grace window.
    AwakeGrace { deadline: Instant },
    /// Normal dialog allowed.
    Active,
    /// After a turn completes; idle timeout before dormant.
    IdleAfterTurn { deadline: Instant },
}

impl WakePhase {
    pub fn allows_asr(&self) -> bool {
        !matches!(self, WakePhase::Dormant)
    }

    pub fn allows_dialog(&self) -> bool {
        matches!(self, WakePhase::Active)
    }

    pub fn check_timeout(&self, now: Instant) -> bool {
        match self {
            WakePhase::AwakeGrace { deadline } | WakePhase::IdleAfterTurn { deadline } => {
                now >= *deadline
            }
            _ => false,
        }
    }
}
