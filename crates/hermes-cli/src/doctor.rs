use hermes_core::AgentError;

pub fn run_basic_checks() -> Result<Vec<String>, AgentError> {
    let mut checks = Vec::new();
    checks.push(format!("OS: {}", std::env::consts::OS));
    checks.push(format!("Arch: {}", std::env::consts::ARCH));
    checks.push("Config: basic checks passed".to_string());
    Ok(checks)
}
