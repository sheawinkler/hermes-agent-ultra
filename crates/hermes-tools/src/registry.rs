//! Tool Registry (Requirement 4)
//!
//! Central registry for all tool definitions and handlers. Supports:
//! - Dynamic registration/deregistration with availability checks
//! - Name-conflict detection with warning on overwrite
//! - Per-tool result size limits and global default
//! - Dispatch with error catching that always returns JSON

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hermes_core::{ToolHandler, ToolSchema};
use serde_json::Value;
use tracing::warn;

use crate::tool_policy::{annotate_policy_audit, ToolPolicyDecision, ToolPolicyEngine};

// ---------------------------------------------------------------------------
// ToolEntry
// ---------------------------------------------------------------------------

/// A registered tool with its handler, metadata, and availability check.
pub struct ToolEntry {
    /// Unique tool name (e.g. "web_search", "terminal").
    pub name: String,
    /// Which toolset this tool belongs to (e.g. "web", "terminal").
    pub toolset: String,
    /// OpenAI-format tool schema.
    pub schema: ToolSchema,
    /// Handler that executes the tool logic.
    pub handler: Arc<dyn ToolHandler>,
    /// Availability check — returns true if the tool can be used right now.
    pub check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Environment dependencies required for this tool (e.g. "EXA_API_KEY").
    pub env_deps: Vec<String>,
    /// Whether this tool performs async I/O.
    pub is_async: bool,
    /// Human-readable description.
    pub description: String,
    /// Emoji icon for display.
    pub emoji: String,
    /// Per-tool max result size override (characters).
    pub max_result_size_chars: Option<usize>,
}

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// Thread-safe registry of all available tools.
pub struct ToolRegistryInner {
    /// Registered tools keyed by name.
    tools: HashMap<String, ToolEntry>,
    /// Global default max result size in characters.
    pub global_max_result_size_chars: usize,
    /// Centralized policy engine for tool-call governance.
    pub policy: ToolPolicyEngine,
}

/// Thread-safe wrapper around `ToolRegistryInner`.
#[derive(Clone)]
pub struct ToolRegistry {
    inner: Arc<Mutex<ToolRegistryInner>>,
}

