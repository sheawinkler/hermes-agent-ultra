//! Shared sherpa-onnx runtime settings (ONNX Runtime execution provider).

use crate::error::{DemoError, Result};

/// Only CPU execution provider is supported.
pub const PLATFORM_PROVIDERS: &[&str] = &["cpu"];

pub fn platform_supports(provider: &str) -> bool {
    provider == "cpu"
}

pub fn validate_provider(provider: &str) -> Result<()> {
    if platform_supports(provider) {
        Ok(())
    } else {
        Err(DemoError::Config(format!(
            "invalid sherpa provider '{provider}' (only 'cpu' is supported; directml/coreml removed)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_supported() {
        validate_provider("cpu").unwrap();
    }

    #[test]
    fn rejects_directml_and_coreml() {
        assert!(validate_provider("directml").is_err());
        assert!(validate_provider("coreml").is_err());
        assert!(validate_provider("gpu").is_err());
    }
}
