impl App {
    fn model_auto_remediation_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_MODEL_AUTO_REMEDIATE")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn is_model_not_found_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        let model_not_found = message.contains("model not found")
            || message.contains("requested model does not exist")
            || message.contains("404 not found")
            || message.contains("openrouter catalog");
        model_not_found && message.contains("model")
    }


    fn is_provider_tool_payload_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg)
            | AgentError::AuthFailed(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        let mentions_tool_payload =
            message.contains("tool") || message.contains("function") || message.contains("schema");
        let provider_payload_rejected = message.contains("provider returned error")
            && mentions_tool_payload
            && (message.contains("request is not valid")
                || message.contains("valid payload")
                || message.contains("check the model name")
                || message.contains("invalid"));
        let openai_shape_rejected = (message.contains("no choices in response")
            || message.contains("empty choices array"))
            && mentions_tool_payload
            && (message.contains("request is not valid")
                || message.contains("valid payload")
                || message.contains("provider returned error")
                || message.contains("invalid"));
        let explicit_tool_schema_rejected =
            message.contains("tool") && (message.contains("invalid") || message.contains("schema"));
        let strict_function_shape =
            message.contains("invalid input") && message.contains("function");
        provider_payload_rejected
            || openai_shape_rejected
            || explicit_tool_schema_rejected
            || strict_function_shape
            || (message.contains("422") && message.contains("valid payload"))
    }

    fn quorum_output_is_degraded_non_answer(output: &str) -> bool {
        let lower = output.to_ascii_lowercase();
        lower.contains("objective delivery compromised")
            || lower.contains("reverting to hermes")
            || lower.contains("safe-mode response")
            || lower.contains("safe mode response")
            || (lower.contains("i do not have") && lower.contains("tools"))
            || (lower.contains("cannot access") && lower.contains("tools"))
    }


    async fn model_auto_remediation_target(&self, err: &AgentError) -> Option<(String, String)> {
        if !Self::model_auto_remediation_enabled() || !Self::is_model_not_found_error(err) {
            return None;
        }

        let (provider, current_model_id) = self
            .current_model
            .split_once(':')
            .unwrap_or(("openai", self.current_model.as_str()));
        let provider = provider.trim().to_ascii_lowercase();
        if provider.is_empty() {
            return None;
        }

        let catalog = provider_model_ids(&provider).await;
        if catalog.is_empty() {
            return None;
        }

        let selected = Self::resolve_quorum_catalog_candidate(current_model_id, &catalog)
            .or_else(|| catalog.first().cloned())?;

        let next_model = format!("{}:{}", provider, selected.trim());
        if next_model.eq_ignore_ascii_case(&self.current_model) {
            return None;
        }
        let close = Self::rank_catalog_candidates(current_model_id, &catalog, 3);
        let notice = format!(
            "Model catalog remediation: `{}` failed with not-found; switching to `{}` and retrying once. close matches: {}",
            self.current_model,
            next_model,
            if close.is_empty() {
                "(none)".to_string()
            } else {
                close.join(", ")
            }
        );
        Some((next_model, notice))
    }
}