impl ToolRegistry {
    /// Create a new empty registry with default global max result size.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ToolRegistryInner {
                tools: HashMap::new(),
                global_max_result_size_chars: 50_000,
                policy: ToolPolicyEngine::from_env(),
            })),
        }
    }

    /// Create a new registry with a custom global max result size.
    pub fn with_max_result_size(max: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ToolRegistryInner {
                tools: HashMap::new(),
                global_max_result_size_chars: max,
                policy: ToolPolicyEngine::from_env(),
            })),
        }
    }

    /// Override active tool policy engine (used by tests/runtime tuning).
    pub fn set_policy(&self, policy: ToolPolicyEngine) {
        let mut inner = self.inner.lock().unwrap();
        inner.policy = policy;
    }

    /// Register a new tool.
    ///
    /// If a tool with the same name already exists, logs a warning and overwrites.
    pub fn register(
        &self,
        name: impl Into<String>,
        toolset: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn ToolHandler>,
        check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
        env_deps: Vec<String>,
        is_async: bool,
        description: impl Into<String>,
        emoji: impl Into<String>,
        max_result_size_chars: Option<usize>,
    ) {
        let name = name.into();
        let mut inner = self.inner.lock().unwrap();
        if inner.tools.contains_key(&name) {
            warn!("Tool '{}' already registered; overwriting", name);
        }
        inner.tools.insert(
            name.clone(),
            ToolEntry {
                name: name.clone(),
                toolset: toolset.into(),
                schema,
                handler,
                check_fn,
                env_deps,
                is_async,
                description: description.into(),
                emoji: emoji.into(),
                max_result_size_chars,
            },
        );
    }

    /// Deregister a tool by name.
    ///
    /// Returns `true` if the tool was present and removed.
    pub fn deregister(&self, name: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.tools.remove(name).is_some()
    }

    /// Get tool definitions for all tools whose `check_fn` returns true.
    ///
    /// This is used to build the tool list sent to the LLM.
    pub fn get_definitions(&self) -> Vec<ToolSchema> {
        let inner = self.inner.lock().unwrap();
        inner
            .tools
            .values()
            .filter(|entry| (entry.check_fn)())
            .map(|entry| entry.schema.clone())
            .collect()
    }

    /// Dispatch a tool call by name, catching all errors.
    ///
    /// On success, returns the tool result string.
    /// On failure, returns a JSON error string: `{"error": "..."}`.
    pub fn dispatch(&self, name: &str, params: Value) -> String {
        let (handler, max_chars, policy_decision) = {
            let inner = self.inner.lock().unwrap();
            let entry = match inner.tools.get(name) {
                Some(e) => e,
                None => return Self::tool_error(&format!("Tool not found: {}", name)),
            };
            let decision = inner.policy.evaluate(name, &params);
            if !decision.allow {
                return Self::tool_error(&format!(
                    "Blocked by tool policy: {}",
                    decision.reason.unwrap_or_else(|| "policy deny".to_string())
                ));
            }
            (
                Arc::clone(&entry.handler),
                entry
                    .max_result_size_chars
                    .unwrap_or(inner.global_max_result_size_chars),
                decision,
            )
        };
        maybe_log_audit(name, &policy_decision);

        // Use tokio to run the async handler
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async { handler.execute(params).await })
        });

        let output = match result {
            Ok(output) => {
                if output.len() > max_chars {
                    Self::tool_result(&format!(
                        "{}\n\n[... truncated {} characters ...]",
                        &output[..max_chars],
                        output.len() - max_chars
                    ))
                } else {
                    Self::tool_result(&output)
                }
            }
            Err(e) => Self::tool_error(&e.to_string()),
        };
        maybe_annotate_audit(output, &policy_decision)
    }

    /// Dispatch a tool call asynchronously.
    pub async fn dispatch_async(&self, name: &str, params: Value) -> String {
        let (handler, max_chars, policy_decision) = {
            let inner = self.inner.lock().unwrap();
            match inner.tools.get(name) {
                Some(e) => (
                    Arc::clone(&e.handler),
                    e.max_result_size_chars
                        .unwrap_or(inner.global_max_result_size_chars),
                    inner.policy.evaluate(name, &params),
                ),
                None => return Self::tool_error(&format!("Tool not found: {}", name)),
            }
        };
        if !policy_decision.allow {
            return Self::tool_error(&format!(
                "Blocked by tool policy: {}",
                policy_decision
                    .reason
                    .clone()
                    .unwrap_or_else(|| "policy deny".to_string())
            ));
        }
        maybe_log_audit(name, &policy_decision);

        let output = match handler.execute(params).await {
            Ok(output) => {
                if output.len() > max_chars {
                    Self::tool_result(&format!(
                        "{}\n\n[... truncated {} characters ...]",
                        &output[..max_chars],
                        output.len() - max_chars
                    ))
                } else {
                    Self::tool_result(&output)
                }
            }
            Err(e) => Self::tool_error(&e.to_string()),
        };
        maybe_annotate_audit(output, &policy_decision)
    }

    /// Get a reference to a tool entry by name.
    ///
    /// Note: This returns a cloned `Arc<ToolEntry>` because we cannot return
    /// a reference to data behind a Mutex lock. Returns `None` if not found.
    pub fn get_tool(&self, name: &str) -> Option<ToolEntryInfo> {
        let inner = self.inner.lock().unwrap();
        inner.tools.get(name).map(|e| ToolEntryInfo {
            name: e.name.clone(),
            toolset: e.toolset.clone(),
            description: e.description.clone(),
            emoji: e.emoji.clone(),
            is_async: e.is_async,
            env_deps: e.env_deps.clone(),
            max_result_size_chars: e.max_result_size_chars,
        })
    }

    /// List all registered tool entries.
    pub fn list_tools(&self) -> Vec<ToolEntryInfo> {
        let inner = self.inner.lock().unwrap();
        inner
            .tools
            .values()
            .map(|e| ToolEntryInfo {
                name: e.name.clone(),
                toolset: e.toolset.clone(),
                description: e.description.clone(),
                emoji: e.emoji.clone(),
                is_async: e.is_async,
                env_deps: e.env_deps.clone(),
                max_result_size_chars: e.max_result_size_chars,
            })
            .collect()
    }

    /// List distinct toolset names.
    pub fn list_toolsets(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        let mut sets: Vec<String> = inner.tools.values().map(|e| e.toolset.clone()).collect();
        sets.sort();
        sets.dedup();
        sets
    }

    /// Check whether a tool is available (exists and check_fn passes).
    pub fn is_available(&self, name: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        match inner.tools.get(name) {
            Some(e) => (e.check_fn)(),
            None => false,
        }
    }

    /// Format an error as JSON: `{"error": "msg"}`.
    pub fn tool_error(msg: &str) -> String {
        serde_json::json!({ "error": msg }).to_string()
    }

    /// Format a tool result. If the result looks like JSON, pass it through;
    /// otherwise wrap it as `{"result": "..."}`.
    pub fn tool_result(data: &str) -> String {
        if data.starts_with('{') || data.starts_with('[') {
            // Assume already JSON-ish, pass through
            data.to_string()
        } else {
            serde_json::json!({ "result": data }).to_string()
        }
    }
}

