const CONFIG_PATCH_HELP: &str = "model, personality, max_turns, system_prompt, prefill_messages_file, model_switch.persist_switch_by_default, budget.max_result_size_chars, budget.max_aggregate_chars, proxy.http, proxy.socks, security.allow_private_urls, web.backend|search_backend|extract_backend|crawl_backend, display.busy_input_mode|busy_ack_enabled|memory_notifications, sessions.auto_prune|retention_days|vacuum_after_prune|min_interval_hours, kanban.dispatch_in_gateway, agent.api_max_retries, delegation.model|provider|base_url|api_key|max_spawn_depth, llm.<provider>.api_key|api_key_env|base_url|model|models|discover_models|api_mode|command|args|request_timeout_seconds|oauth_token_url|oauth_client_id, auxiliary.<task>.provider|model|base_url|api_key|timeout|download_timeout, smart_model_routing.enabled|max_simple_chars|max_simple_words|cheap_model.model|cheap_model.provider";

fn mask_secret(s: &str) -> String {
    if s.is_empty() {
        return "(empty)".to_string();
    }
    if s.len() <= 4 {
        "***".to_string()
    } else {
        format!("***{}", &s[s.len() - 4..])
    }
}

fn parse_config_bool(key: &str, value: &str) -> Result<bool, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::ValidationError(format!(
            "{key} must be a boolean: {value}"
        ))),
    }
}

fn parse_positive_timeout_seconds(key: &str, value: &str) -> Result<f64, ConfigError> {
    let parsed: f64 = value.parse().map_err(|_| {
        ConfigError::ValidationError(format!("{key} must be a positive finite number: {value}"))
    })?;
    if parsed.is_finite() && parsed > 0.0 {
        Ok(parsed)
    } else {
        Err(ConfigError::ValidationError(format!(
            "{key} must be a positive finite number: {value}"
        )))
    }
}

fn normalize_provider_api_mode(value: &str) -> Result<String, ConfigError> {
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "chat_completions"
        | "anthropic_messages"
        | "codex_responses"
        | "bedrock_converse" => Ok(normalized),
        _ => Err(ConfigError::ValidationError(format!(
            "llm provider api_mode must be one of chat_completions, anthropic_messages, codex_responses, bedrock_converse: {}",
            value
        ))),
    }
}

/// Apply a single scalar field used by `hermes config set` (does not touch other keys).
///
/// Supports dotted keys aligned with `GatewayConfig`:
/// - `budget.max_result_size_chars`, `budget.max_aggregate_chars`
/// - `proxy.http` / `proxy.http_proxy`, `proxy.socks` / `proxy.socks_proxy`
/// - `llm.<provider>.api_key`, `llm.<provider>.base_url`, `llm.<provider>.model`
/// - `llm.<provider>.command`, `llm.<provider>.args`
pub fn apply_user_config_patch(
    config: &mut GatewayConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    if !key.contains('.') {
        return apply_user_config_patch_flat(config, key, value);
    }
    apply_user_config_patch_dotted(config, key, value)
}

fn apply_user_config_patch_flat(
    config: &mut GatewayConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    match key {
        "model" => {
            config.model = Some(value.to_string());
        }
        "personality" => {
            config.personality = Some(value.to_string());
        }
        "max_turns" => {
            config.max_turns = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "max_turns must be a non-negative integer: {}",
                    value
                ))
            })?;
        }
        "system_prompt" => {
            config.system_prompt = Some(value.to_string());
        }
        "prefill_messages_file" => {
            config.prefill_messages_file = Some(value.to_string());
        }
        other => {
            return Err(ConfigError::NotFound(format!(
                "unknown config key: {} (supported: {})",
                other, CONFIG_PATCH_HELP
            )));
        }
    }
    Ok(())
}

