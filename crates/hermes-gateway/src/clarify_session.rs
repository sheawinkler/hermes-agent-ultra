//! Per-gateway-route session key for channel `clarify` registration.
//!
//! Tool execution runs inside [`with_gateway_clarify_session`], so
//! [`ChannelClarifyBackend`] can associate a pending clarify with the active
//! IM session without a global mutable "current session" slot.

tokio::task_local! {
    static GATEWAY_CLARIFY_SESSION_KEY: String;
}

/// Run `fut` with `session_key` visible to channel clarify registration.
pub async fn with_gateway_clarify_session<F, T>(session_key: &str, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    GATEWAY_CLARIFY_SESSION_KEY
        .scope(session_key.to_string(), fut)
        .await
}

/// Session key for the gateway route currently executing agent tools.
pub fn current_gateway_clarify_session() -> Option<String> {
    GATEWAY_CLARIFY_SESSION_KEY.try_with(|k| k.clone()).ok()
}
