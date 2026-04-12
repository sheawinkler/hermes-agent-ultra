//! Core agent loop engine.
//!
//! The `AgentLoop` orchestrates the autonomous agent cycle:
//! 1. Send messages + tools to the LLM
//! 2. If the LLM responds with tool calls, execute them (in parallel)
//! 3. Append results to conversation history
//! 4. Repeat until the model finishes naturally or the turn budget is exceeded

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinSet;
use tokio::time::sleep;

use hermes_core::{
    AgentError, AgentResult, BudgetConfig, LlmProvider, Message, StreamChunk,
    ToolCall, ToolError, ToolResult, ToolSchema, UsageStats,
};

use crate::budget;
use crate::context::ContextManager;
use crate::interrupt::InterruptController;
use crate::memory_manager::MemoryManager;
use crate::plugins::{HookResult, HookType, PluginManager};

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

/// API mode — determines how requests are formatted for the LLM backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiMode {
    ChatCompletions,
    AnthropicMessages,
    CodexResponses,
}

impl Default for ApiMode {
    fn default() -> Self {
        Self::ChatCompletions
    }
}

/// Retry / failover configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum retries before giving up.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Base delay for exponential backoff (ms).
    #[serde(default = "default_base_delay_ms")]
    pub base_delay_ms: u64,
    /// Maximum backoff cap (ms).
    #[serde(default = "default_max_delay_ms")]
    pub max_delay_ms: u64,
    /// Optional fallback model identifier (tried after all retries on the primary model fail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_model: Option<String>,
}

fn default_max_retries() -> u32 { 3 }
fn default_base_delay_ms() -> u64 { 1000 }
fn default_max_delay_ms() -> u64 { 30_000 }

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            base_delay_ms: default_base_delay_ms(),
            max_delay_ms: default_max_delay_ms(),
            fallback_model: None,
        }
    }
}

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

    /// API mode — selects the request format for the LLM provider.
    #[serde(default)]
    pub api_mode: ApiMode,

    /// Retry / failover configuration.
    #[serde(default)]
    pub retry: RetryConfig,

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

    /// Flush memories every N turns.
    #[serde(default = "default_memory_flush_interval")]
    pub memory_flush_interval: u32,

    /// Session identifier — used for memory and persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// HERMES_HOME path — used by memory plugins for config resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hermes_home: Option<String>,

    /// Skip memory integration even if a MemoryManager is provided.
    #[serde(default)]
    pub skip_memory: bool,

    /// Provider hint (e.g. "openai", "anthropic", "openrouter").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Session-level hard spend limit in USD. When reached, the loop trips
    /// the cost gate and returns early with a summary system message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,

    /// Ratio (0.0-1.0) at which to proactively degrade to a cheaper model.
    #[serde(default = "default_cost_guard_degrade_at_ratio")]
    pub cost_guard_degrade_at_ratio: f64,

    /// Optional explicit cheaper model to use after crossing the degrade ratio.
    /// If unset, falls back to `retry.fallback_model` then a built-in cheap default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_guard_degrade_model: Option<String>,

    /// Optional per-million-token prompt price used when provider does not
    /// return `usage.estimated_cost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cost_per_million_usd: Option<f64>,

    /// Optional per-million-token completion price used when provider does not
    /// return `usage.estimated_cost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_cost_per_million_usd: Option<f64>,

    /// Auto-checkpoint interval in turns. `0` disables automatic checkpoints.
    #[serde(default = "default_checkpoint_interval_turns")]
    pub checkpoint_interval_turns: u32,

    /// If a single turn generates at least this many tool errors, rollback
    /// to the latest checkpoint and continue. `0` disables rollback.
    #[serde(default = "default_rollback_on_tool_error_threshold")]
    pub rollback_on_tool_error_threshold: u32,
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

fn default_cost_guard_degrade_at_ratio() -> f64 {
    0.8
}

fn default_checkpoint_interval_turns() -> u32 {
    3
}