fn apply_user_config_patch_dotted(
    config: &mut GatewayConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["budget", "max_result_size_chars"] => {
            config.budget.max_result_size_chars = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "budget.max_result_size_chars must be a usize: {}",
                    value
                ))
            })?;
        }
        ["budget", "max_aggregate_chars"] => {
            config.budget.max_aggregate_chars = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "budget.max_aggregate_chars must be a usize: {}",
                    value
                ))
            })?;
        }
        ["proxy", "http"] | ["proxy", "http_proxy"] => {
            let proxy = config.proxy.get_or_insert_with(ProxyConfig::default);
            proxy.http_proxy = Some(value.to_string());
        }
        ["proxy", "socks"] | ["proxy", "socks_proxy"] => {
            let proxy = config.proxy.get_or_insert_with(ProxyConfig::default);
            proxy.socks_proxy = Some(value.to_string());
        }
        ["security", "allow_private_urls"] => {
            let normalized = value.trim().to_ascii_lowercase();
            let parsed = match normalized.as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "security.allow_private_urls must be a boolean: {}",
                        value
                    )));
                }
            };
            config.security.allow_private_urls = parsed;
        }
        ["web", "backend"] => {
            config.web.backend = value.trim().to_string();
        }
        ["web", "search_backend"] => {
            config.web.search_backend = value.trim().to_string();
        }
        ["web", "extract_backend"] => {
            config.web.extract_backend = value.trim().to_string();
        }
        ["web", "crawl_backend"] => {
            config.web.crawl_backend = value.trim().to_string();
        }
        ["display", "busy_input_mode"] => {
            let normalized = match value.trim().to_ascii_lowercase().as_str() {
                "queue" | "queued" => "queue",
                "steer" | "steering" => "steer",
                "interrupt" | "interrupted" | "replace" | "" => "interrupt",
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "display.busy_input_mode must be one of interrupt, queue, steer: {value}"
                    )));
                }
            };
            config.display.busy_input_mode = Some(normalized.to_string());
        }
        ["display", "busy_ack_enabled"] => {
            config.display.busy_ack_enabled =
                Some(parse_config_bool("display.busy_ack_enabled", value)?);
        }
        ["display", "memory_notifications"] => {
            config.display.memory_notifications =
                Some(parse_config_bool("display.memory_notifications", value)?);
        }
        ["sessions", "auto_prune"] => {
            let normalized = value.trim().to_ascii_lowercase();
            let parsed = match normalized.as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "sessions.auto_prune must be a boolean: {}",
                        value
                    )));
                }
            };
            config.sessions.auto_prune = parsed;
        }
        ["sessions", "retention_days"] => {
            config.sessions.retention_days = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "sessions.retention_days must be a non-negative integer: {}",
                    value
                ))
            })?;
        }
        ["sessions", "vacuum_after_prune"] => {
            let normalized = value.trim().to_ascii_lowercase();
            let parsed = match normalized.as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "sessions.vacuum_after_prune must be a boolean: {}",
                        value
                    )));
                }
            };
            config.sessions.vacuum_after_prune = parsed;
        }
        ["sessions", "min_interval_hours"] => {
            config.sessions.min_interval_hours = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "sessions.min_interval_hours must be a non-negative integer: {}",
                    value
                ))
            })?;
        }
        ["kanban", "dispatch_in_gateway"] => {
            config.kanban.dispatch_in_gateway =
                parse_config_bool("kanban.dispatch_in_gateway", value)?;
        }
        ["model_switch", "persist_switch_by_default"] | ["model", "persist_switch_by_default"] => {
            config.model_switch.persist_switch_by_default =
                parse_config_bool("model_switch.persist_switch_by_default", value)?;
        }
        ["agent", "api_max_retries"] | ["agent", "apiMaxRetries"] => {
            config.agent.api_max_retries = Some(value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "agent.api_max_retries must be a non-negative integer: {value}"
                ))
            })?);
        }
        ["delegation", "model"] => {
            config.delegation.model = Some(value.to_string());
        }
        ["delegation", "provider"] => {
            config.delegation.provider = Some(value.to_string());
        }
        ["delegation", "base_url"] => {
            config.delegation.base_url = Some(value.to_string());
        }
        ["delegation", "api_key"] => {
            config.delegation.api_key = Some(value.to_string());
        }
        ["delegation", "max_spawn_depth"] => {
            config.delegation.max_spawn_depth = Some(value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "delegation.max_spawn_depth must be a non-negative integer: {value}"
                ))
            })?);
        }
        ["auxiliary", task, field] => {
            let entry = config
                .auxiliary
                .entry((*task).to_string())
                .or_insert_with(crate::config::AuxiliaryTaskConfig::default);
            match *field {
                "provider" => entry.provider = value.to_string(),
                "model" => entry.model = value.to_string(),
                "base_url" => entry.base_url = value.to_string(),
                "api_key" => entry.api_key = value.to_string(),
                "timeout" | "timeout_secs" => {
                    entry.timeout = Some(value.parse().map_err(|_| {
                        ConfigError::ValidationError(format!(
                            "auxiliary.{}.{} must be a non-negative integer: {}",
                            task, field, value
                        ))
                    })?);
                }
                "download_timeout" => {
                    entry.download_timeout = Some(value.parse().map_err(|_| {
                        ConfigError::ValidationError(format!(
                            "auxiliary.{}.download_timeout must be a non-negative integer: {}",
                            task, value
                        ))
                    })?);
                }
                other => {
                    return Err(ConfigError::NotFound(format!(
                        "unknown auxiliary field: auxiliary.{}.{} (supported: provider, model, base_url, api_key, timeout, download_timeout)",
                        task, other
                    )));
                }
            }
        }
        ["llm", provider, field] => {
            let entry = config
                .llm_providers
                .entry((*provider).to_string())
                .or_insert_with(LlmProviderConfig::default);
            match *field {
                "api_key" => entry.api_key = Some(value.to_string()),
                "api_key_env" => entry.api_key_env = Some(value.to_string()),
                "base_url" => entry.base_url = Some(value.to_string()),
                "model" => entry.model = Some(value.to_string()),
                "models" => {
                    entry.models = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                "discover_models" => {
                    entry.discover_models =
                        parse_config_bool(&format!("llm.{}.discover_models", provider), value)?;
                }
                "api_mode" => entry.api_mode = Some(normalize_provider_api_mode(value)?),
                "max_tokens" | "max_output_tokens" => {
                    let parsed = value.parse::<u32>().map_err(|_| {
                        ConfigError::ValidationError(format!(
                            "llm.{}.{} must be a positive integer: {}",
                            provider, field, value
                        ))
                    })?;
                    if parsed == 0 {
                        return Err(ConfigError::ValidationError(format!(
                            "llm.{}.{} must be a positive integer: {}",
                            provider, field, value
                        )));
                    }
                    entry.max_tokens = Some(parsed);
                }
                "command" => entry.command = Some(value.to_string()),
                "args" => {
                    entry.args = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                "request_timeout_seconds" => {
                    entry.request_timeout_seconds =
                        Some(parse_positive_timeout_seconds(key, value)?);
                }
                "oauth_token_url" => entry.oauth_token_url = Some(value.to_string()),
                "oauth_client_id" => entry.oauth_client_id = Some(value.to_string()),
                other => {
                    return Err(ConfigError::NotFound(format!(
                        "unknown llm field: llm.{}.{} (supported: api_key, api_key_env, base_url, model, models, discover_models, api_mode, max_tokens, max_output_tokens, command, args, request_timeout_seconds, oauth_token_url, oauth_client_id)",
                        provider, other
                    )));
                }
            }
        }
        ["smart_model_routing", "enabled"] => {
            let normalized = value.trim().to_ascii_lowercase();
            let parsed = match normalized.as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "smart_model_routing.enabled must be a boolean: {}",
                        value
                    )));
                }
            };
            config.smart_model_routing.enabled = parsed;
        }
        ["smart_model_routing", "max_simple_chars"] => {
            config.smart_model_routing.max_simple_chars = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "smart_model_routing.max_simple_chars must be a usize: {}",
                    value
                ))
            })?;
        }
        ["smart_model_routing", "max_simple_words"] => {
            config.smart_model_routing.max_simple_words = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "smart_model_routing.max_simple_words must be a usize: {}",
                    value
                ))
            })?;
        }
        ["smart_model_routing", "cheap_model", "model"] => {
            let cheap = config
                .smart_model_routing
                .cheap_model
                .get_or_insert_with(crate::CheapModelRouteConfig::default);
            cheap.model = Some(value.to_string());
        }
        ["smart_model_routing", "cheap_model", "provider"] => {
            let cheap = config
                .smart_model_routing
                .cheap_model
                .get_or_insert_with(crate::CheapModelRouteConfig::default);
            cheap.provider = Some(value.to_string());
        }
        _ => {
            return Err(ConfigError::NotFound(format!(
                "unknown config key: {} (supported: {})",
                key, CONFIG_PATCH_HELP
            )));
        }
    }
    Ok(())
}

