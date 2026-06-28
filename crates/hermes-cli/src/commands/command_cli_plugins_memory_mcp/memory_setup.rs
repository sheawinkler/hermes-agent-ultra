fn prompt_memory_setup_value(
    label: &str,
    default: Option<&str>,
    yes: bool,
) -> Result<String, AgentError> {
    if yes {
        return Ok(default.unwrap_or_default().to_string());
    }
    match default {
        Some(value) if !value.is_empty() && memory_setup_label_is_secret(label) => {
            print!("{label} [set]: ");
        }
        Some(value) if !value.is_empty() => {
            print!("{label} [{value}]: ");
        }
        _ => {
            print!("{label}: ");
        }
    }
    std::io::stdout()
        .flush()
        .map_err(|e| AgentError::Io(format!("flush stdout: {e}")))?;
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| AgentError::Io(format!("read setup input: {e}")))?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default.unwrap_or_default().to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn memory_setup_label_is_secret(label: &str) -> bool {
    let lower = label.to_ascii_lowercase();
    lower.contains("api key")
        || lower.contains("jwt")
        || lower.contains("token")
        || lower.contains("secret")
}

fn active_honcho_host_key_for_cli() -> String {
    if let Ok(explicit) = std::env::var("HERMES_HONCHO_HOST") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return explicit.to_string();
        }
    }
    let profile = std::env::var("HERMES_PROFILE").unwrap_or_default();
    let profile = profile.trim();
    if profile.is_empty() || matches!(profile, "default" | "custom") {
        "hermes".to_string()
    } else {
        let sanitized = profile
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .to_string();
        format!(
            "hermes_{}",
            if sanitized.is_empty() {
                "profile"
            } else {
                sanitized.as_str()
            }
        )
    }
}

fn legacy_honcho_host_key_for_cli(host: &str) -> Option<String> {
    let suffix = host.strip_prefix("hermes_")?;
    if suffix.trim().is_empty() {
        None
    } else {
        Some(format!("hermes.{suffix}"))
    }
}

