use hermes_core::AgentError;

/// Human-readable line after a successful non-OAuth LLM login (API key stored in token store).
pub async fn login(provider: &str) -> Result<String, AgentError> {
    Ok(format!(
        "LLM API key stored for provider '{}'.",
        provider.trim()
    ))
}

pub async fn logout(provider: &str) -> Result<String, AgentError> {
    Ok(format!(
        "Removed stored credential for provider '{}'.",
        provider.trim()
    ))
}