/// Display a single config field for `hermes config get` (same keys as [`apply_user_config_patch`]).
pub fn user_config_field_display(config: &GatewayConfig, key: &str) -> Result<String, ConfigError> {
    if !key.contains('.') {
        return Ok(match key {
            "model" => config
                .model
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(not set)".to_string()),
            "personality" => config
                .personality
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(not set)".to_string()),
            "max_turns" => config.max_turns.to_string(),
            "system_prompt" => config
                .system_prompt
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(not set)".to_string()),
            "prefill_messages_file" => config
                .prefill_messages_file
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(not set)".to_string()),
            other => {
                return Err(ConfigError::NotFound(format!(
                    "unknown config key: {} (supported: {})",
                    other, CONFIG_PATCH_HELP
                )));
            }
        });
    }

    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["budget", "max_result_size_chars"] => Ok(config.budget.max_result_size_chars.to_string()),
        ["budget", "max_aggregate_chars"] => Ok(config.budget.max_aggregate_chars.to_string()),
        ["proxy", "http"] | ["proxy", "http_proxy"] => Ok(config
            .proxy
            .as_ref()
            .and_then(|p| p.http_proxy.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["proxy", "socks"] | ["proxy", "socks_proxy"] => Ok(config
            .proxy
            .as_ref()
            .and_then(|p| p.socks_proxy.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["web", "backend"] => Ok(if config.web.backend.trim().is_empty() {
            "(not set)".to_string()
        } else {
            config.web.backend.clone()
        }),
        ["web", "search_backend"] => Ok(if config.web.search_backend.trim().is_empty() {
            "(not set)".to_string()
        } else {
            config.web.search_backend.clone()
        }),
        ["web", "extract_backend"] => Ok(if config.web.extract_backend.trim().is_empty() {
            "(not set)".to_string()
        } else {
            config.web.extract_backend.clone()
        }),
        ["web", "crawl_backend"] => Ok(if config.web.crawl_backend.trim().is_empty() {
            "(not set)".to_string()
        } else {
            config.web.crawl_backend.clone()
        }),
        ["display", "busy_input_mode"] => Ok(config
            .display
            .busy_input_mode
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "interrupt".to_string())),
        ["display", "busy_ack_enabled"] => Ok(config.display.busy_ack_enabled().to_string()),
        ["display", "memory_notifications"] => {
            Ok(config.display.memory_notifications_enabled().to_string())
        }
        ["sessions", "auto_prune"] => Ok(config.sessions.auto_prune.to_string()),
        ["sessions", "retention_days"] => Ok(config.sessions.retention_days.to_string()),
        ["sessions", "vacuum_after_prune"] => Ok(config.sessions.vacuum_after_prune.to_string()),
        ["sessions", "min_interval_hours"] => Ok(config.sessions.min_interval_hours.to_string()),
        ["kanban", "dispatch_in_gateway"] => Ok(config.kanban.dispatch_in_gateway.to_string()),
        ["model_switch", "persist_switch_by_default"] | ["model", "persist_switch_by_default"] => {
            Ok(config.model_switch.persist_switch_by_default.to_string())
        }
        ["agent", "api_max_retries"] | ["agent", "apiMaxRetries"] => Ok(config
            .agent
            .api_max_retries
            .map(|value| value.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["delegation", "model"] => Ok(config
            .delegation
            .model
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["delegation", "provider"] => Ok(config
            .delegation
            .provider
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["delegation", "base_url"] => Ok(config
            .delegation
            .base_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["delegation", "api_key"] => Ok(config
            .delegation
            .api_key
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(mask_secret)
            .unwrap_or_else(|| "(not set)".to_string())),
        ["delegation", "max_spawn_depth"] => Ok(config
            .delegation
            .max_spawn_depth
            .map(|value| value.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "api_key"] => Ok(
            match config
                .llm_providers
                .get(*provider)
                .and_then(|c| c.api_key.as_deref())
                .filter(|s| !s.is_empty())
            {
                Some(s) => mask_secret(s),
                None => "(not set)".to_string(),
            },
        ),
        ["llm", provider, "base_url"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.base_url.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "model"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.model.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "models"] => Ok(config
            .llm_providers
            .get(*provider)
            .map(|c| c.models.join(","))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "discover_models"] => Ok(config
            .llm_providers
            .get(*provider)
            .map(|c| c.discover_models.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "api_mode"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.api_mode.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "max_tokens"] | ["llm", provider, "max_output_tokens"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.max_tokens)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "command"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.command.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "args"] => Ok(config
            .llm_providers
            .get(*provider)
            .map(|c| c.args.join(","))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "request_timeout_seconds"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.request_timeout_seconds)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "oauth_token_url"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.oauth_token_url.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "oauth_client_id"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.oauth_client_id.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["auxiliary", task, "provider"] => Ok(config
            .auxiliary
            .get(*task)
            .map(|c| c.provider.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "auto".to_string())),
        ["auxiliary", task, "model"] => Ok(config
            .auxiliary
            .get(*task)
            .map(|c| c.model.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["auxiliary", task, "base_url"] => Ok(config
            .auxiliary
            .get(*task)
            .map(|c| c.base_url.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["auxiliary", task, "api_key"] => Ok(config
            .auxiliary
            .get(*task)
            .map(|c| c.api_key.trim())
            .filter(|s| !s.is_empty())
            .map(mask_secret)
            .unwrap_or_else(|| "(not set)".to_string())),
        ["auxiliary", task, "timeout"] | ["auxiliary", task, "timeout_secs"] => Ok(config
            .auxiliary
            .get(*task)
            .and_then(|c| c.timeout)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["auxiliary", task, "download_timeout"] => Ok(config
            .auxiliary
            .get(*task)
            .and_then(|c| c.download_timeout)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["smart_model_routing", "enabled"] => Ok(config.smart_model_routing.enabled.to_string()),
        ["smart_model_routing", "max_simple_chars"] => {
            Ok(config.smart_model_routing.max_simple_chars.to_string())
        }
        ["smart_model_routing", "max_simple_words"] => {
            Ok(config.smart_model_routing.max_simple_words.to_string())
        }
        ["smart_model_routing", "cheap_model", "model"] => Ok(config
            .smart_model_routing
            .cheap_model
            .as_ref()
            .and_then(|c| c.model.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["smart_model_routing", "cheap_model", "provider"] => Ok(config
            .smart_model_routing
            .cheap_model
            .as_ref()
            .and_then(|c| c.provider.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        _ => Err(ConfigError::NotFound(format!(
            "unknown config key: {} (supported: {})",
            key, CONFIG_PATCH_HELP
        ))),
    }
}
