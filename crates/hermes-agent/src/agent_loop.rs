//! Core agent loop engine.
//!
//! The `AgentLoop` orchestrates the autonomous agent cycle:
//! 1. Send messages + tools to the LLM
//! 2. If the LLM responds with tool calls, execute them (in parallel)
//! 3. Append results to conversation history
//! 4. Repeat until the model finishes naturally or the turn budget is exceeded

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinSet;

use hermes_core::{
    AgentError, AgentResult, BudgetConfig, LlmProvider, Message, StreamChunk,
    ToolCall, ToolError, ToolResult, ToolSchema, UsageStats,
};

use crate::budget;
use crate::context::ContextManager;
use crate::interrupt::InterruptController;

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// A single tool entry in the registry.
#[derive(Clone)]
pub struct ToolEntry {
    /// The tool's JSON Schema descriptor.
    pub schema: ToolSchema,
    /// A handler function: takes a JSON Value and returns the tool output string.
    pub handler: Arc<dyn Fn(Value) -> Result<String, ToolError> + Send + Sync>,
}

impl std::fmt::Debug for ToolEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolEntry")
            .field("schema", &self.schema)
            .field("handler", &"<function>")
            .finish()
    }
}

/// A simple registry mapping tool names to their schemas and handlers.
///
/// The full-featured implementation lives in `hermes-tools`; this minimal
/// version exists so the agent loop can be tested and used independently.
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn Fn(Value) -> Result<String, ToolError> + Send + Sync>,
    ) {
        self.tools.insert(name.into(), ToolEntry { schema, handler });
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&ToolEntry> {
        self.tools.get(name)
    }

    /// Return all registered tool schemas.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|e| e.schema.clone()).collect()
    }

    /// Return all registered tool names.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// AgentConfig
// ---------------------------------------------------------------------------

/// Configuration for the agent loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Maximum number of LLM → tool → LLM iterations.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// Budget settings for truncating tool output.
    #[serde(default)]
    pub budget: BudgetConfig,

    /// Model identifier (e.g. "gpt-4o", "claude-3-5-sonnet").
    #[serde(default = "default_model")]
    pub model: String,

    /// Optional system prompt prepended to every conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// Optional personality overlay appended to the system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,

    /// Extra JSON body fields forwarded to the provider on every request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<Value>,

    /// Whether to use streaming mode by default.
    #[serde(default)]
    pub stream: bool,

    /// Temperature for LLM sampling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Maximum tokens for LLM completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Maximum number of concurrent delegate_task tool calls.
    #[serde(default = "default_max_concurrent_delegates")]
    pub max_concurrent_delegates: u32,

    /// Flush memories every N turns (placeholder for MemoryManager integration).
    #[serde(default = "default_memory_flush_interval")]
    pub memory_flush_interval: u32,
}

fn default_max_turns() -> u32 {
    30
}

fn default_model() -> String {
    "gpt-4o".to_string()
}

fn default_max_concurrent_delegates() -> u32 {
    1
}

fn default_memory_flush_interval() -> u32 {
    5
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: default_max_turns(),
            budget: BudgetConfig::default(),
            model: default_model(),
            system_prompt: None,
            personality: None,
            extra_body: None,
            stream: false,
            temperature: None,
            max_tokens: None,
            max_concurrent_delegates: default_max_concurrent_delegates(),
            memory_flush_interval: default_memory_flush_interval(),
        }
    }
}

// ---------------------------------------------------------------------------
// TurnMetrics
// ---------------------------------------------------------------------------

/// Timing and usage metrics for a single agent turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnMetrics {
    /// Wall-clock time spent waiting for the LLM API, in milliseconds.
    pub api_time_ms: u64,
    /// Wall-clock time spent executing tools, in milliseconds.
    pub tool_time_ms: u64,
    /// Token usage for this turn (if reported by the provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageStats>,
}

// ---------------------------------------------------------------------------
// AgentLoop
// ---------------------------------------------------------------------------

/// The main agent loop.
///
/// Owns the configuration, a tool registry, and an LLM provider.
/// Call `run()` or `run_stream()` to begin an autonomous loop.
pub struct AgentLoop {
    pub config: AgentConfig,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub interrupt: InterruptController,
}

