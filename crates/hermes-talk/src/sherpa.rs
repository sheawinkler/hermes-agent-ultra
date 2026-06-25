//! Shared sherpa-onnx runtime settings (ONNX Runtime execution provider).

use crate::error::{DemoError, Result};

/// ONNX Runtime execution providers accepted in talk config.
pub const VALID_PROVIDERS: &[&str] = &["cpu", "cuda", "directml", "coreml"];

pub fn validate_provider(provider: &str) -> Result<()> {
    if VALID_PROVIDERS.contains(&provider) {
        Ok(())
    } else {
        Err(DemoError::Config(format!(
            "invalid sherpa provider '{provider}' (expected one of: cpu, cuda, directml, coreml)"
        )))
    }
}

pub fn provider_hint(provider: &str) -> Option<&'static str> {
    match provider {
        "cuda" => Some(
            "build with --features sherpa-cuda and install CUDA 12.x + cuDNN 9; \
             run scripts/talk/fetch_sherpa_runtime.sh cuda",
        ),
        "directml" => Some(
            "build with --features sherpa-directml and set SHERPA_ONNX_LIB_DIR to a DirectML-enabled \
             sherpa-onnx lib/ directory (no official prebuilt release)",
        ),
        "coreml" => {
            Some("build with --features sherpa-coreml on macOS; set provider=coreml in config")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_known_providers() {
        for p in VALID_PROVIDERS {
            validate_provider(p).unwrap();
        }
    }

    #[test]
    fn rejects_gpu_alias() {
        assert!(validate_provider("gpu").is_err());
    }
}
