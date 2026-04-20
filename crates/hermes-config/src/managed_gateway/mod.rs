//! Managed-tool-gateway resolver layer ported from Python's
//! `tools/managed_tool_gateway.py` and `tools/tool_backend_helpers.py`.
//!
//! Hermes ships with a "managed mode" where vendor calls (Firecrawl,
//! Modal, browser-use, OpenAI audio, fal-queue, ...) are proxied through
//! Nous-hosted gateways using a Nous OAuth subscriber token. The Python
//! reference exposes:
//!
//! * a feature flag (`HERMES_ENABLE_NOUS_MANAGED_TOOLS`)
//! * a token reader that prefers `TOOL_GATEWAY_USER_TOKEN`, falls back to
//!   `auth.json` (and refreshes via the Nous OAuth flow when expiring)
//! * a per-vendor gateway URL builder with three precedence levels
//! * helpers for direct-vs-managed backend selection (used by Modal)
//!
//! This module mirrors that surface in Rust so that any tool/environment
//! backend can call [`resolve_managed_tool_gateway`] (or
//! [`is_managed_tool_gateway_ready`]) and decide which transport to use,
//! without each backend re-implementing env-var parsing and JSON probing.
//!
//! Lives in `hermes-config` (not `hermes-tools`) so that lower-level
//! crates such as `hermes-environments` can also depend on it without
//! pulling in the full tool registry.
//!
//! Sub-modules:
//! - [`config`]    — `ManagedToolGatewayConfig` + URL building
//! - [`auth`]      — Nous access token reader (env + `auth.json` + skew)
//! - [`selection`] — backend selection enums & truthy env helpers
//! - [`resolver`]  — top-level `resolve_managed_tool_gateway` / `is_managed_tool_gateway_ready`

pub mod auth;
pub mod config;
pub mod resolver;
pub mod selection;

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_lock {
    //! A single workspace-wide mutex for env-var-touching tests across all
    //! `managed_gateway` consumers. Without this, two tests in different
    //! crates can race on `HERMES_HOME` / `HERMES_ENABLE_NOUS_MANAGED_TOOLS`
    //! and dead-lock the test binary.
    //!
    //! Exposed (publicly) behind the `test-helpers` feature so downstream
    //! crates' tests can serialise on the same lock.
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    pub fn lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub use auth::{read_nous_access_token, NousProviderState, TokenReader};
pub use config::{
    build_vendor_gateway_url, get_tool_gateway_scheme, GatewayBuilder, GatewaySchemeError,
    ManagedToolGatewayConfig, DEFAULT_TOOL_GATEWAY_DOMAIN,
};
pub use resolver::{is_managed_tool_gateway_ready, resolve_managed_tool_gateway, ResolveOptions};
pub use selection::{
    coerce_modal_mode, env_var_enabled, has_direct_modal_credentials, managed_nous_tools_enabled,
    resolve_modal_backend_state, resolve_openai_audio_api_key, ModalBackendState, ModalMode,
    SelectedBackend,
};
