use hermes_intelligence::{is_sensitive_field_name, redact_sensitive_text};

/// The single immutable bundle prepared before a governed run is admitted.
///
/// The public entry points borrow this bundle to produce sealed start evidence
/// and then move the same values into the execution context. It intentionally
/// has module visibility only: ingress must not synthesize a second, divergent
/// prompt/input/tool contract.
struct PreparedRunInput {
    initial_messages: Vec<Message>,
    advertised_tools: Vec<ToolSchema>,
    task_hint: String,
    system_prompt: ResolvedSystemPrompt,
}

impl PreparedRunInput {
    /// Construct only from inputs that have passed `sanitize_execution_inputs`.
    /// Callers derive `task_hint` from those same sanitized messages.
    fn new(
        initial_messages: Vec<Message>,
        advertised_tools: Vec<ToolSchema>,
        task_hint: String,
        system_prompt: ResolvedSystemPrompt,
    ) -> Self {
        Self {
            initial_messages,
            advertised_tools,
            task_hint,
            system_prompt,
        }
    }

    fn sanitize_execution_inputs(
        mut initial_messages: Vec<Message>,
        advertised_tools: Vec<ToolSchema>,
    ) -> Result<(Vec<Message>, Vec<ToolSchema>), AgentError> {
        sanitize_prepared_messages(&mut initial_messages);
        validate_credential_safe_tools(&advertised_tools)?;
        Ok((initial_messages, advertised_tools))
    }

    fn sanitize_system_prompt(system_prompt: &mut ResolvedSystemPrompt) {
        system_prompt.sanitize_for_execution();
    }

    fn start_evidence(&self) -> crate::run_runtime::PreparedRunStartEvidence<'_> {
        crate::run_runtime::PreparedRunStartEvidence::new(
            &self.initial_messages,
            &self.advertised_tools,
            self.system_prompt.start_evidence(),
        )
    }

    fn into_execution(
        self,
    ) -> Result<(Vec<Message>, Vec<ToolSchema>, String, String, bool), AgentError> {
        let (system_prompt, restored_from_storage) = self.system_prompt.into_execution()?;
        Ok((
            self.initial_messages,
            self.advertised_tools,
            self.task_hint,
            system_prompt,
            restored_from_storage,
        ))
    }
}

/// One resolved system-prompt source. Fresh prompts retain the exact skills
/// block injected into their content; restored prompts never run a fresh skill
/// scan and therefore never claim fresh-skills provenance.
enum ResolvedSystemPrompt {
    Stored { content: String },
    Fresh(FreshSystemPrompt),
    Unavailable { reason: &'static str },
}

impl ResolvedSystemPrompt {
    fn stored(content: String) -> Self {
        Self::Stored { content }
    }

    fn unavailable(reason: &'static str) -> Self {
        Self::Unavailable { reason }
    }

    fn start_evidence(&self) -> crate::run_runtime::PreparedSystemPromptEvidence<'_> {
        match self {
            Self::Stored { content } => crate::run_runtime::PreparedSystemPromptEvidence::Stored {
                content,
            },
            Self::Fresh(fresh) => crate::run_runtime::PreparedSystemPromptEvidence::Fresh {
                content: &fresh.content,
                injected_skills_block: fresh.injected_skills_block.as_deref(),
            },
            Self::Unavailable { reason } => {
                crate::run_runtime::PreparedSystemPromptEvidence::Unavailable { reason }
            }
        }
    }

    fn into_execution(self) -> Result<(String, bool), AgentError> {
        match self {
            Self::Stored { content } => Ok((content, true)),
            Self::Fresh(fresh) => Ok((fresh.content, false)),
            Self::Unavailable { reason } => Err(AgentError::Config(format!(
                "cannot execute a run whose system prompt evidence is unavailable: {reason}"
            ))),
        }
    }

    fn sanitize_for_execution(&mut self) {
        match self {
            Self::Stored { content } => sanitize_prepared_text(content),
            Self::Fresh(fresh) => {
                sanitize_prepared_text(&mut fresh.content);
                if let Some(skills) = &mut fresh.injected_skills_block {
                    sanitize_prepared_text(skills);
                }
            }
            Self::Unavailable { .. } => {}
        }
    }
}

