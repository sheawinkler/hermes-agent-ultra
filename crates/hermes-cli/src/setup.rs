use hermes_core::AgentError;

pub fn write_default_profile(config_dir: &std::path::Path, model: &str) -> Result<(), AgentError> {
    let profiles_dir = config_dir.join("profiles");
    std::fs::create_dir_all(&profiles_dir)
        .map_err(|e| AgentError::Io(format!("Failed to create profiles dir: {}", e)))?;
    let default_profile = profiles_dir.join("default.yaml");
    if !default_profile.exists() {
        std::fs::write(&default_profile, format!("name: default\nmodel: {}\n", model))
            .map_err(|e| AgentError::Io(format!("Failed to write default profile: {}", e)))?;
    }
    Ok(())
}
