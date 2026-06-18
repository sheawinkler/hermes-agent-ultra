//! Best-effort A-share symbol extraction from a user message (name/code in prose).

/// If `message` contains a resolvable A-share name or code, return canonical symbol.
#[cfg(feature = "trading-research")]
pub async fn try_resolve_a_share_from_user_message(message: &str) -> Option<String> {
    let msg = message.trim();
    if msg.is_empty() {
        return None;
    }
    hermes_trading::providers::akshare::resolve_a_share_symbol(msg)
        .await
        .ok()
}

#[cfg(not(feature = "trading-research"))]
pub async fn try_resolve_a_share_from_user_message(_message: &str) -> Option<String> {
    None
}