impl AgentLoop {
    /// Create a new agent loop.
    pub fn new(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
    ) -> Self {
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt: InterruptController::new(),
        }
    }

    /// Create a new agent loop with a shared interrupt controller.
    pub fn with_interrupt(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
        interrupt: InterruptController,
    ) -> Self {
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt,
        }
    }

    /// Run the agent loop (non-streaming).
    ///
    /// Sends the initial messages to the LLM, then iteratively:
    /// - Executes any tool calls the LLM makes
    /// - Feeds results back as tool messages
    /// - Stops when the LLM responds without tool calls, or max turns exceeded
    pub async fn run(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
    ) -> Result<AgentResult, AgentError> {
        let mut ctx = ContextManager::default_budget();
        let mut tool_errors: Vec<hermes_core::ToolErrorRecord> = Vec::new();

        // Add system prompt if configured
        if let Some(ref prompt) = self.config.system_prompt {
            let system_content = match &self.config.personality {
                Some(personality) => format!("{prompt}\n\nPersonality: {personality}"),
                None => prompt.clone(),
            };
            ctx.add_message(Message::system(&system_content));
        }

        // Add initial messages
        for msg in messages {
            ctx.add_message(msg);
        }
        self.hydrate_todo_store(&ctx);

        // Determine which tools to expose
        let tool_schemas: Vec<ToolSchema> = tools
            .unwrap_or_else(|| self.tool_registry.schemas());

        let mut total_turns: u32 = 0;
        let mut _total_api_time_ms: u64 = 0;
        let mut _total_tool_time_ms: u64 = 0;
        let mut accumulated_usage: Option<UsageStats> = None;

        loop {
            // Check for interrupt before each turn
            self.interrupt.check_interrupt()?;

            if total_turns >= self.config.max_turns {
                tracing::warn!(
                    "Max turns ({}) exceeded, requesting final summary",
                    self.config.max_turns
                );
                let summary_msg = self.handle_max_iterations(&mut ctx).await?;
                if let Some(msg) = summary_msg {
                    ctx.add_message(msg);
                }
                return Ok(AgentResult {
                    messages: ctx.get_messages().to_vec(),
                    finished_naturally: false,
                    total_turns,
                    tool_errors,
                    usage: accumulated_usage,
                });
            }

            total_turns += 1;
            tracing::debug!("Agent turn {}", total_turns);

            // Memory flush hook
            if total_turns % self.config.memory_flush_interval == 0 && total_turns > 0 {
                tracing::debug!("Memory flush triggered at turn {}", total_turns);
                // TODO: integrate with MemoryManager
            }

            // Inject budget warning when close to the turn limit
            if let Some(warning) = self.get_budget_warning(total_turns) {
                tracing::info!("{}", warning);
                ctx.add_message(Message::system(&warning));
            }

            // --- LLM API call ---
            let api_start = Instant::now();
            let response = self
                .llm_provider
                .chat_completion(
                    ctx.get_messages(),
                    &tool_schemas,
                    self.config.max_tokens,
                    self.config.temperature,
                    Some(self.config.model.as_str()),
                    self.config.extra_body.as_ref(),
                )
                .await
                .map_err(|e| AgentError::LlmApi(e.to_string()))?;
            let api_elapsed = api_start.elapsed().as_millis() as u64;
            _total_api_time_ms += api_elapsed;

            // Accumulate usage
            if let Some(ref usage) = response.usage {
                accumulated_usage = Some(merge_usage(accumulated_usage, usage));
            }

            // Check for reasoning content and attach it to the message
            let assistant_msg = response.message.clone();

            // Check if the response has tool calls
            let tool_calls = assistant_msg.tool_calls.clone();

            // Always add the assistant message to history
            ctx.add_message(assistant_msg);

            // If no tool calls, the agent is done
            let tool_calls = match tool_calls {
                Some(calls) if !calls.is_empty() => calls,
                _ => {
                    tracing::debug!("No tool calls in response, finishing naturally");
                    return Ok(AgentResult {
                        messages: ctx.get_messages().to_vec(),
                        finished_naturally: true,
                        total_turns,
                        tool_errors,
                        usage: accumulated_usage,
                    });
                }
            };

            // Deduplicate tool calls
            let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);

            // Repair unknown tool names via fuzzy matching
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
            }

            // Cap concurrent delegate_task calls
            let delegate_count = tool_calls
                .iter()
                .filter(|tc| tc.function.name == "delegate_task")
                .count() as u32;
            if delegate_count > self.config.max_concurrent_delegates {
                tracing::warn!(
                    "Capping delegate_task calls from {} to {}",
                    delegate_count,
                    self.config.max_concurrent_delegates
                );
                let mut kept_delegates = 0u32;
                tool_calls.retain(|tc| {
                    if tc.function.name == "delegate_task" {
                        if kept_delegates < self.config.max_concurrent_delegates {
                            kept_delegates += 1;
                            true
                        } else {
                            false
                        }
                    } else {
                        true
                    }
                });
            }

            // --- Execute tool calls in parallel ---
            self.interrupt.check_interrupt()?;
            let tool_start = Instant::now();
            let results = self.execute_tool_calls(&tool_calls, total_turns, &mut tool_errors).await;
            let tool_elapsed = tool_start.elapsed().as_millis() as u64;
            _total_tool_time_ms += tool_elapsed;

            // Enforce budget on tool results
            let mut results = results;
            budget::enforce_budget(&mut results, &self.config.budget);

            // Append tool results as tool messages
            for result in results {
                ctx.add_message(Message::tool_result(&result.tool_call_id, &result.content));
            }
            self.spawn_background_review(total_turns, ctx.get_messages().to_vec());

            // Auto context compression
            let total_chars = ctx.total_chars();
            let threshold = (200_000_f64 * 0.8) as usize;
            if total_chars > threshold {
                tracing::info!(
                    "Context pressure at {}%, triggering compression",
                    (total_chars * 100) / 200_000
                );
                ctx.compress();
            }
        }
    }

    /// Run the agent loop with streaming.
    ///
    /// Uses the LLM provider's streaming API and invokes `on_chunk` for each
    /// incremental delta. The stream is collected into a complete response
    /// before tool execution proceeds.
    pub async fn run_stream(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
        on_chunk: Option<Box<dyn Fn(StreamChunk) + Send + Sync>>,
    ) -> Result<AgentResult, AgentError> {
        let on_chunk = match on_chunk {
            Some(cb) => cb,
            None => {
                // No callback — fall back to non-streaming
                return self.run(messages, tools).await;
            }
        };

        let mut ctx = ContextManager::default_budget();
        let mut tool_errors: Vec<hermes_core::ToolErrorRecord> = Vec::new();

        if let Some(ref prompt) = self.config.system_prompt {
            let system_content = match &self.config.personality {
                Some(personality) => format!("{prompt}\n\nPersonality: {personality}"),
                None => prompt.clone(),
            };
            ctx.add_message(Message::system(&system_content));
        }

        for msg in messages {
            ctx.add_message(msg);
        }
        self.hydrate_todo_store(&ctx);

        let tool_schemas: Vec<ToolSchema> = tools
            .unwrap_or_else(|| self.tool_registry.schemas());

        let mut total_turns: u32 = 0;
        let mut accumulated_usage: Option<UsageStats> = None;

        loop {
            self.interrupt.check_interrupt()?;

            if total_turns >= self.config.max_turns {
                tracing::warn!(
                    "Max turns ({}) exceeded, requesting final summary",
                    self.config.max_turns
                );
                let summary_msg = self.handle_max_iterations(&mut ctx).await?;
                if let Some(msg) = summary_msg {
                    ctx.add_message(msg);
                }
                return Ok(AgentResult {
                    messages: ctx.get_messages().to_vec(),
                    finished_naturally: false,
                    total_turns,
                    tool_errors,
                    usage: accumulated_usage,
                });
            }

            total_turns += 1;

            // Memory flush hook
            if total_turns % self.config.memory_flush_interval == 0 && total_turns > 0 {
                tracing::debug!("Memory flush triggered at turn {}", total_turns);
                // TODO: integrate with MemoryManager
            }

            // Inject budget warning when close to the turn limit
            if let Some(warning) = self.get_budget_warning(total_turns) {
                tracing::info!("{}", warning);
                ctx.add_message(Message::system(&warning));
            }

            // --- Streaming LLM call ---
            let mut stream = self.llm_provider.chat_completion_stream(
                ctx.get_messages(),
                &tool_schemas,
                self.config.max_tokens,
                self.config.temperature,
                Some(self.config.model.as_str()),
                self.config.extra_body.as_ref(),
            );

            let mut content = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut last_usage: Option<UsageStats> = None;

            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result?;

                // Accumulate content deltas
                if let Some(ref delta) = chunk.delta {
                    if let Some(ref text) = delta.content {
                        content.push_str(text);
                    }
                    // Accumulate tool call deltas
                    if let Some(ref tc_deltas) = delta.tool_calls {
                        for tcd in tc_deltas {
                            let idx = tcd.index as usize;
                            while tool_calls.len() <= idx {
                                tool_calls.push(ToolCall {
                                    id: String::new(),
                                    function: hermes_core::FunctionCall {
                                        name: String::new(),
                                        arguments: String::new(),
                                    },
                                });
                            }
                            if let Some(ref id) = tcd.id {
                                tool_calls[idx].id = id.clone();
                            }
                            if let Some(ref fc) = tcd.function {
                                if let Some(ref name) = fc.name {
                                    tool_calls[idx].function.name = name.clone();
                                }
                                if let Some(ref args) = fc.arguments {
                                    tool_calls[idx].function.arguments.push_str(args);
                                }
                            }
                        }
                    }
                }

                if let Some(ref usage) = chunk.usage {
                    last_usage = Some(usage.clone());
                }

                // Forward chunk to callback
                on_chunk(chunk);
            }

            if let Some(ref usage) = last_usage {
                accumulated_usage = Some(merge_usage(accumulated_usage, usage));
            }

            // Build assistant message
            let assistant_msg = if tool_calls.is_empty() || tool_calls.iter().all(|tc| tc.function.name.is_empty()) {
                Message::assistant(&content)
            } else {
                let content_opt = if content.is_empty() { None } else { Some(content.clone()) };
                Message::assistant_with_tool_calls(content_opt, tool_calls.clone())
            };

            ctx.add_message(assistant_msg);

            // Filter out empty tool calls
            let tool_calls: Vec<ToolCall> = tool_calls
                .into_iter()
                .filter(|tc| !tc.function.name.is_empty())
                .collect();

            if tool_calls.is_empty() {
                return Ok(AgentResult {
                    messages: ctx.get_messages().to_vec(),
                    finished_naturally: true,
                    total_turns,
                    tool_errors,
                    usage: accumulated_usage,
                });
            }

            // Deduplicate tool calls
            let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);

            // Repair unknown tool names via fuzzy matching
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
            }

            // Cap concurrent delegate_task calls
            let delegate_count = tool_calls
                .iter()
                .filter(|tc| tc.function.name == "delegate_task")
                .count() as u32;
            if delegate_count > self.config.max_concurrent_delegates {
                tracing::warn!(
                    "Capping delegate_task calls from {} to {}",
                    delegate_count,
                    self.config.max_concurrent_delegates
                );
                let mut kept_delegates = 0u32;
                tool_calls.retain(|tc| {
                    if tc.function.name == "delegate_task" {
                        if kept_delegates < self.config.max_concurrent_delegates {
                            kept_delegates += 1;
                            true
                        } else {
                            false
                        }
                    } else {
                        true
                    }
                });
            }

            // Execute tool calls
            let mut results = self.execute_tool_calls(&tool_calls, total_turns, &mut tool_errors).await;
            budget::enforce_budget(&mut results, &self.config.budget);

            for result in results {
                ctx.add_message(Message::tool_result(&result.tool_call_id, &result.content));
            }
            self.spawn_background_review(total_turns, ctx.get_messages().to_vec());

            // Auto context compression
            let total_chars = ctx.total_chars();
            let threshold = (200_000_f64 * 0.8) as usize;
            if total_chars > threshold {
                tracing::info!(
                    "Context pressure at {}%, triggering compression",
                    (total_chars * 100) / 200_000
                );
                ctx.compress();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Remove duplicate tool calls that share the same function name and arguments.
    fn deduplicate_tool_calls(calls: &[ToolCall]) -> Vec<ToolCall> {
        let mut seen = HashSet::new();
        let mut deduped = Vec::new();
        for tc in calls {
            let key = format!("{}:{}", tc.function.name, tc.function.arguments);
            if seen.insert(key) {
                deduped.push(tc.clone());
            } else {
                tracing::warn!("Deduplicated tool call: {}", tc.function.name);
            }
        }
        deduped
    }

    /// Try to repair an unknown tool name via case-insensitive or substring matching.
    /// Returns `true` if the tool call was repaired.
    fn repair_tool_call(&self, tc: &mut ToolCall) -> bool {
        if self.tool_registry.get(&tc.function.name).is_some() {
            return false;
        }
        let names = self.tool_registry.names();
        let target = tc.function.name.to_lowercase();

        if let Some(name) = names.iter().find(|n| n.to_lowercase() == target) {
            tracing::info!("Repaired tool call: '{}' → '{}'", tc.function.name, name);
            tc.function.name = name.clone();
            return true;
        }

        if let Some(name) = names.iter().find(|n| {
            n.to_lowercase().contains(&target) || target.contains(&n.to_lowercase())
        }) {
            tracing::info!(
                "Repaired tool call (fuzzy): '{}' → '{}'",
                tc.function.name,
                name
            );
            tc.function.name = name.clone();
            return true;
        }
        false
    }

    /// Return a budget warning message when the agent is close to the turn limit.
    fn get_budget_warning(&self, current_turn: u32) -> Option<String> {
        let remaining = self.config.max_turns.saturating_sub(current_turn);
        if remaining <= 3 && remaining > 0 {
            Some(format!(
                "[SYSTEM WARNING] You have {} turn(s) remaining before the conversation limit. \
                 Please wrap up your current task and provide a final summary.",
                remaining
            ))
        } else {
            None
        }
    }

    /// Ask the LLM for a final summary when the turn budget is exhausted.
    async fn handle_max_iterations(
        &self,
        ctx: &mut ContextManager,
    ) -> Result<Option<Message>, AgentError> {
        ctx.add_message(Message::system(
            "[SYSTEM] Maximum conversation turns reached. Please provide a brief summary of \
             what was accomplished and any remaining tasks.",
        ));
        let response = self
            .llm_provider
            .chat_completion(
                ctx.get_messages(),
                &[],
                self.config.max_tokens,
                self.config.temperature,
                Some(self.config.model.as_str()),
                self.config.extra_body.as_ref(),
            )
            .await
            .map_err(|e| AgentError::LlmApi(e.to_string()))?;
        Ok(Some(response.message))
    }

    /// Execute a batch of tool calls in parallel using a JoinSet.
    async fn execute_tool_calls(
        &self,
        tool_calls: &[ToolCall],
        turn: u32,
        tool_errors: &mut Vec<hermes_core::ToolErrorRecord>,
    ) -> Vec<ToolResult> {
        let mut join_set = JoinSet::new();

        for tc in tool_calls {
            let tool_call_id = tc.id.clone();
            let tool_name = tc.function.name.clone();
            let raw_args = tc.function.arguments.clone();
            let registry = self.tool_registry.clone();

            join_set.spawn(async move {
                match registry.get(&tool_name) {
                    Some(entry) => {
                        // Parse arguments
                        let params: Value = match serde_json::from_str(&raw_args) {
                            Ok(v) => v,
                            Err(e) => {
                                let error_msg = format!(
                                    "Invalid JSON params for tool '{}': {}. \
                                     Please check your parameters and retry with valid JSON.",
                                    tool_name, e
                                );
                                return ToolResult::err(&tool_call_id, error_msg);
                            }
                        };

                        // Execute the handler
                        match (entry.handler)(params) {
                            Ok(output) => ToolResult::ok(&tool_call_id, output),
                            Err(e) => ToolResult::err(&tool_call_id, e.to_string()),
                        }
                    }
                    None => {
                        let available = registry.names().join(", ");
                        let error_msg = format!(
                            "Unknown tool '{}'. Available tools: [{}]",
                            tool_name, available
                        );
                        ToolResult::err(&tool_call_id, error_msg)
                    }
                }
            });
        }

        let mut results = Vec::with_capacity(tool_calls.len());
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(tool_result) => {
                    if tool_result.is_error {
                        // Record the error but we still add the result to context
                        let tc = tool_calls
                            .iter()
                            .find(|tc| tc.id == tool_result.tool_call_id);
                        if let Some(tc) = tc {
                            tool_errors.push(hermes_core::ToolErrorRecord {
                                tool_name: tc.function.name.clone(),
                                error: tool_result.content.clone(),
                                turn,
                            });
                        }
                    }
                    results.push(tool_result);
                }
                Err(e) => {
                    tracing::error!("Task join error: {}", e);
                }
            }
        }

        results
    }

    /// Spawn asynchronous post-tool review hook.
    ///
    /// Current implementation records lightweight metrics and leaves room for
    /// richer policy checks (unsafe actions, low-confidence outputs, etc.).
    fn spawn_background_review(&self, turn: u32, snapshot: Vec<Message>) {
        tokio::spawn(async move {
            let tool_msg_count = snapshot
                .iter()
                .filter(|m| matches!(m.role, hermes_core::MessageRole::Tool))
                .count();
            tracing::debug!(
                turn,
                tool_messages = tool_msg_count,
                total_messages = snapshot.len(),
                "Background review snapshot captured"
            );
        });
    }

    /// Recover todo-state hints from historical messages at loop start.
    fn hydrate_todo_store(&self, ctx: &ContextManager) {
        let todo_markers = ctx
            .get_messages()
            .iter()
            .filter_map(|m| m.content.as_deref())
            .filter(|c| c.contains("TODO") || c.contains("[ ]") || c.contains("[x]"))
            .count();
        if todo_markers > 0 {
            tracing::debug!(todo_markers, "Hydrated todo markers from prior context");
        }
    }
}

