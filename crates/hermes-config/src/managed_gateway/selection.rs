//! Backend selection helpers ported from `tools/tool_backend_helpers.py`
//! and `utils.py::env_var_enabled` / `is_truthy_value`.

use std::path::PathBuf;

/// Truthy strings recognised by Python's `is_truthy_value`. Kept as a
/// lowercase compile-time array so we can also drive doctests.
const TRUTHY: &[&str] = &["1", "true", "yes", "on"];

/// Mirror of `utils.is_truthy_value` (only the string branch matters here).
pub fn is_truthy_str(value: &str) -> bool {
    let trimmed = value.trim().to_ascii_lowercase();
    TRUTHY.iter().any(|t| *t == trimmed)
}

/// Mirror of Python's `env_var_enabled(name)` — true iff the named env var
/// is set to a truthy value. Defaults to `false` when unset/empty.
pub fn env_var_enabled(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| is_truthy_str(&v))
        .unwrap_or(false)
}

/// Hidden feature flag for the Nous-managed tools rollout.
pub fn managed_nous_tools_enabled() -> bool {
    env_var_enabled("HERMES_ENABLE_NOUS_MANAGED_TOOLS")
}

// ---------------------------------------------------------------------------
// Modal mode
// ---------------------------------------------------------------------------

/// Strongly-typed mirror of Python's modal_mode strings (`"auto"`,
/// `"direct"`, `"managed"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalMode {
    Auto,
    Direct,
    Managed,
}

impl ModalMode {
    /// Coerce an arbitrary user-supplied value into a [`ModalMode`].
    /// Mirrors Python's `coerce_modal_mode` — unknown values fall back to
    /// `Auto`.
    pub fn from_str_or_default(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("direct") => Self::Direct,
            Some("managed") => Self::Managed,
            _ => Self::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Direct => "direct",
            Self::Managed => "managed",
        }
    }
}

/// Free-function alias kept for ergonomic parity with Python's
/// `coerce_modal_mode(value)`.
pub fn coerce_modal_mode(value: Option<&str>) -> ModalMode {
    ModalMode::from_str_or_default(value)
}

/// Resolved backend selection: `Some("managed" | "direct")` when usable,
/// or `None` when neither path is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedBackend {
    Managed,
    Direct,
}

impl SelectedBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::Direct => "direct",
        }
    }
}

/// Output of [`resolve_modal_backend_state`]. Carries diagnostic flags so
/// callers can produce the same hint messages Python produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModalBackendState {
    pub requested_mode: ModalMode,
    pub mode: ModalMode,
    pub has_direct: bool,
    pub managed_ready: bool,
    /// True iff the user explicitly asked for `"managed"` but the feature
    /// flag is off. Used to surface a helpful error rather than silently
    /// falling back.
    pub managed_mode_blocked: bool,
    pub selected_backend: Option<SelectedBackend>,
}

/// Direct vs managed Modal backend selection logic.
///
/// Faithful port of Python's `resolve_modal_backend_state`. Semantics:
///
/// * `Direct`  → direct-only (no managed fallback)
/// * `Managed` → managed-only (no direct fallback)
/// * `Auto`    → prefer managed (when feature flag on AND managed_ready),
///   else direct (when has_direct), else `None`
pub fn resolve_modal_backend_state(
    modal_mode: Option<&str>,
    has_direct: bool,
    managed_ready: bool,
) -> ModalBackendState {
    let requested_mode = ModalMode::from_str_or_default(modal_mode);
    let normalized_mode = requested_mode;
    let managed_enabled = managed_nous_tools_enabled();
    let managed_mode_blocked = matches!(requested_mode, ModalMode::Managed) && !managed_enabled;

    let selected_backend = match normalized_mode {
        ModalMode::Managed => {
            if managed_enabled && managed_ready {
                Some(SelectedBackend::Managed)
            } else {
                None
            }
        }
        ModalMode::Direct => {
            if has_direct {
                Some(SelectedBackend::Direct)
            } else {
                None
            }
        }
        ModalMode::Auto => {
            if managed_enabled && managed_ready {
                Some(SelectedBackend::Managed)
            } else if has_direct {
                Some(SelectedBackend::Direct)
            } else {
                None
            }
        }
    };

    ModalBackendState {
        requested_mode,
        mode: normalized_mode,
        has_direct,
        managed_ready,
        managed_mode_blocked,
        selected_backend,
    }
}

// ---------------------------------------------------------------------------
// Direct credentials
// ---------------------------------------------------------------------------

