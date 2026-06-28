{
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills config <name> [key] [value]".into(),
                )
            })?;
            let config_file = skills_dir.join(&skill_name).join("config.json");
            if let Some(key) = extra {
                // Set or get a config key
                let parts: Vec<&str> = key.splitn(2, '=').collect();
                if parts.len() == 2 {
                    let mut config: serde_json::Value = if config_file.exists() {
                        let c = std::fs::read_to_string(&config_file)
                            .unwrap_or_else(|_| "{}".to_string());
                        serde_json::from_str(&c).unwrap_or(serde_json::json!({}))
                    } else {
                        serde_json::json!({})
                    };
                    config[parts[0]] = serde_json::Value::String(parts[1].to_string());
                    let json = serde_json::to_string_pretty(&config)
                        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
                    std::fs::write(&config_file, json)
                        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                    println!("Set {} = {} for skill '{}'", parts[0], parts[1], skill_name);
                } else {
                    // Get value
                    if config_file.exists() {
                        let c = std::fs::read_to_string(&config_file)
                            .unwrap_or_else(|_| "{}".to_string());
                        let config: serde_json::Value =
                            serde_json::from_str(&c).unwrap_or(serde_json::json!({}));
                        match config.get(&key) {
                            Some(v) => println!("{} = {}", key, v),
                            None => println!("Key '{}' not found in skill config.", key),
                        }
                    } else {
                        println!("No config for skill '{}'.", skill_name);
                    }
                }
            } else {
                // Show all config
                if config_file.exists() {
                    let content = std::fs::read_to_string(&config_file)
                        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                    println!("Config for skill '{}':", skill_name);
                    println!("{}", content);
                } else {
                    println!("No config for skill '{}'.", skill_name);
                }
            }
        }