fn honcho_host_value_has_oauth_grant(block: &serde_json::Value) -> bool {
    let Some(api_key) = block.get("apiKey").and_then(serde_json::Value::as_str) else {
        return false;
    };
    if !api_key.starts_with("hch-at-") {
        return false;
    }
    let Some(oauth) = block.get("oauth").and_then(serde_json::Value::as_object) else {
        return false;
    };
    ["refreshToken", "clientId", "tokenEndpoint"]
        .iter()
        .all(|key| {
            oauth
                .get(*key)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
}

fn honcho_config_has_oauth_grant(path: &Path, host: &str) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(hosts) = parsed.get("hosts").and_then(serde_json::Value::as_object) else {
        return honcho_host_value_has_oauth_grant(&parsed);
    };
    hosts
        .get(host)
        .or_else(|| {
            legacy_honcho_host_key_for_cli(host)
                .as_deref()
                .and_then(|legacy| hosts.get(legacy))
        })
        .is_some_and(honcho_host_value_has_oauth_grant)
}

fn honcho_ai_peer_for_host(host: &str) -> String {
    host.strip_prefix("hermes.")
        .or_else(|| host.strip_prefix("hermes_"))
        .filter(|profile| !profile.trim().is_empty())
        .unwrap_or(host)
        .to_string()
}

fn parse_honcho_aliases(raw: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut aliases = serde_json::Map::new();
    for entry in raw.split(',') {
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if !key.is_empty() && !value.is_empty() {
            aliases.insert(
                key.to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }
    aliases
}

struct HonchoSetupConfigInput<'a> {
    host: &'a str,
    deployment: &'a str,
    api_key: &'a str,
    base_url: &'a str,
    peer_name: &'a str,
    shape: &'a str,
    runtime_peer_prefix: &'a str,
    aliases: &'a serde_json::Map<String, serde_json::Value>,
}

fn build_honcho_setup_config(input: HonchoSetupConfigInput<'_>) -> serde_json::Value {
    let mut root = serde_json::Map::new();
    let mut host_block = serde_json::Map::new();

    root.insert("enabled".to_string(), serde_json::Value::Bool(true));
    if input.deployment == "local" {
        root.insert(
            "baseUrl".to_string(),
            serde_json::Value::String(input.base_url.to_string()),
        );
        if !input.api_key.trim().is_empty() {
            host_block.insert(
                "apiKey".to_string(),
                serde_json::Value::String(input.api_key.to_string()),
            );
        }
    } else if !input.api_key.trim().is_empty() {
        root.insert(
            "apiKey".to_string(),
            serde_json::Value::String(input.api_key.to_string()),
        );
    }

    host_block.insert("enabled".to_string(), serde_json::Value::Bool(true));
    host_block.insert(
        "workspace".to_string(),
        serde_json::Value::String("hermes".to_string()),
    );
    host_block.insert(
        "aiPeer".to_string(),
        serde_json::Value::String(honcho_ai_peer_for_host(input.host)),
    );
    if !input.peer_name.trim().is_empty() {
        host_block.insert(
            "peerName".to_string(),
            serde_json::Value::String(input.peer_name.to_string()),
        );
    }

    match input.shape {
        "single" => {
            host_block.insert("pinUserPeer".to_string(), serde_json::Value::Bool(true));
        }
        "hybrid" => {
            host_block.insert("pinUserPeer".to_string(), serde_json::Value::Bool(false));
            if !input.aliases.is_empty() {
                host_block.insert(
                    "userPeerAliases".to_string(),
                    serde_json::Value::Object(input.aliases.clone()),
                );
            }
            if !input.runtime_peer_prefix.trim().is_empty() {
                host_block.insert(
                    "runtimePeerPrefix".to_string(),
                    serde_json::Value::String(input.runtime_peer_prefix.to_string()),
                );
            }
        }
        _ => {
            host_block.insert("pinUserPeer".to_string(), serde_json::Value::Bool(false));
            if !input.runtime_peer_prefix.trim().is_empty() {
                host_block.insert(
                    "runtimePeerPrefix".to_string(),
                    serde_json::Value::String(input.runtime_peer_prefix.to_string()),
                );
            }
        }
    }

    let mut hosts = serde_json::Map::new();
    hosts.insert(
        input.host.to_string(),
        serde_json::Value::Object(host_block),
    );
    root.insert("hosts".to_string(), serde_json::Value::Object(hosts));
    serde_json::Value::Object(root)
}

fn setup_mem0_provider(yes: bool) -> Result<PathBuf, AgentError> {
    let api_key_default = std::env::var("MEM0_API_KEY").unwrap_or_default();
    let user_id_default =
        std::env::var("MEM0_USER_ID").unwrap_or_else(|_| "hermes-user".to_string());
    let agent_id_default = std::env::var("MEM0_AGENT_ID").unwrap_or_else(|_| "hermes".to_string());
    let base_url_default =
        std::env::var("MEM0_BASE_URL").unwrap_or_else(|_| "https://api.mem0.ai/v1".to_string());

    let api_key = prompt_memory_setup_value("Mem0 API key", Some(&api_key_default), yes)?;
    if api_key.trim().is_empty() {
        return Err(AgentError::Config(
            "Mem0 setup requires MEM0_API_KEY or an API key entered at the prompt.".into(),
        ));
    }
    let user_id = prompt_memory_setup_value("Mem0 user_id", Some(&user_id_default), yes)?;
    let agent_id = prompt_memory_setup_value("Mem0 agent_id", Some(&agent_id_default), yes)?;
    let base_url = prompt_memory_setup_value("Mem0 base_url", Some(&base_url_default), yes)?;

    let config = serde_json::json!({
        "api_key": api_key,
        "user_id": user_id,
        "agent_id": agent_id,
        "base_url": base_url,
        "rerank": true
    });
    hermes_agent::memory_plugins::mem0::Mem0MemoryPlugin::new()
        .save_config(&config)
        .map_err(AgentError::Config)?;
    Ok(hermes_config::hermes_home().join("mem0.json"))
}

fn setup_honcho_provider(yes: bool) -> Result<PathBuf, AgentError> {
    let env_api_key = std::env::var("HONCHO_API_KEY").unwrap_or_default();
    let env_base_url = std::env::var("HONCHO_BASE_URL").unwrap_or_default();
    let default_deployment =
        if env_base_url.trim().is_empty() || env_base_url.contains("api.honcho.dev") {
            "cloud"
        } else {
            "local"
        };
    let deployment = prompt_memory_setup_value(
        "Honcho deployment (cloud|local)",
        Some(default_deployment),
        yes,
    )?
    .to_ascii_lowercase();
    let deployment = if deployment == "local" {
        "local"
    } else {
        "cloud"
    };
    let host = active_honcho_host_key_for_cli();
    let existing_oauth_grant =
        honcho_config_has_oauth_grant(&hermes_config::hermes_home().join("honcho.json"), &host);

    let base_url_default = if deployment == "local" {
        if env_base_url.trim().is_empty() {
            "http://localhost:8000".to_string()
        } else {
            env_base_url.clone()
        }
    } else {
        env_base_url.clone()
    };
    let base_url = if deployment == "local" {
        prompt_memory_setup_value("Honcho local baseUrl", Some(&base_url_default), yes)?
    } else {
        base_url_default
    };
    let api_label = if deployment == "local" {
        "Honcho local JWT/API key (blank for no-auth local)"
    } else {
        "Honcho API key"
    };
    let api_key = prompt_memory_setup_value(api_label, Some(&env_api_key), yes)?;
    if deployment == "cloud" && api_key.trim().is_empty() && !existing_oauth_grant {
        return Err(AgentError::Config(
            "Honcho cloud setup requires HONCHO_API_KEY or an API key entered at the prompt."
                .into(),
        ));
    }

    let peer_default = std::env::var("HERMES_USER").unwrap_or_default();
    let peer_name = prompt_memory_setup_value("Honcho peerName", Some(&peer_default), yes)?;
    let shape_input = prompt_memory_setup_value(
        "Deployment shape (single|multi|hybrid)",
        Some("single"),
        yes,
    )?
    .to_ascii_lowercase();
    let shape = match shape_input.as_str() {
        "single" | "hybrid" => shape_input,
        _ => "multi".to_string(),
    };
    let runtime_peer_prefix = if shape == "multi" || shape == "hybrid" {
        prompt_memory_setup_value("Runtime peer prefix", Some(""), yes)?
    } else {
        String::new()
    };
    let alias_raw = if shape == "hybrid" {
        prompt_memory_setup_value(
            "Runtime aliases (comma key=peer, blank for none)",
            Some(""),
            yes,
        )?
    } else {
        String::new()
    };
    let aliases = parse_honcho_aliases(&alias_raw);
    let config = build_honcho_setup_config(HonchoSetupConfigInput {
        host: &host,
        deployment,
        api_key: &api_key,
        base_url: &base_url,
        peer_name: &peer_name,
        shape: &shape,
        runtime_peer_prefix: &runtime_peer_prefix,
        aliases: &aliases,
    });

    hermes_agent::memory_plugins::honcho::HonchoMemoryPlugin::new()
        .save_config(&config)
        .map_err(AgentError::Config)?;
    Ok(hermes_config::hermes_home().join("honcho.json"))
}

fn normalize_openviking_setup_endpoint(raw: &str) -> String {
    let trimmed = raw.trim();
    let endpoint = if trimmed.is_empty() {
        "http://127.0.0.1:1933".to_string()
    } else if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    endpoint.trim_end_matches('/').to_string()
}

fn openviking_setup_endpoint_is_local(endpoint: &str) -> bool {
    endpoint.starts_with("http://127.0.0.1:")
        || endpoint.starts_with("http://localhost:")
        || endpoint == "http://127.0.0.1"
        || endpoint == "http://localhost"
}

fn normalize_openviking_setup_key_type(raw: &str, endpoint: &str, api_key: &str) -> String {
    let normalized = match raw.trim().to_ascii_lowercase().as_str() {
        "root" | "root_api_key" | "root-api-key" => "root",
        "none" | "dev" | "local" | "no_api_key" | "no-api-key" => "none",
        "user" | "user_api_key" | "user-api-key" => "user",
        "" if openviking_setup_endpoint_is_local(endpoint) && api_key.trim().is_empty() => "none",
        _ => "user",
    };
    normalized.to_string()
}

struct OpenVikingSetupConfigInput<'a> {
    endpoint: &'a str,
    api_key: &'a str,
    api_key_type: &'a str,
    account: &'a str,
    user: &'a str,
    agent: &'a str,
}

fn build_openviking_setup_config(
    input: OpenVikingSetupConfigInput<'_>,
) -> Result<serde_json::Value, AgentError> {
    let endpoint = normalize_openviking_setup_endpoint(input.endpoint);
    let api_key_type =
        normalize_openviking_setup_key_type(input.api_key_type, &endpoint, input.api_key);
    let api_key = input.api_key.trim();
    if api_key_type != "none" && api_key.is_empty() {
        return Err(AgentError::Config(format!(
            "OpenViking {api_key_type} setup requires an API key."
        )));
    }
    let account = input.account.trim();
    let user = input.user.trim();
    if api_key_type == "root" && (account.is_empty() || user.is_empty()) {
        return Err(AgentError::Config(
            "OpenViking root API key setup requires account and user.".into(),
        ));
    }
    let account = if account.is_empty() {
        "default"
    } else {
        account
    };
    let user = if user.is_empty() { "default" } else { user };
    let agent = if input.agent.trim().is_empty() {
        "hermes"
    } else {
        input.agent.trim()
    };

    Ok(serde_json::json!({
        "enabled": true,
        "endpoint": endpoint,
        "api_key": api_key,
        "api_key_type": api_key_type,
        "account": account,
        "user": user,
        "agent": agent,
        "setup_mode": "manual"
    }))
}

fn setup_openviking_provider(yes: bool) -> Result<PathBuf, AgentError> {
    let endpoint_default = std::env::var("OPENVIKING_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:1933".to_string());
    let endpoint = normalize_openviking_setup_endpoint(&prompt_memory_setup_value(
        "OpenViking server URL",
        Some(&endpoint_default),
        yes,
    )?);
    let api_key_default = std::env::var("OPENVIKING_API_KEY").unwrap_or_default();
    let key_type_default = std::env::var("OPENVIKING_API_KEY_TYPE").unwrap_or_else(|_| {
        if openviking_setup_endpoint_is_local(&endpoint) && api_key_default.trim().is_empty() {
            "none".to_string()
        } else {
            "user".to_string()
        }
    });
    let key_type = normalize_openviking_setup_key_type(
        &prompt_memory_setup_value(
            "OpenViking API key type (none|user|root)",
            Some(&key_type_default),
            yes,
        )?,
        &endpoint,
        &api_key_default,
    );
    let api_key = if key_type == "none" {
        String::new()
    } else {
        let label = if key_type == "root" {
            "OpenViking root API key"
        } else {
            "OpenViking user API key"
        };
        prompt_memory_setup_value(label, Some(&api_key_default), yes)?
    };
    let account_default = std::env::var("OPENVIKING_ACCOUNT").unwrap_or_else(|_| "default".into());
    let user_default = std::env::var("OPENVIKING_USER").unwrap_or_else(|_| "default".into());
    let account = if key_type == "root" || key_type == "none" {
        prompt_memory_setup_value("OpenViking account", Some(&account_default), yes)?
    } else {
        account_default
    };
    let user = if key_type == "root" || key_type == "none" {
        prompt_memory_setup_value("OpenViking user", Some(&user_default), yes)?
    } else {
        user_default
    };
    let agent_default = std::env::var("OPENVIKING_AGENT").unwrap_or_else(|_| "hermes".into());
    let agent = prompt_memory_setup_value("OpenViking agent", Some(&agent_default), yes)?;
    let config = build_openviking_setup_config(OpenVikingSetupConfigInput {
        endpoint: &endpoint,
        api_key: &api_key,
        api_key_type: &key_type,
        account: &account,
        user: &user,
        agent: &agent,
    })?;

    hermes_agent::memory_plugins::openviking::OpenVikingMemoryPlugin::new()
        .save_config(&config)
        .map_err(AgentError::Config)?;
    Ok(hermes_config::hermes_home().join("openviking.json"))
}

fn setup_memory_provider_target(provider: &str, yes: bool) -> Result<PathBuf, AgentError> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "mem0" => setup_mem0_provider(yes),
        "honcho" => setup_honcho_provider(yes),
        "openviking" | "ov" => setup_openviking_provider(yes),
        other => Err(AgentError::Config(format!(
            "Unsupported memory provider setup target '{other}'. Supported: honcho, mem0, openviking"
        ))),
    }
}