fn maybe_log_audit(tool_name: &str, decision: &ToolPolicyDecision) {
    if decision.audited_only {
        warn!(
            "Tool policy audit-only violation for '{}': {}",
            tool_name,
            decision.reason.as_deref().unwrap_or("no reason supplied")
        );
    }
}

fn maybe_annotate_audit(output: String, decision: &ToolPolicyDecision) -> String {
    if decision.audited_only {
        annotate_policy_audit(
            &output,
            decision
                .reason
                .as_deref()
                .unwrap_or("tool policy audit warning"),
        )
    } else {
        output
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Simplified info about a tool entry (no handler, for inspection).
#[derive(Debug, Clone)]
pub struct ToolEntryInfo {
    pub name: String,
    pub toolset: String,
    pub description: String,
    pub emoji: String,
    pub is_async: bool,
    pub env_deps: Vec<String>,
    pub max_result_size_chars: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use hermes_core::{tool_schema, JsonSchema, ToolError};
    use serde_json::json;

    use crate::tool_policy::{ToolPolicyEngine, ToolPolicyMode};

    struct EchoHandler;

    #[async_trait]
    impl ToolHandler for EchoHandler {
        async fn execute(&self, params: Value) -> Result<String, ToolError> {
            Ok(params.to_string())
        }
        fn schema(&self) -> ToolSchema {
            tool_schema("echo", "Echo back input", JsonSchema::new("object"))
        }
    }

    #[test]
    fn test_register_and_dispatch() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo tool",
            "🔊",
            None,
        );

        let defs = registry.get_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "echo");
    }

    #[test]
    fn test_dispatch_not_found() {
        let registry = ToolRegistry::new();
        let result = registry.dispatch("nonexistent", Value::Null);
        assert!(result.contains("error"));
    }

    #[test]
    fn test_tool_error_format() {
        let msg = ToolRegistry::tool_error("something went wrong");
        let parsed: Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(parsed["error"], "something went wrong");
    }

    #[test]
    fn test_tool_result_format() {
        // Plain string wrapped as JSON
        let result = ToolRegistry::tool_result("hello");
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["result"], "hello");

        // Already JSON passes through
        let json_result = ToolRegistry::tool_result(r#"{"key": "val"}"#);
        let parsed: Value = serde_json::from_str(&json_result).unwrap();
        assert_eq!(parsed["key"], "val");
    }

    #[test]
    fn test_deregister() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo tool",
            "🔊",
            None,
        );
        assert!(registry.deregister("echo"));
        assert!(!registry.deregister("echo"));
        assert!(registry.get_definitions().is_empty());
    }

    #[test]
    fn test_check_fn_filtering() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "available",
            "test",
            schema.clone(),
            Arc::new(EchoHandler),
            Arc::new(|| true),
            vec![],
            false,
            "Available",
            "✅",
            None,
        );
        registry.register(
            "unavailable",
            "test",
            schema,
            Arc::new(EchoHandler),
            Arc::new(|| false),
            vec![],
            false,
            "Unavailable",
            "❌",
            None,
        );
        let defs = registry.get_definitions();
        assert_eq!(defs.len(), 1);
        // ToolSchema name comes from the handler's schema(), which is "echo" for EchoHandler.
        // The registry key ("available") and the schema name may differ.
        assert_eq!(defs[0].name, "echo");
    }

    #[test]
    fn test_dispatch_async_blocked_by_policy() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo",
            "🔊",
            None,
        );
        registry
            .set_policy(ToolPolicyEngine::new(ToolPolicyMode::Enforce).with_denylist(&["echo"]));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let result = rt.block_on(async { registry.dispatch_async("echo", json!({"k":"v"})).await });
        assert!(result.contains("Blocked by tool policy"));
    }

    #[test]
    fn test_dispatch_async_audit_annotation() {
        let registry = ToolRegistry::new();
        let handler = Arc::new(EchoHandler);
        let schema = handler.schema();
        registry.register(
            "echo",
            "test",
            schema,
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Echo",
            "🔊",
            None,
        );
        registry
            .set_policy(ToolPolicyEngine::new(ToolPolicyMode::Audit).with_allowlist(&["other"]));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let result = rt.block_on(async { registry.dispatch_async("echo", json!({"k":"v"})).await });
        let parsed: Value = serde_json::from_str(&result).expect("json output");
        assert!(parsed["_tool_policy_warning"]
            .as_str()
            .unwrap_or("")
            .contains("allowlist"));
        assert_eq!(parsed["k"], "v");
    }
}
