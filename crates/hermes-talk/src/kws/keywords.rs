use crate::config::WakeConfig;
use crate::error::Result;
use crate::kws::text2token;

/// Encode configured phrases into sherpa-onnx `keywords_buf` (newline-separated token lines).
pub fn encode_phrases(cfg: &WakeConfig) -> Result<String> {
    text2token::encode_wake_phrases(cfg)
}