/// Fresh prompt material retained until start evidence is sealed. Keeping the
/// skills block separate prevents a later scan from silently changing what was
/// injected into the prompt.
struct FreshSystemPrompt {
    content: String,
    injected_skills_block: Option<String>,
}

impl std::ops::Deref for FreshSystemPrompt {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

/// Apply the same credential-safe transformation to every value that can enter
/// model execution or sealed start evidence. This occurs before the bundle is
/// exposed, so execution and exact evidence are necessarily identical.
fn sanitize_prepared_messages(messages: &mut [Message]) {
    strip_budget_warnings_from_messages(messages);
    for message in messages {
        if let Some(content) = &mut message.content {
            sanitize_prepared_structured_text(content);
        }
        if let Some(tool_call_id) = &mut message.tool_call_id {
            sanitize_prepared_text(tool_call_id);
        }
        if let Some(name) = &mut message.name {
            sanitize_prepared_text(name);
        }
        if let Some(reasoning) = &mut message.reasoning_content {
            sanitize_prepared_text(reasoning);
        }
        if let Some(tool_calls) = &mut message.tool_calls {
            for tool_call in tool_calls {
                sanitize_prepared_text(&mut tool_call.id);
                sanitize_prepared_text(&mut tool_call.function.name);
                sanitize_prepared_structured_text(&mut tool_call.function.arguments);
                if let Some(extra) = &mut tool_call.extra_content {
                    sanitize_prepared_json(extra);
                }
            }
        }
        if let Some(blocks) = &mut message.anthropic_content_blocks {
            for block in blocks {
                sanitize_prepared_json(block);
            }
        }
    }
}

/// Tool schemas are executable contracts. Never redact or rewrite them:
/// parameter names such as `api_key` are structural names, not secret values.
/// Instead reject a schema before admission if any literal would be redacted.
fn validate_credential_safe_tools(tools: &[ToolSchema]) -> Result<(), AgentError> {
    for tool in tools {
        validate_schema_text(&tool.name, "tool name")?;
        validate_schema_text(&tool.description, "tool description")?;
        if let Some(schema_type) = &tool.parameters.schema_type {
            validate_schema_text(schema_type, "tool schema type")?;
        }
        if let Some(required) = &tool.parameters.required {
            for name in required {
                validate_schema_text(name, "required schema field")?;
            }
        }
        if let Some(properties) = &tool.parameters.properties {
            for value in properties.values() {
                validate_schema_value(value)?;
            }
        }
        if let Some(defs) = &tool.parameters.defs {
            for value in defs.values() {
                validate_schema_value(value)?;
            }
        }
    }
    Ok(())
}

fn validate_schema_value(value: &serde_json::Value) -> Result<(), AgentError> {
    match value {
        serde_json::Value::Object(object) => {
            for value in object.values() {
                validate_schema_value(value)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                validate_schema_value(value)?;
            }
        }
        serde_json::Value::String(value) => validate_schema_text(value, "tool schema literal")?,
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
    Ok(())
}

fn validate_schema_text(value: &str, location: &str) -> Result<(), AgentError> {
    if redact_sensitive_text(value) != value {
        return Err(AgentError::Config(format!(
            "{location} contains a credential-like literal and cannot be advertised"
        )));
    }
    Ok(())
}

fn sanitize_prepared_structured_text(value: &mut String) {
    if let Ok(mut json) = serde_json::from_str(value) {
        sanitize_prepared_json(&mut json);
        *value = serde_json::to_string(&json)
            .expect("serialized JSON values remain serializable after redaction");
    } else {
        sanitize_prepared_text(value);
    }
}

fn sanitize_prepared_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                if is_sensitive_field_name(key) {
                    *value = serde_json::Value::String("[REDACTED_SECRET]".to_string());
                } else {
                    sanitize_prepared_json(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                sanitize_prepared_json(value);
            }
        }
        serde_json::Value::String(value) => sanitize_prepared_text(value),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn sanitize_prepared_text(value: &mut String) {
    let surrogate_free = sanitize_surrogates(value).into_owned();
    *value = redact_sensitive_text(surrogate_free);
}