fn default_rollback_on_tool_error_threshold() -> u32 {
    3
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: default_max_turns(),
            budget: BudgetConfig::default(),
            model: default_model(),
            api_mode: ApiMode::default(),
            retry: RetryConfig::default(),
            system_prompt: None,
            personality: None,
            extra_body: None,
            stream: false,
            temperature: None,
            max_tokens: None,
            max_concurrent_delegates: default_max_concurrent_delegates(),
            memory_flush_interval: default_memory_flush_interval(),
            session_id: None,
            hermes_home: None,
            skip_memory: false,
            provider: None,
            max_cost_usd: None,
            cost_guard_degrade_at_ratio: default_cost_guard_degrade_at_ratio(),
            cost_guard_degrade_model: None,
            prompt_cost_per_million_usd: None,
            completion_cost_per_million_usd: None,
            checkpoint_interval_turns: default_checkpoint_interval_turns(),
            rollback_on_tool_error_threshold: default_rollback_on_tool_error_threshold(),
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

/// Callbacks invoked during tool execution for progress reporting.
#[derive(Default)]
pub struct AgentCallbacks {
    /// Called when the LLM is "thinking" (reasoning tokens).
    pub on_thinking: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called when a tool call begins.
    pub on_tool_start: Option<Box<dyn Fn(&str, &Value) + Send + Sync>>,
    /// Called when a tool call finishes.
    pub on_tool_complete: Option<Box<dyn Fn(&str, &str) + Send + Sync>>,
    /// Called for each stream delta.
    pub on_stream_delta: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called after each completed LLM step (full response assembled).
    pub on_step_complete: Option<Box<dyn Fn(u32) + Send + Sync>>,
}

/// Classify an API error for retry/failover decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorClass {
    Retryable,
    RateLimit,
    ContextOverflow,
    Auth,
    Fatal,
}

fn classify_error(err: &str) -> ErrorClass {
    let lower = err.to_lowercase();
    if lower.contains("rate limit") || lower.contains("429") || lower.contains("too many") {
        ErrorClass::RateLimit
    } else if lower.contains("context length") || lower.contains("maximum context")
        || lower.contains("token limit") || lower.contains("context_length_exceeded")
    {
        ErrorClass::ContextOverflow
    } else if lower.contains("401") || lower.contains("403") || lower.contains("unauthorized")
        || lower.contains("authentication")
    {
        ErrorClass::Auth
    } else if lower.contains("500") || lower.contains("502") || lower.contains("503")
        || lower.contains("timeout") || lower.contains("connection")
        || lower.contains("overloaded")
    {
        ErrorClass::Retryable
    } else {
        ErrorClass::Fatal
    }
}

/// Compute jittered exponential backoff delay.
fn jittered_backoff(attempt: u32, base_ms: u64, max_ms: u64) -> Duration {
    let exp = base_ms.saturating_mul(1u64 << attempt.min(10));
    let capped = exp.min(max_ms);
    let jitter = capped / 4;
    let delay = capped.saturating_sub(jitter / 2)
        + (rand_u64_range(0, jitter.max(1)));
    Duration::from_millis(delay)
}

fn rand_u64_range(min: u64, max: u64) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    let h = hasher.finish();
    if max <= min { min } else { min + h % (max - min) }
}

