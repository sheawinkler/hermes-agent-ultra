use hermes_core::AgentError;

pub fn normalize_provider_model(input: &str) -> Result<String, AgentError> {
    if input.trim().is_empty() {
        return Err(AgentError::Config("Model cannot be empty".to_string()));
    }
    if input.contains(':') {
        Ok(input.to_string())
    } else {
        Ok(format!("openai:{}", input))
    }
}
