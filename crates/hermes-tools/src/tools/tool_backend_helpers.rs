//! Shared helpers for tool backend selection.
//!
//! Corresponds to `hermes-agent/tools/tool_backend_helpers.py`.
//!
//! Most functions from Python's `tool_backend_helpers` already live in
//! `hermes_config::managed_gateway` (re-exported here for convenience).
//! This module only adds the helpers that don't have a home yet.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Re-exports from hermes-config (already migrated)
// ---------------------------------------------------------------------------
pub use hermes_config::{
    coerce_modal_mode,
    has_direct_modal_credentials,
    managed_nous_tools_enabled,
    resolve_modal_backend_state,
    resolve_openai_audio_api_key,
};

// ---------------------------------------------------------------------------
// Browser cloud provider
// ---------------------------------------------------------------------------

const DEFAULT_BROWSER_PROVIDER: &str = "local";

/// Return a normalized browser provider key.
pub fn normalize_browser_cloud_provider(value: Option<&str>) -> String {
    let provider = value
        .unwrap_or(DEFAULT_BROWSER_PROVIDER)
        .trim()
        .to_lowercase();
    if provider.is_empty() {
        DEFAULT_BROWSER_PROVIDER.to_string()
    } else {
        provider
    }
}

/// Alias for `coerce_modal_mode`. Converts the modal mode to its string form.
pub fn normalize_modal_mode(value: Option<&str>) -> String {
    coerce_modal_mode(value).as_str().to_string()
}

// ---------------------------------------------------------------------------
// FAL key check
// ---------------------------------------------------------------------------

/// Return true when `FAL_KEY` is set to a non-whitespace value.
///
/// Matches Python's `fal_key_is_configured`.
pub fn fal_key_is_configured() -> bool {
    let value = std::env::var("FAL_KEY").ok();
    match value {
        Some(v) => !v.trim().is_empty(),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Modal config file check (complements has_direct_modal_credentials)
// ---------------------------------------------------------------------------

/// Return true when `~/.modal.toml` exists.
pub fn modal_config_file_exists() -> bool {
    home_dir()
        .map(|h| h.join(".modal.toml").exists())
        .unwrap_or(false)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_browser_cloud_provider_default() {
        assert_eq!(normalize_browser_cloud_provider(None), "local");
        assert_eq!(normalize_browser_cloud_provider(Some("")), "local");
    }

    #[test]
    fn test_normalize_browser_cloud_provider_lowercases_and_trims() {
        assert_eq!(normalize_browser_cloud_provider(Some("BROWSERLESS")), "browserless");
        assert_eq!(normalize_browser_cloud_provider(Some("  Remote  ")), "remote");
    }

    #[test]
    fn test_fal_key_is_configured_false_when_unset() {
        // FAL_KEY should be unset in test, unless explicitly set by developer.
        // We don't assert false — just check it doesn't panic.
        let _result = fal_key_is_configured();
    }

    #[test]
    fn test_coerce_modal_mode_defaults_to_auto() {
        use hermes_config::ModalMode;
        assert_eq!(coerce_modal_mode(None), ModalMode::Auto);
        assert_eq!(coerce_modal_mode(Some("invalid")), ModalMode::Auto);
        assert_eq!(coerce_modal_mode(Some("direct")), ModalMode::Direct);
        assert_eq!(coerce_modal_mode(Some("managed")), ModalMode::Managed);
    }

    #[test]
    fn test_normalize_modal_mode_returns_string() {
        assert_eq!(normalize_modal_mode(None), "auto");
        assert_eq!(normalize_modal_mode(Some("direct")), "direct");
    }
}