/// Merge two UsageStats, summing token counts and keeping the latest cost estimate.
fn merge_usage(existing: Option<UsageStats>, new: &UsageStats) -> UsageStats {
    match existing {
        Some(prev) => UsageStats {
            prompt_tokens: prev.prompt_tokens + new.prompt_tokens,
            completion_tokens: prev.completion_tokens + new.completion_tokens,
            total_tokens: prev.total_tokens + new.total_tokens,
            estimated_cost: match (prev.estimated_cost, new.estimated_cost) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
        },
        None => new.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.max_turns, 30);
        assert_eq!(config.model, "gpt-4o");
        assert!(!config.stream);
        assert_eq!(config.max_concurrent_delegates, 1);
        assert_eq!(config.memory_flush_interval, 5);
    }

    #[test]
    fn test_deduplicate_tool_calls() {
        let calls = vec![
            ToolCall {
                id: "1".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            },
            ToolCall {
                id: "2".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            },
            ToolCall {
                id: "3".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"b.txt"}"#.into(),
                },
            },
        ];
        let deduped = AgentLoop::deduplicate_tool_calls(&calls);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].id, "1");
        assert_eq!(deduped[1].id, "3");
    }

    #[test]
    fn test_budget_warning() {
        let config = AgentConfig {
            max_turns: 10,
            ..AgentConfig::default()
        };
        let registry = Arc::new(ToolRegistry::new());
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(config, registry, Arc::new(DummyProvider));

        assert!(agent.get_budget_warning(1).is_none());
        assert!(agent.get_budget_warning(7).is_some()); // 3 remaining
        assert!(agent.get_budget_warning(8).is_some()); // 2 remaining
        assert!(agent.get_budget_warning(9).is_some()); // 1 remaining
        assert!(agent.get_budget_warning(10).is_none()); // 0 remaining
    }

    #[test]
    fn test_tool_registry_new() {
        let registry = ToolRegistry::new();
        assert!(registry.names().is_empty());
    }

    #[test]
    fn test_merge_usage() {
        let a = UsageStats {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            estimated_cost: Some(0.01),
        };
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            estimated_cost: Some(0.02),
        };
        let merged = merge_usage(Some(a), &b);
        assert_eq!(merged.prompt_tokens, 300);
        assert_eq!(merged.completion_tokens, 150);
        assert_eq!(merged.total_tokens, 450);
        assert_eq!(merged.estimated_cost, Some(0.03));
    }

    #[test]
    fn test_merge_usage_none() {
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            estimated_cost: None,
        };
        let merged = merge_usage(None, &b);
        assert_eq!(merged.prompt_tokens, 200);
    }
}