/// Mirror of Python's `has_direct_modal_credentials()`. True iff:
///
/// * `MODAL_TOKEN_ID` AND `MODAL_TOKEN_SECRET` are both set, OR
/// * `~/.modal.toml` exists.
pub fn has_direct_modal_credentials() -> bool {
    let id = std::env::var("MODAL_TOKEN_ID")
        .ok()
        .filter(|s| !s.is_empty());
    let secret = std::env::var("MODAL_TOKEN_SECRET")
        .ok()
        .filter(|s| !s.is_empty());
    if id.is_some() && secret.is_some() {
        return true;
    }
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .map(PathBuf::from);
    if let Some(home) = home {
        if home.join(".modal.toml").exists() {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// OpenAI audio key fallback
// ---------------------------------------------------------------------------

/// Prefer the dedicated voice-tools key; then `HERMES_OPENAI_API_KEY`; then
/// legacy `OPENAI_API_KEY`.
pub fn resolve_openai_audio_api_key() -> String {
    let voice = std::env::var("VOICE_TOOLS_OPENAI_KEY").unwrap_or_default();
    if !voice.trim().is_empty() {
        return voice.trim().to_string();
    }
    std::env::var("HERMES_OPENAI_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .unwrap_or_default()
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_gateway::test_lock;

    struct EnvGuard {
        keys: Vec<&'static str>,
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let g = test_lock::lock();
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in keys {
                std::env::remove_var(k);
            }
            Self {
                keys: keys.to_vec(),
                original,
                _g: g,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.original {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
            let _ = &self.keys;
        }
    }

    #[test]
    fn truthy_string_table() {
        for v in ["1", "true", "TRUE", "Yes", "on", "  yes  "] {
            assert!(is_truthy_str(v), "{v:?} should be truthy");
        }
        for v in ["0", "false", "no", "off", "", "anything-else"] {
            assert!(!is_truthy_str(v), "{v:?} should not be truthy");
        }
    }

    #[test]
    fn modal_mode_coerce() {
        assert_eq!(coerce_modal_mode(None), ModalMode::Auto);
        assert_eq!(coerce_modal_mode(Some("direct")), ModalMode::Direct);
        assert_eq!(coerce_modal_mode(Some("MANAGED")), ModalMode::Managed);
        assert_eq!(coerce_modal_mode(Some("auto")), ModalMode::Auto);
        assert_eq!(coerce_modal_mode(Some("garbage")), ModalMode::Auto);
        assert_eq!(coerce_modal_mode(Some("  managed  ")), ModalMode::Managed);
    }

    #[test]
    fn resolve_modal_backend_state_truth_table() {
        let _g = EnvGuard::new(&["HERMES_ENABLE_NOUS_MANAGED_TOOLS"]);

        std::env::remove_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS");
        let s = resolve_modal_backend_state(Some("auto"), true, true);
        assert_eq!(s.selected_backend, Some(SelectedBackend::Direct));
        assert!(!s.managed_mode_blocked);

        let s = resolve_modal_backend_state(Some("managed"), true, true);
        assert_eq!(s.selected_backend, None);
        assert!(s.managed_mode_blocked);

        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        let s = resolve_modal_backend_state(Some("managed"), false, true);
        assert_eq!(s.selected_backend, Some(SelectedBackend::Managed));
        assert!(!s.managed_mode_blocked);

        let s = resolve_modal_backend_state(Some("managed"), true, false);
        assert_eq!(s.selected_backend, None);

        let s = resolve_modal_backend_state(Some("auto"), true, false);
        assert_eq!(s.selected_backend, Some(SelectedBackend::Direct));

        let s = resolve_modal_backend_state(Some("direct"), false, true);
        assert_eq!(s.selected_backend, None);

        let s = resolve_modal_backend_state(None, false, true);
        assert_eq!(s.selected_backend, Some(SelectedBackend::Managed));
    }

    #[test]
    fn env_flag_helpers_round_trip() {
        let _g = EnvGuard::new(&["HERMES_ENABLE_NOUS_MANAGED_TOOLS"]);

        std::env::remove_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS");
        assert!(!managed_nous_tools_enabled());

        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "yes");
        assert!(managed_nous_tools_enabled());

        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "no");
        assert!(!managed_nous_tools_enabled());
    }

    #[test]
    fn openai_audio_key_prefers_voice_override() {
        let _g = EnvGuard::new(&[
            "VOICE_TOOLS_OPENAI_KEY",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
        ]);

        std::env::set_var("VOICE_TOOLS_OPENAI_KEY", "voice-key");
        std::env::set_var("HERMES_OPENAI_API_KEY", "hermes-key");
        std::env::set_var("OPENAI_API_KEY", "main-key");
        assert_eq!(resolve_openai_audio_api_key(), "voice-key");

        std::env::remove_var("VOICE_TOOLS_OPENAI_KEY");
        assert_eq!(resolve_openai_audio_api_key(), "hermes-key");

        std::env::remove_var("HERMES_OPENAI_API_KEY");
        assert_eq!(resolve_openai_audio_api_key(), "main-key");

        std::env::remove_var("OPENAI_API_KEY");
        assert_eq!(resolve_openai_audio_api_key(), "");
    }
}