/// The main agent loop.
///
/// Owns the configuration, a tool registry, and an LLM provider.
/// Call `run()` or `run_stream()` to begin an autonomous loop.
pub struct AgentLoop {
    pub config: AgentConfig,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub interrupt: InterruptController,
    /// Optional memory manager for prefetch/sync/tool routing.
    pub memory_manager: Option<Arc<std::sync::Mutex<MemoryManager>>>,
    /// Optional plugin manager for lifecycle hooks.
    pub plugin_manager: Option<Arc<std::sync::Mutex<PluginManager>>>,
    /// Callbacks for progress reporting.
    pub callbacks: Arc<AgentCallbacks>,
    /// Sub-agent delegation depth (0 = root).
    pub delegate_depth: u32,
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
            memory_manager: None,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
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
            memory_manager: None,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
        }
    }

    /// Set the memory manager.
    pub fn with_memory(mut self, mm: Arc<std::sync::Mutex<MemoryManager>>) -> Self {
        self.memory_manager = Some(mm);
        self
    }

    /// Set the plugin manager.
    pub fn with_plugins(mut self, pm: Arc<std::sync::Mutex<PluginManager>>) -> Self {
        self.plugin_manager = Some(pm);
        self
    }

    /// Set the callbacks.
    pub fn with_callbacks(mut self, cb: AgentCallbacks) -> Self {
        self.callbacks = Arc::new(cb);
        self
    }

    /// Set the delegate depth.
    pub fn with_delegate_depth(mut self, depth: u32) -> Self {
        self.delegate_depth = depth;
        self
    }

    // -- Plugin hook helpers ------------------------------------------------

    fn invoke_hook(&self, hook: HookType, ctx_val: &Value) -> Vec<HookResult> {
        if let Some(ref pm) = self.plugin_manager {
            if let Ok(pm) = pm.lock() {
                return pm.invoke_hook(hook, ctx_val);
            }
        }
        Vec::new()
    }

    fn inject_hook_context(&self, results: &[HookResult], ctx: &mut ContextManager) {
        for r in results {
            if let HookResult::InjectContext(text) = r {
                ctx.add_message(Message::system(text));
            }
        }
    }

    // -- Memory helpers ----------------------------------------------------

    fn memory_prefetch(&self, query: &str, session_id: &str) -> String {
        if self.config.skip_memory {
            return String::new();
        }
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mm) = mm.lock() {
                return mm.prefetch_all(query, session_id);
            }
        }
        String::new()
    }

    fn memory_sync(&self, user: &str, assistant: &str, session_id: &str) {
        if self.config.skip_memory {
            return;
        }
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mm) = mm.lock() {
                mm.sync_all(user, assistant, session_id);
            }
        }
    }

    fn memory_on_turn_start(&self, turn: u32, message: &str) {
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mut mm) = mm.lock() {
                mm.on_turn_start(turn, message);
            }
        }
    }

    fn memory_system_prompt(&self) -> String {
        if self.config.skip_memory {
            return String::new();
        }
        if let Some(ref mm) = self.memory_manager {
            if let Ok(mm) = mm.lock() {
                return mm.build_system_prompt();
            }
        }
        String::new()
    }

    /// Build the full system prompt including personality, memory, and plugin context.
    fn build_system_prompt(&self) -> Option<String> {
        let base = self.config.system_prompt.as_deref()?;
        let mut parts = vec![base.to_string()];

        if let Some(ref personality) = self.config.personality {
            parts.push(format!("Personality: {personality}"));
        }

        let mem_block = self.memory_system_prompt();
        if !mem_block.is_empty() {
            parts.push(mem_block);
        }

        Some(parts.join("\n\n"))
    }

    // -- Retry-aware LLM call ---------------------------------------------

    fn call_llm_with_retry<'a>(
        &'a self,
        ctx: &'a ContextManager,
        tool_schemas: &'a [ToolSchema],
        model_override: Option<&'a str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<hermes_core::LlmResponse, AgentError>> + Send + 'a>> {
        Box::pin(self.call_llm_with_retry_inner(ctx, tool_schemas, model_override))
    }

    async fn call_llm_with_retry_inner(
        &self,
        ctx: &ContextManager,
        tool_schemas: &[ToolSchema],
        model_override: Option<&str>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        let model = model_override.unwrap_or(self.config.model.as_str());
        let retry = &self.config.retry;

        for attempt in 0..=retry.max_retries {
            let result = self
                .llm_provider
                .chat_completion(
                    ctx.get_messages(),
                    tool_schemas,
                    self.config.max_tokens,
                    self.config.temperature,
                    Some(model),
                    self.config.extra_body.as_ref(),
                )
                .await;

            match result {
                Ok(response) => return Ok(response),
                Err(e) => {
                    let err_str = e.to_string();
                    let class = classify_error(&err_str);
                    tracing::warn!(
                        attempt,
                        error_class = ?class,
                        "LLM API error: {}",
                        &err_str[..err_str.len().min(200)]
                    );

                    match class {
                        ErrorClass::Auth | ErrorClass::Fatal => {
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::ContextOverflow => {
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::RateLimit | ErrorClass::Retryable => {
                            if attempt >= retry.max_retries {
                                if let Some(ref fallback) = retry.fallback_model {
                                    if model != fallback.as_str() {
                                        tracing::info!(
                                            "All retries exhausted on {}. Trying fallback: {}",
                                            model,
                                            fallback
                                        );
                                        return self
                                            .call_llm_with_retry(ctx, tool_schemas, Some(fallback))
                                            .await;
                                    }
                                }
                                return Err(AgentError::LlmApi(err_str));
                            }
                            let delay = jittered_backoff(
                                attempt,
                                retry.base_delay_ms,
                                retry.max_delay_ms,
                            );
                            tracing::info!(
                                "Retrying in {}ms (attempt {}/{})",
                                delay.as_millis(),
                                attempt + 1,
                                retry.max_retries
                            );
                            sleep(delay).await;
                        }
                    }
                }
            }
        }
        unreachable!()
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
        let session_id = self.config.session_id.as_deref().unwrap_or("");

        // Build and inject system prompt
        if let Some(system_content) = self.build_system_prompt() {
            ctx.add_message(Message::system(&system_content));
        }

        // Add initial messages
        for msg in messages {
            ctx.add_message(msg);
        }
        self.hydrate_todo_store(&ctx);

        // Memory prefetch for first user message
        let first_user = ctx.get_messages().iter()
            .filter(|m| matches!(m.role, hermes_core::MessageRole::User))
            .last()
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let mem_ctx = self.memory_prefetch(&first_user, session_id);
        if !mem_ctx.is_empty() {
            ctx.add_message(Message::system(&mem_ctx));
        }

        // Determine which tools to expose
        let tool_schemas: Vec<ToolSchema> = tools
            .unwrap_or_else(|| self.tool_registry.schemas());

        let mut total_turns: u32 = 0;
        let mut _total_api_time_ms: u64 = 0;
        let mut _total_tool_time_ms: u64 = 0;
        let mut accumulated_usage: Option<UsageStats> = None;
        let mut session_cost_usd: f64 = 0.0;
        let mut cost_warned = false;
        let mut active_model_override: Option<String> = None;
        let mut last_checkpoint_messages: Option<Vec<Message>> = None;

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
            tracing::debug!("Agent turn {}", total_turns);

            if self.config.checkpoint_interval_turns > 0
                && (total_turns - 1) % self.config.checkpoint_interval_turns == 0
            {
                last_checkpoint_messages = Some(ctx.get_messages().to_vec());
            }

            // Notify memory + plugins of new turn
            self.memory_on_turn_start(total_turns, "");

            // Memory sync at flush interval
            if total_turns % self.config.memory_flush_interval == 0 && total_turns > 0 {
                let msgs = ctx.get_messages();
                let (u, a) = extract_last_user_assistant(msgs);
                self.memory_sync(&u, &a, session_id);
            }

            // Inject budget warning when close to the turn limit
            if let Some(warning) = self.get_budget_warning(total_turns) {
                tracing::info!("{}", warning);
                ctx.add_message(Message::system(&warning));
            }

            // --- Pre-LLM hook ---
            let active_model = active_model_override
                .as_deref()
                .unwrap_or(self.config.model.as_str());
            let hook_ctx = serde_json::json!({"turn": total_turns, "model": active_model});
            let pre_results = self.invoke_hook(HookType::PreLlmCall, &hook_ctx);
            self.inject_hook_context(&pre_results, &mut ctx);

            // --- LLM API call with retry ---
            let api_start = Instant::now();
            let response = self
                .call_llm_with_retry(&ctx, &tool_schemas, active_model_override.as_deref())
                .await?;
            let api_elapsed = api_start.elapsed().as_millis() as u64;
            _total_api_time_ms += api_elapsed;

            // --- Post-LLM hook ---
            let post_ctx = serde_json::json!({
                "turn": total_turns,
                "api_time_ms": api_elapsed,
                "has_tool_calls": response.message.tool_calls.as_ref().map_or(false, |tc| !tc.is_empty()),
            });
            let post_results = self.invoke_hook(HookType::PostLlmCall, &post_ctx);
            self.inject_hook_context(&post_results, &mut ctx);

            // Accumulate usage
            if let Some(ref usage) = response.usage {
                accumulated_usage = Some(merge_usage(accumulated_usage, usage));
                if let Some(cost) =
                    estimate_usage_cost_usd(usage, response.model.as_str(), &self.config)
                {
                    session_cost_usd += cost;
                }
            }

            if let Some(limit) = self.config.max_cost_usd {
                if !cost_warned && session_cost_usd >= limit * self.config.cost_guard_degrade_at_ratio {
                    cost_warned = true;
                    if active_model_override.is_none() {
                        if let Some(model) = self.resolve_cost_degrade_model() {
                            active_model_override = Some(model.clone());
                            ctx.add_message(Message::system(format!(
                                "Cost guard: session spend is now ${:.4}/${:.4}. Switching to cheaper model `{}`.",
                                session_cost_usd, limit, model
                            )));
                        } else {
                            ctx.add_message(Message::system(format!(
                                "Cost guard warning: session spend is now ${:.4}/${:.4}.",
                                session_cost_usd, limit
                            )));
                        }
                    }
                }
                if session_cost_usd >= limit {
                    ctx.add_message(Message::system(format!(
                        "Cost guard tripped: session spend ${:.4} exceeded max_cost_usd ${:.4}. Stopping loop.",
                        session_cost_usd, limit
                    )));
                    return Ok(AgentResult {
                        messages: ctx.get_messages().to_vec(),
                        finished_naturally: false,
                        total_turns,
                        tool_errors,
                        usage: accumulated_usage,
                    });
                }
            }

            let assistant_msg = response.message.clone();
            let tool_calls = assistant_msg.tool_calls.clone();
            ctx.add_message(assistant_msg.clone());

            // Step complete callback
            if let Some(ref cb) = self.callbacks.on_step_complete {
                cb(total_turns);
            }

            // If no tool calls, the agent is done
            let tool_calls = match tool_calls {
                Some(calls) if !calls.is_empty() => calls,
                _ => {
                    tracing::debug!("No tool calls in response, finishing naturally");
                    // Final memory sync
                    let (u, a) = extract_last_user_assistant(ctx.get_messages());
                    self.memory_sync(&u, &a, session_id);
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
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
            }

            // Cap concurrent delegate_task calls
            self.cap_delegates(&mut tool_calls);

            // --- Pre-tool hook ---
            for tc in &tool_calls {
                let tc_ctx = serde_json::json!({
                    "tool": &tc.function.name,
                    "turn": total_turns,
                });
                self.invoke_hook(HookType::PreToolCall, &tc_ctx);

                if let Some(ref cb) = self.callbacks.on_tool_start {
                    let args: Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(Value::Null);
                    cb(&tc.function.name, &args);
                }
            }

            // --- Execute tool calls in parallel ---
            self.interrupt.check_interrupt()?;
            let tool_start = Instant::now();
            let results = self.execute_tool_calls(&tool_calls, total_turns, &mut tool_errors).await;
            let tool_elapsed = tool_start.elapsed().as_millis() as u64;
            _total_tool_time_ms += tool_elapsed;

            let turn_tool_error_count = results.iter().filter(|r| r.is_error).count() as u32;
            if self.config.rollback_on_tool_error_threshold > 0
                && turn_tool_error_count >= self.config.rollback_on_tool_error_threshold
            {
                if let Some(snapshot) = last_checkpoint_messages.clone() {
                    *ctx.get_messages_mut() = snapshot;
                    ctx.add_message(Message::system(format!(
                        "Auto-rollback: {} tool call(s) failed in one turn. Restored latest checkpoint and continuing.",
                        turn_tool_error_count
                    )));
                    continue;
                }
            }

            // --- Post-tool hook ---
            for (tc, res) in tool_calls.iter().zip(results.iter()) {
                let tc_ctx = serde_json::json!({
                    "tool": &tc.function.name,
                    "is_error": res.is_error,
                    "turn": total_turns,
                });
                self.invoke_hook(HookType::PostToolCall, &tc_ctx);

                if let Some(ref cb) = self.callbacks.on_tool_complete {
                    cb(&tc.function.name, &res.content);
                }
            }

            // Enforce budget on tool results
            let mut results = results;
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
                return self.run(messages, tools).await;
            }
        };

        let mut ctx = ContextManager::default_budget();
        let mut tool_errors: Vec<hermes_core::ToolErrorRecord> = Vec::new();
        let session_id = self.config.session_id.as_deref().unwrap_or("");

        if let Some(system_content) = self.build_system_prompt() {
            ctx.add_message(Message::system(&system_content));
        }

        for msg in messages {
            ctx.add_message(msg);
        }
        self.hydrate_todo_store(&ctx);

        // Memory prefetch
        let first_user = ctx.get_messages().iter()
            .filter(|m| matches!(m.role, hermes_core::MessageRole::User))
            .last()
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let mem_ctx = self.memory_prefetch(&first_user, session_id);
        if !mem_ctx.is_empty() {
            ctx.add_message(Message::system(&mem_ctx));
        }

        let tool_schemas: Vec<ToolSchema> = tools
            .unwrap_or_else(|| self.tool_registry.schemas());

        let mut total_turns: u32 = 0;
        let mut accumulated_usage: Option<UsageStats> = None;
        let mut session_cost_usd: f64 = 0.0;
        let mut cost_warned = false;
        let mut active_model_override: Option<String> = None;
        let mut last_checkpoint_messages: Option<Vec<Message>> = None;

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
            self.memory_on_turn_start(total_turns, "");

            if self.config.checkpoint_interval_turns > 0
                && (total_turns - 1) % self.config.checkpoint_interval_turns == 0
            {
                last_checkpoint_messages = Some(ctx.get_messages().to_vec());
            }

            if total_turns % self.config.memory_flush_interval == 0 && total_turns > 0 {
                let (u, a) = extract_last_user_assistant(ctx.get_messages());
                self.memory_sync(&u, &a, session_id);
            }

            if let Some(warning) = self.get_budget_warning(total_turns) {
                tracing::info!("{}", warning);
                ctx.add_message(Message::system(&warning));
            }

            // Pre-LLM hook
            let active_model = active_model_override
                .as_deref()
                .unwrap_or(self.config.model.as_str());
            let hook_ctx = serde_json::json!({"turn": total_turns, "model": active_model});
            let pre_results = self.invoke_hook(HookType::PreLlmCall, &hook_ctx);
            self.inject_hook_context(&pre_results, &mut ctx);

            // --- Streaming LLM call ---
            let mut stream = self.llm_provider.chat_completion_stream(
                ctx.get_messages(),
                &tool_schemas,
                self.config.max_tokens,
                self.config.temperature,
                Some(active_model),
                self.config.extra_body.as_ref(),
            );

            let mut content = String::new();
            let mut reasoning_content = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut last_usage: Option<UsageStats> = None;

            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result?;

                if let Some(ref delta) = chunk.delta {
                    if let Some(ref text) = delta.content {
                        content.push_str(text);
                        if let Some(ref cb) = self.callbacks.on_stream_delta {
                            cb(text);
                        }
                    }
                    // Accumulate reasoning/thinking tokens if present
                    if let Some(ref extra) = delta.extra {
                        if let Some(thinking) = extra.get("thinking").and_then(|v| v.as_str()) {
                            reasoning_content.push_str(thinking);
                            if let Some(ref cb) = self.callbacks.on_thinking {
                                cb(thinking);
                            }
                        }
                    }
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

                on_chunk(chunk);
            }

            if let Some(ref usage) = last_usage {
                accumulated_usage = Some(merge_usage(accumulated_usage, usage));
                if let Some(cost) = estimate_usage_cost_usd(usage, active_model, &self.config) {
                    session_cost_usd += cost;
                }
            }

            if let Some(limit) = self.config.max_cost_usd {
                if !cost_warned && session_cost_usd >= limit * self.config.cost_guard_degrade_at_ratio {
                    cost_warned = true;
                    if active_model_override.is_none() {
                        if let Some(model) = self.resolve_cost_degrade_model() {
                            active_model_override = Some(model.clone());
                            ctx.add_message(Message::system(format!(
                                "Cost guard: session spend is now ${:.4}/${:.4}. Switching to cheaper model `{}`.",
                                session_cost_usd, limit, model
                            )));
                        } else {
                            ctx.add_message(Message::system(format!(
                                "Cost guard warning: session spend is now ${:.4}/${:.4}.",
                                session_cost_usd, limit
                            )));
                        }
                    }
                }
                if session_cost_usd >= limit {
                    ctx.add_message(Message::system(format!(
                        "Cost guard tripped: session spend ${:.4} exceeded max_cost_usd ${:.4}. Stopping loop.",
                        session_cost_usd, limit
                    )));
                    return Ok(AgentResult {
                        messages: ctx.get_messages().to_vec(),
                        finished_naturally: false,
                        total_turns,
                        tool_errors,
                        usage: accumulated_usage,
                    });
                }
            }

            // Post-LLM hook
            let post_ctx = serde_json::json!({
                "turn": total_turns,
                "has_tool_calls": !tool_calls.is_empty(),
            });
            self.invoke_hook(HookType::PostLlmCall, &post_ctx);

            // Build assistant message
            let assistant_msg = if tool_calls.is_empty() || tool_calls.iter().all(|tc| tc.function.name.is_empty()) {
                Message::assistant(&content)
            } else {
                let content_opt = if content.is_empty() { None } else { Some(content.clone()) };
                Message::assistant_with_tool_calls(content_opt, tool_calls.clone())
            };

            ctx.add_message(assistant_msg);

            if let Some(ref cb) = self.callbacks.on_step_complete {
                cb(total_turns);
            }

            let tool_calls: Vec<ToolCall> = tool_calls
                .into_iter()
                .filter(|tc| !tc.function.name.is_empty())
                .collect();

            if tool_calls.is_empty() {
                let (u, a) = extract_last_user_assistant(ctx.get_messages());
                self.memory_sync(&u, &a, session_id);
                return Ok(AgentResult {
                    messages: ctx.get_messages().to_vec(),
                    finished_naturally: true,
                    total_turns,
                    tool_errors,
                    usage: accumulated_usage,
                });
            }

            let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
            }
            self.cap_delegates(&mut tool_calls);

            // Pre-tool hooks + callbacks
            for tc in &tool_calls {
                let tc_ctx = serde_json::json!({"tool": &tc.function.name, "turn": total_turns});
                self.invoke_hook(HookType::PreToolCall, &tc_ctx);
                if let Some(ref cb) = self.callbacks.on_tool_start {
                    let args: Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(Value::Null);
                    cb(&tc.function.name, &args);
                }
            }

            let mut results = self.execute_tool_calls(&tool_calls, total_turns, &mut tool_errors).await;

            let turn_tool_error_count = results.iter().filter(|r| r.is_error).count() as u32;
            if self.config.rollback_on_tool_error_threshold > 0
                && turn_tool_error_count >= self.config.rollback_on_tool_error_threshold
            {
                if let Some(snapshot) = last_checkpoint_messages.clone() {
                    *ctx.get_messages_mut() = snapshot;
                    ctx.add_message(Message::system(format!(
                        "Auto-rollback: {} tool call(s) failed in one turn. Restored latest checkpoint and continuing.",
                        turn_tool_error_count
                    )));
                    continue;
                }
            }

            // Post-tool hooks + callbacks
            for (tc, res) in tool_calls.iter().zip(results.iter()) {
                let tc_ctx = serde_json::json!({"tool": &tc.function.name, "is_error": res.is_error, "turn": total_turns});
                self.invoke_hook(HookType::PostToolCall, &tc_ctx);
                if let Some(ref cb) = self.callbacks.on_tool_complete {
                    cb(&tc.function.name, &res.content);
                }
            }

            budget::enforce_budget(&mut results, &self.config.budget);

            for result in results {
                ctx.add_message(Message::tool_result(&result.tool_call_id, &result.content));
            }
            self.spawn_background_review(total_turns, ctx.get_messages().to_vec());

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

    /// Resolve the model used for automatic degradation when nearing
    /// `max_cost_usd`.
    fn resolve_cost_degrade_model(&self) -> Option<String> {
        if let Some(ref m) = self.config.cost_guard_degrade_model {
            if !m.trim().is_empty() {
                return Some(m.trim().to_string());
            }
        }
        if let Some(ref m) = self.config.retry.fallback_model {
            if !m.trim().is_empty() {
                return Some(m.trim().to_string());
            }
        }
        if self.config.model.trim() != "openai:gpt-4o-mini" {
            return Some("openai:gpt-4o-mini".to_string());
        }
        None
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

    /// Cap concurrent delegate_task calls based on config.
    fn cap_delegates(&self, tool_calls: &mut Vec<ToolCall>) {
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

/// Extract the last user and assistant content from a message slice for memory sync.
fn extract_last_user_assistant(messages: &[Message]) -> (String, String) {
    let user = messages.iter().rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let assistant = messages.iter().rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    (user, assistant)
}

fn default_model_cost_per_million(model: &str) -> Option<(f64, f64)> {
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") || m.contains("4.1-mini") || m.contains("haiku") {
        return Some((0.15, 0.60));
    }
    if m.contains("gpt-4o") || m.contains("4.1") || m.contains("sonnet") {
        return Some((2.5, 10.0));
    }
    if m.contains("o3") {
        return Some((10.0, 40.0));
    }
    None
}

fn estimate_usage_cost_usd(
    usage: &UsageStats,
    model: &str,
    config: &AgentConfig,
) -> Option<f64> {
    if let Some(v) = usage.estimated_cost {
        return Some(v.max(0.0));
    }
    let (in_pm, out_pm) = match (
        config.prompt_cost_per_million_usd,
        config.completion_cost_per_million_usd,
    ) {
        (Some(i), Some(o)) => (i, o),
        _ => default_model_cost_per_million(model)?,
    };
    let prompt_cost = (usage.prompt_tokens as f64 / 1_000_000.0) * in_pm;
    let completion_cost = (usage.completion_tokens as f64 / 1_000_000.0) * out_pm;
    Some(prompt_cost + completion_cost)
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
        assert_eq!(config.api_mode, ApiMode::ChatCompletions);
        assert_eq!(config.retry.max_retries, 3);
        assert!(config.session_id.is_none());
        assert!(!config.skip_memory);
        assert!(config.max_cost_usd.is_none());
        assert_eq!(config.cost_guard_degrade_at_ratio, 0.8);
        assert!(config.cost_guard_degrade_model.is_none());
        assert_eq!(config.checkpoint_interval_turns, 3);
        assert_eq!(config.rollback_on_tool_error_threshold, 3);
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

    #[test]
    fn test_estimate_usage_cost_prefers_reported_estimate() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1000,
            completion_tokens: 1000,
            total_tokens: 2000,
            estimated_cost: Some(0.42),
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o", &cfg).unwrap();
        assert!((cost - 0.42).abs() < 1e-9);
    }

    #[test]
    fn test_estimate_usage_cost_uses_model_fallback_table() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            total_tokens: 2_000_000,
            estimated_cost: None,
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o-mini", &cfg).unwrap();
        assert!((cost - 0.75).abs() < 1e-9);
    }
}