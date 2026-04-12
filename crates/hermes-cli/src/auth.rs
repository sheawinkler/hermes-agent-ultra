use hermes_core::AgentError;

pub async fn login(provider: &str) -> Result<String, AgentError> {
    Ok(format!("Login flow initialized for '{}'", provider))
}

pub async fn logout(provider: &str) -> Result<String, AgentError> {
    Ok(format!("Logged out from '{}'", provider))
}
