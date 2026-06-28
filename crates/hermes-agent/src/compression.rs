//! Automatic context-window compression for long conversations.
//!
//! Port of Python `agent.context_compressor.ContextCompressor` (738 LoC).
//! See the Python source for a detailed algorithm description; the Rust
//! version mirrors it phase-for-phase:
//!
//! 1. **Prune** old `tool` results (cheap pre-pass, no LLM call)
//! 2. **Protect** head messages (system prompt + first exchange)
//! 3. **Find tail boundary** by token budget (~20K tokens of recent context)
//! 4. **Summarise** middle turns via the [`AuxiliaryClient`] using a
//!    structured Goal / Progress / Decisions / Files / Next-Steps prompt
//! 5. **Sanitise** orphaned tool_call / tool_result pairs so the API never
//!    receives mismatched IDs
//!
//! Iterative summary updates preserve information across multiple
//! compactions: the previous summary is fed back into the prompt and the
//! model is told to update rather than rewrite.

use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use hermes_core::{AgentError, LlmProvider, Message, MessageRole};
use hermes_intelligence::auxiliary::{
    AuxiliaryClient, AuxiliaryError, AuxiliaryRequest, AuxiliaryTask,
};
use regex::Regex;

// ---------------------------------------------------------------------------
// Constants — kept aligned with Python module-level globals.
// ---------------------------------------------------------------------------

/// Banner prepended to every compaction summary so a downstream agent knows
/// some history has been compacted into prose.
pub const SUMMARY_PREFIX: &str =
    "[CONTEXT COMPACTION] Earlier turns in this conversation were compacted \
     to save context space. The summary below describes work that was \
     already completed, and the current session state may still reflect \
     that work (for example, files may already be changed). Use the summary \
     and the current state to continue from where things left off, and \
     avoid repeating work:";

/// Older banner from v1 — stripped before re-applying [`SUMMARY_PREFIX`] so
/// iterative updates don't accumulate prefixes.
pub const LEGACY_SUMMARY_PREFIX: &str = "[CONTEXT SUMMARY]:";

/// Placeholder substituted for old, oversized tool results.
pub const PRUNED_TOOL_PLACEHOLDER: &str = "[Old tool output cleared to save context space]";

const MIN_SUMMARY_TOKENS: u64 = 2_000;
const SUMMARY_RATIO: f64 = 0.20;
const SUMMARY_TOKENS_CEILING: u64 = 12_000;
const CHARS_PER_TOKEN: usize = 4;
const SUMMARY_FAILURE_COOLDOWN: Duration = Duration::from_secs(600);

// Truncation limits for the summariser input (per Python class constants).
const CONTENT_MAX: usize = 6_000;
const CONTENT_HEAD: usize = 4_000;
const CONTENT_TAIL: usize = 1_500;
const TOOL_ARGS_MAX: usize = 1_500;
const TOOL_ARGS_HEAD: usize = 1_200;

static SUMMARY_SECRET_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)(api[_-]?key|token|secret|password)\s*[:=]\s*[A-Za-z0-9._\-]{8,}")
            .unwrap(),
        Regex::new(r"(?i)authorization\s*:\s*bearer\s+[A-Za-z0-9._\-]{8,}").unwrap(),
        Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----")
            .unwrap(),
    ]
});

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by [`ContextCompressor`] operations.
#[derive(Debug, thiserror::Error)]
pub enum CompressionError {
    /// The auxiliary call could not produce a summary and the cooldown is
    /// active. The caller should fall through to the static fallback marker.
    #[error("summarisation cooldown active for another {0:?}")]
    CooldownActive(Duration),

    /// The underlying auxiliary client failed in a non-retryable way.
    #[error("auxiliary client error: {0}")]
    Auxiliary(#[from] AuxiliaryError),

    /// The LLM provider returned an error during the summarisation call.
    #[error("LLM provider error: {0}")]
    Llm(#[from] AgentError),
}

// ---------------------------------------------------------------------------
// Backwards-compatible helper kept from the old stub.
// ---------------------------------------------------------------------------

/// Build a compact summary text from older messages via an LLM provider.
///
/// Lightweight one-shot helper retained for callers that don't want the full
/// [`ContextCompressor`] state machine.
pub async fn summarize_messages_with_llm(
    provider: &dyn LlmProvider,
    messages: &[Message],
    model: Option<&str>,
) -> Result<String, AgentError> {
    let mut prompt_messages = Vec::with_capacity(2 + messages.len());
    prompt_messages.push(Message::system(
        "Summarize the conversation into concise bullets. Preserve facts, decisions, todos, file paths, and unresolved questions.",
    ));
    prompt_messages.push(Message::user(
        "Return only the summary text. Keep it under 3000 characters.",
    ));
    prompt_messages.extend_from_slice(messages);

    let resp = provider
        .chat_completion(&prompt_messages, &[], Some(700), Some(0.1), model, None)
        .await?;

    Ok(resp
        .message
        .content
        .unwrap_or_else(|| "[summary unavailable]".to_string()))
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Tunable options for [`ContextCompressor`]. All fields default to the
/// Python implementation's defaults.
#[derive(Debug, Clone)]
pub struct CompressorConfig {
    /// Total context window (in tokens) of the *primary* model the
    /// compressor protects.
    pub context_length: u64,
    /// Compression triggers when prompt tokens exceed this fraction of
    /// `context_length`.
    pub threshold_percent: f64,
    /// Number of head messages always preserved verbatim (system prompt +
    /// first turns).
    pub protect_first_n: usize,
    /// Hard minimum number of tail messages to protect; the token-budget
    /// cut may keep more.
    pub protect_last_n: usize,
    /// Fraction of `threshold_tokens` to spend on the summary itself.
    pub summary_target_ratio: f64,
    /// Optional override of the auxiliary model used for summarisation.
    pub summary_model_override: Option<String>,
    /// Suppress `tracing::info!` chatter — useful for batch / test runs.
    pub quiet_mode: bool,
}

impl Default for CompressorConfig {
    fn default() -> Self {
        Self {
            context_length: 200_000,
            threshold_percent: 0.50,
            protect_first_n: 3,
            protect_last_n: 20,
            summary_target_ratio: 0.20,
            summary_model_override: None,
            quiet_mode: false,
        }
    }
}

// ---------------------------------------------------------------------------
// ContextCompressor
// ---------------------------------------------------------------------------

/// Compresses conversation context when approaching the model's context
/// limit. Mirrors `agent.context_compressor.ContextCompressor`.
pub struct ContextCompressor {
    config: CompressorConfig,
    auxiliary: Arc<AuxiliaryClient>,

    // Derived budgets, fixed at construction time.
    threshold_tokens: u64,
    tail_token_budget: u64,
    max_summary_tokens: u64,

    // Dynamic state.
    compression_count: u64,
    last_prompt_tokens: u64,
    previous_summary: Option<String>,
    failure_cooldown_until: Option<Instant>,
    summary_model_fallen_back: bool,
    last_summary_error: Option<String>,
    last_summary_dropped_count: usize,
    last_summary_fallback_used: bool,
}

impl ContextCompressor {
    /// Create a compressor for `model` with the given configuration.
    ///
    /// Token budgets are derived eagerly so they don't change as the
    /// compressor is used. To re-target a different model, build a new
    /// instance.
    pub fn new(config: CompressorConfig, auxiliary: Arc<AuxiliaryClient>) -> Self {
        let summary_ratio = config.summary_target_ratio.clamp(0.10, 0.80);
        let threshold_tokens = (config.context_length as f64 * config.threshold_percent) as u64;
        let target_tokens = (threshold_tokens as f64 * summary_ratio) as u64;
        let max_summary_tokens =
            ((config.context_length as f64 * 0.05) as u64).min(SUMMARY_TOKENS_CEILING);

        if !config.quiet_mode {
            tracing::info!(
                "Context compressor initialised: context_length={} threshold={} ({:.0}%) target_ratio={:.0}% tail_budget={}",
                config.context_length,
                threshold_tokens,
                config.threshold_percent * 100.0,
                summary_ratio * 100.0,
                target_tokens,
            );
        }

        Self {
            config: CompressorConfig {
                summary_target_ratio: summary_ratio,
                ..config
            },
            auxiliary,
            threshold_tokens,
            tail_token_budget: target_tokens,
            max_summary_tokens,
            compression_count: 0,
            last_prompt_tokens: 0,
            previous_summary: None,
            failure_cooldown_until: None,
            summary_model_fallen_back: false,
            last_summary_error: None,
            last_summary_dropped_count: 0,
            last_summary_fallback_used: false,
        }
    }

    /// Total in-window threshold (tokens). Compression triggers above this.
    pub fn threshold_tokens(&self) -> u64 {
        self.threshold_tokens
    }

    /// How many compactions this instance has performed.
    pub fn compression_count(&self) -> u64 {
        self.compression_count
    }

    /// Last summary-generation error captured by this compressor instance.
    pub fn last_summary_error(&self) -> Option<&str> {
        self.last_summary_error.as_deref()
    }

    /// Number of historical messages dropped in the most recent fallback path.
    pub fn last_summary_dropped_count(&self) -> usize {
        self.last_summary_dropped_count
    }

    /// Whether the most recent `compress()` used static fallback text.
    pub fn last_summary_fallback_used(&self) -> bool {
        self.last_summary_fallback_used
    }

    /// Update the tracked usage from the latest response.
    pub fn update_from_usage(&mut self, prompt_tokens: u64) {
        self.last_prompt_tokens = prompt_tokens;
    }

    /// Compression trigger predicate.
    pub fn should_compress(&self, current_prompt_tokens: Option<u64>) -> bool {
        current_prompt_tokens.unwrap_or(self.last_prompt_tokens) >= self.threshold_tokens
    }

    fn rehydrate_previous_summary_from_messages(&mut self, messages: &[Message]) {
        if self.previous_summary.is_some() {
            return;
        }
        self.previous_summary = messages
            .iter()
            .filter_map(|msg| msg.content.as_deref())
            .filter_map(summary_body_from_handoff)
            .map(|body| body.trim().to_string())
            .rfind(|body| !body.is_empty());
    }

    // ------------------------------------------------------------------
    // Phase 1: tool-output pruning (cheap, no LLM call).
    // ------------------------------------------------------------------

    /// Replace the contents of old, oversized `tool` messages with a short
    /// placeholder. The most recent messages (within `protect_tail_tokens`,
    /// or the last `protect_tail_count` if no token budget supplied) are
    /// left untouched.
    ///
    /// Returns `(messages, pruned_count)`.
    pub fn prune_old_tool_results(
        &self,
        messages: &[Message],
        protect_tail_count: usize,
        protect_tail_tokens: Option<u64>,
    ) -> (Vec<Message>, usize) {
        if messages.is_empty() {
            return (Vec::new(), 0);
        }

        let n = messages.len();
        let mut result: Vec<Message> = messages.to_vec();
        let mut pruned = 0;

        let prune_boundary = if let Some(budget) = protect_tail_tokens.filter(|b| *b > 0) {
            // Token-budget walk backward.
            let mut accumulated: u64 = 0;
            let mut boundary = n;
            let min_protect = protect_tail_count.min(n.saturating_sub(1));
            for i in (0..n).rev() {
                let msg = &result[i];
                let mut msg_tokens = chars_to_tokens(content_len(msg)) + 10;
                if let Some(tcs) = msg.tool_calls.as_ref() {
                    for tc in tcs {
                        msg_tokens += chars_to_tokens(tc.function.arguments.len());
                    }
                }
                if accumulated + msg_tokens > budget && (n - i) >= min_protect {
                    boundary = i;
                    break;
                }
                accumulated += msg_tokens;
                boundary = i;
            }
            boundary.max(n.saturating_sub(min_protect))
        } else {
            n.saturating_sub(protect_tail_count)
        };

        for msg in result.iter_mut().take(prune_boundary) {
            if msg.role != MessageRole::Tool {
                continue;
            }
            let content = msg.content.as_deref().unwrap_or("");
            if content.is_empty() || content == PRUNED_TOOL_PLACEHOLDER {
                continue;
            }
            if content.len() > 200 {
                msg.content = Some(PRUNED_TOOL_PLACEHOLDER.to_string());
                pruned += 1;
            }
        }

        (result, pruned)
    }

    // ------------------------------------------------------------------
    // Phase 4: summary budget + serialisation.
    // ------------------------------------------------------------------

    fn compute_summary_budget(&self, turns: &[Message]) -> u64 {
        let content_tokens = estimate_messages_tokens(turns);
        let budget = (content_tokens as f64 * SUMMARY_RATIO) as u64;
        budget.clamp(
            MIN_SUMMARY_TOKENS,
            self.max_summary_tokens.max(MIN_SUMMARY_TOKENS),
        )
    }

    fn serialize_for_summary(&self, turns: &[Message]) -> String {
        let mut parts = Vec::with_capacity(turns.len());
        for msg in turns {
            let role = msg.role;
            let raw_content =
                redact_sensitive_summary_text(&msg.content.clone().unwrap_or_default());

            let content = truncate_middle(&raw_content, CONTENT_MAX, CONTENT_HEAD, CONTENT_TAIL);
            match role {
                MessageRole::Tool => {
                    let id = msg.tool_call_id.as_deref().unwrap_or("");
                    parts.push(format!("[TOOL RESULT {}]: {}", id, content));
                }
                MessageRole::Assistant => {
                    let mut body = content;
                    if let Some(tcs) = msg.tool_calls.as_ref() {
                        if !tcs.is_empty() {
                            let mut tc_parts = Vec::with_capacity(tcs.len());
                            for tc in tcs {
                                let safe_args =
                                    redact_sensitive_summary_text(&tc.function.arguments);
                                let args = if safe_args.len() > TOOL_ARGS_MAX {
                                    format!("{}...", &safe_args[..TOOL_ARGS_HEAD])
                                } else {
                                    safe_args
                                };
                                tc_parts.push(format!("  {}({})", tc.function.name, args));
                            }
                            body.push_str("\n[Tool calls:\n");
                            body.push_str(&tc_parts.join("\n"));
                            body.push_str("\n]");
                        }
                    }
                    parts.push(format!("[ASSISTANT]: {}", body));
                }
                _ => {
                    parts.push(format!(
                        "[{}]: {}",
                        role_label(role).to_uppercase(),
                        content
                    ));
                }
            }
        }
        parts.join("\n\n")
    }

    fn build_summary_prompt(&self, content_block: &str, summary_budget: u64) -> String {
        if let Some(prev) = self.previous_summary.as_ref() {
            let prev = redact_sensitive_summary_text(prev);
            format!(
                "You are updating a context compaction summary. A previous compaction produced the summary below. \
New conversation turns have occurred since then and need to be incorporated.\n\n\
PREVIOUS SUMMARY:\n{prev}\n\nNEW TURNS TO INCORPORATE:\n{content_block}\n\n\
Update the summary using this exact structure. PRESERVE all existing information that is still relevant. \
ADD new progress. Move items from \"In Progress\" to \"Done\" when completed. Remove information only if it is clearly obsolete.\n\n\
{COMPACTION_TEMPLATE}\n\n\
Target ~{summary_budget} tokens. Be specific — include file paths, command outputs, error messages, and concrete values rather than vague descriptions.\n\n\
Write only the summary body. Do not include any preamble or prefix."
            )
        } else {
            format!(
                "Create a structured handoff summary for a later assistant that will continue this conversation after earlier turns are compacted.\n\n\
TURNS TO SUMMARIZE:\n{content_block}\n\nUse this exact structure:\n\n{COMPACTION_TEMPLATE}\n\n\
Target ~{summary_budget} tokens. Be specific — include file paths, command outputs, error messages, and concrete values rather than vague descriptions. \
The goal is to prevent the next assistant from repeating work or losing important details.\n\n\
Write only the summary body. Do not include any preamble or prefix."
            )
        }
    }

    /// Generate a structured summary of the supplied turns using the
    /// auxiliary client.
    ///
    /// Returns `Ok(Some(summary))` on success, `Ok(None)` when the cooldown
    /// is active, and `Err(...)` for hard auxiliary failures (which also
    /// arm the cooldown for `SUMMARY_FAILURE_COOLDOWN`).
    pub async fn generate_summary(
        &mut self,
        turns: &[Message],
    ) -> Result<Option<String>, CompressionError> {
        self.last_summary_error = None;
        if let Some(deadline) = self.failure_cooldown_until {
            let now = Instant::now();
            if now < deadline {
                return Err(CompressionError::CooldownActive(deadline - now));
            }
            self.failure_cooldown_until = None;
        }

        let budget = self.compute_summary_budget(turns);
        let block = self.serialize_for_summary(turns);
        let prompt = self.build_summary_prompt(&block, budget);

        let mut retry_on_main_pending =
            self.config.summary_model_override.is_some() && !self.summary_model_fallen_back;
        loop {
            let mut request = AuxiliaryRequest::new(
                AuxiliaryTask::Compression,
                vec![Message::user(prompt.clone())],
            )
            .with_max_tokens((budget * 2) as u32);
            if let Some(model) = self.config.summary_model_override.as_ref() {
                request = request.with_model(model.clone());
            }

            match self.auxiliary.call(request).await {
                Ok(resp) => {
                    let body = resp
                        .text()
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                    if body.is_empty() {
                        self.last_summary_error = Some("empty summary response".to_string());
                        self.arm_cooldown();
                        return Ok(None);
                    } else {
                        self.previous_summary = Some(body.clone());
                        self.failure_cooldown_until = None;
                        self.summary_model_fallen_back = false;
                        self.last_summary_error = None;
                        return Ok(Some(with_summary_prefix(&body)));
                    }
                }
                Err(err) => {
                    let err_text = err.to_string();
                    if retry_on_main_pending {
                        retry_on_main_pending = false;
                        self.summary_model_fallen_back = true;
                        if let Some(model) = self.config.summary_model_override.as_ref() {
                            if !self.config.quiet_mode {
                                tracing::warn!(
                                    "Summary model '{}' failed ({}). Retrying on main model before giving up.",
                                    model,
                                    err_text
                                );
                            }
                        }
                        self.config.summary_model_override = None;
                        self.failure_cooldown_until = None;
                        continue;
                    }

                    self.last_summary_error = Some(err_text.clone());
                    self.arm_cooldown();
                    if !self.config.quiet_mode {
                        tracing::warn!(
                            "Failed to generate context summary: {err}. \
                             Further summary attempts paused for {:?}.",
                            SUMMARY_FAILURE_COOLDOWN
                        );
                    }
                    return Err(CompressionError::Auxiliary(err));
                }
            }
        }
    }

    fn arm_cooldown(&mut self) {
        self.failure_cooldown_until = Some(Instant::now() + SUMMARY_FAILURE_COOLDOWN);
    }

    // ------------------------------------------------------------------
    // Phase 5: tool-pair sanitiser.
    // ------------------------------------------------------------------

    /// Fix orphaned tool_call / tool_result pairs after compression.
    pub fn sanitize_tool_pairs(&self, messages: Vec<Message>) -> Vec<Message> {
        let mut surviving_call_ids = std::collections::HashSet::new();
        for msg in &messages {
            if msg.role == MessageRole::Assistant {
                if let Some(tcs) = msg.tool_calls.as_ref() {
                    for tc in tcs {
                        if !tc.id.is_empty() {
                            surviving_call_ids.insert(tc.id.clone());
                        }
                    }
                }
            }
        }

        let mut result_call_ids = std::collections::HashSet::new();
        for msg in &messages {
            if msg.role == MessageRole::Tool {
                if let Some(cid) = msg.tool_call_id.as_ref() {
                    result_call_ids.insert(cid.clone());
                }
            }
        }

        let orphaned_results: std::collections::HashSet<_> = result_call_ids
            .difference(&surviving_call_ids)
            .cloned()
            .collect();
        let missing_results: std::collections::HashSet<_> = surviving_call_ids
            .difference(&result_call_ids)
            .cloned()
            .collect();

        let messages: Vec<Message> = messages
            .into_iter()
            .filter(|m| {
                !(m.role == MessageRole::Tool
                    && m.tool_call_id
                        .as_ref()
                        .map(|cid| orphaned_results.contains(cid))
                        .unwrap_or(false))
            })
            .collect();

        if !orphaned_results.is_empty() && !self.config.quiet_mode {
            tracing::info!(
                "Compression sanitiser: removed {} orphaned tool result(s)",
                orphaned_results.len()
            );
        }

        if missing_results.is_empty() {
            return messages;
        }

        let mut patched = Vec::with_capacity(messages.len() + missing_results.len());
        for msg in messages.into_iter() {
            let role = msg.role;
            let tool_calls = msg.tool_calls.clone();
            patched.push(msg);
            if role == MessageRole::Assistant {
                if let Some(tcs) = tool_calls {
                    for tc in tcs {
                        if missing_results.contains(&tc.id) {
                            patched.push(Message {
                                role: MessageRole::Tool,
                                content: Some(
                                    "[Result from earlier conversation — see context summary above]"
                                        .into(),
                                ),
                                tool_calls: None,
                                tool_call_id: Some(tc.id.clone()),
                                name: None,
                                reasoning_content: None,
                                anthropic_content_blocks: None,
                                cache_control: None,
                            });
                        }
                    }
                }
            }
        }

        if !self.config.quiet_mode {
            tracing::info!(
                "Compression sanitiser: added {} stub tool result(s)",
                missing_results.len()
            );
        }

        patched
    }

    // ------------------------------------------------------------------
    // Boundary alignment helpers.
    // ------------------------------------------------------------------

    fn align_boundary_forward(&self, messages: &[Message], idx: usize) -> usize {
        let mut idx = idx;
        while idx < messages.len() && messages[idx].role == MessageRole::Tool {
            idx += 1;
        }
        idx
    }

    fn align_boundary_backward(&self, messages: &[Message], idx: usize) -> usize {
        if idx == 0 || idx >= messages.len() {
            return idx;
        }
        let mut check = idx as isize - 1;
        while check >= 0 && messages[check as usize].role == MessageRole::Tool {
            check -= 1;
        }
        if check >= 0 {
            let candidate = &messages[check as usize];
            if candidate.role == MessageRole::Assistant
                && candidate
                    .tool_calls
                    .as_ref()
                    .map(|t| !t.is_empty())
                    .unwrap_or(false)
            {
                return check as usize;
            }
        }
        idx
    }

    fn find_tail_cut_by_tokens(
        &self,
        messages: &[Message],
        head_end: usize,
        token_budget: Option<u64>,
    ) -> usize {
        let budget = token_budget.unwrap_or(self.tail_token_budget);
        let n = messages.len();
        let min_tail = if n > head_end + 1 {
            3.min(n - head_end - 1)
        } else {
            0
        };
        let soft_ceiling = (budget as f64 * 1.5) as u64;
        let mut accumulated: u64 = 0;
        let mut cut_idx = n;

        for i in (head_end..n).rev() {
            let msg = &messages[i];
            let mut msg_tokens = chars_to_tokens(content_len(msg)) + 10;
            if let Some(tcs) = msg.tool_calls.as_ref() {
                for tc in tcs {
                    msg_tokens += chars_to_tokens(tc.function.arguments.len());
                }
            }
            if accumulated + msg_tokens > soft_ceiling && (n - i) >= min_tail {
                break;
            }
            accumulated += msg_tokens;
            cut_idx = i;
        }

        let fallback_cut = n.saturating_sub(min_tail);
        if cut_idx > fallback_cut {
            cut_idx = fallback_cut;
        }
        if cut_idx <= head_end {
            cut_idx = fallback_cut.max(head_end + 1);
        }
        let cut_idx = self.align_boundary_backward(messages, cut_idx);
        cut_idx.max(head_end + 1)
    }

    // ------------------------------------------------------------------
    // Main entry point.
    // ------------------------------------------------------------------

    /// Run a full compression pass on `messages`.
    ///
    /// `current_tokens` is used purely for logging — pass `None` to fall
    /// back to the last tracked `prompt_tokens` value.
    pub async fn compress(
        &mut self,
        messages: Vec<Message>,
        current_tokens: Option<u64>,
    ) -> Vec<Message> {
        self.last_summary_error = None;
        self.last_summary_dropped_count = 0;
        self.last_summary_fallback_used = false;

        let n_messages = messages.len();
        let min_for_compress = self.config.protect_first_n + 3 + 1;
        if n_messages <= min_for_compress {
            if !self.config.quiet_mode {
                tracing::warn!(
                    "Cannot compress: only {} messages (need > {})",
                    n_messages,
                    min_for_compress,
                );
            }
            return messages;
        }

        let display_tokens = current_tokens
            .or(Some(self.last_prompt_tokens))
            .filter(|t| *t > 0)
            .unwrap_or_else(|| estimate_messages_tokens(&messages));

        // Phase 1: prune old tool results.
        let (messages, pruned_count) = self.prune_old_tool_results(
            &messages,
            self.config.protect_last_n,
            Some(self.tail_token_budget),
        );
        if pruned_count > 0 && !self.config.quiet_mode {
            tracing::info!(
                "Pre-compression: pruned {} old tool result(s)",
                pruned_count
            );
        }
        self.rehydrate_previous_summary_from_messages(&messages);

        // Phase 2: determine boundaries.
        let compress_start = self.align_boundary_forward(&messages, self.config.protect_first_n);
        let compress_end = self.find_tail_cut_by_tokens(&messages, compress_start, None);
        if compress_start >= compress_end {
            return messages;
        }
        let turns_to_summarize: Vec<Message> = messages[compress_start..compress_end]
            .iter()
            .filter(|msg| !is_summary_handoff_message(msg))
            .cloned()
            .collect();
        if turns_to_summarize.is_empty() {
            return messages;
        }

        if !self.config.quiet_mode {
            let tail_msgs = n_messages - compress_end;
            tracing::info!(
                "Context compression triggered ({} tokens >= {} threshold)",
                display_tokens,
                self.threshold_tokens
            );
            tracing::info!(
                "Summarising turns {}-{} ({} turns), protecting {} head + {} tail messages",
                compress_start + 1,
                compress_end,
                turns_to_summarize.len(),
                compress_start,
                tail_msgs
            );
        }

        // Phase 3: generate structured summary.
        let summary_opt: Option<String> = match self.generate_summary(&turns_to_summarize).await {
            Ok(s) => s,
            Err(err) => {
                if !self.config.quiet_mode {
                    tracing::warn!("Summary generation error (using fallback): {err}");
                }
                None
            }
        };

        // Phase 4: assemble the compressed message list.
        let mut compressed: Vec<Message> = Vec::with_capacity(messages.len());
        for i in 0..compress_start {
            let mut msg = messages[i].clone();
            if i == 0 && msg.role == MessageRole::System && self.compression_count == 0 {
                let extra = "\n\n[Note: Some earlier conversation turns have been compacted into a handoff summary to preserve context space. The current session state may still reflect earlier work, so build on that summary and state rather than re-doing work.]";
                let new_content = msg.content.unwrap_or_default() + extra;
                msg.content = Some(new_content);
            }
            compressed.push(msg);
        }

        let n_dropped = compress_end - compress_start;
        let summary = summary_opt.unwrap_or_else(|| {
            if !self.config.quiet_mode {
                tracing::warn!(
                    "Summary generation failed — inserting static fallback context marker"
                );
            }
            self.last_summary_dropped_count = n_dropped;
            self.last_summary_fallback_used = true;
            format!(
                "{SUMMARY_PREFIX}\nSummary generation was unavailable. {n_dropped} \
                 message(s) were removed to free context space but could not be \
                 summarized. The removed messages contained earlier work in this session. \
                 Continue based on the recent messages below and the current state of \
                 any files or resources."
            )
        });

        let last_head_role = compress_start
            .checked_sub(1)
            .map(|i| messages[i].role)
            .unwrap_or(MessageRole::User);
        let first_tail_role = if compress_end < n_messages {
            messages[compress_end].role
        } else {
            MessageRole::User
        };

        let mut summary_role =
            if matches!(last_head_role, MessageRole::Assistant | MessageRole::Tool) {
                MessageRole::User
            } else {
                MessageRole::Assistant
            };
        let mut merge_summary_into_tail = false;
        if summary_role == first_tail_role {
            let flipped = if summary_role == MessageRole::User {
                MessageRole::Assistant
            } else {
                MessageRole::User
            };
            if flipped != last_head_role {
                summary_role = flipped;
            } else {
                merge_summary_into_tail = true;
            }
        }
        if !merge_summary_into_tail {
            compressed.push(Message {
                role: summary_role,
                content: Some(summary.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                anthropic_content_blocks: None,
                cache_control: None,
            });
        }

        for i in compress_end..n_messages {
            let mut msg = messages[i].clone();
            if merge_summary_into_tail && i == compress_end {
                let original = msg.content.unwrap_or_default();
                msg.content = Some(format!("{summary}\n\n{original}"));
                merge_summary_into_tail = false;
            }
            compressed.push(msg);
        }

        self.compression_count += 1;
        let compressed = self.sanitize_tool_pairs(compressed);

        if !self.config.quiet_mode {
            let new_estimate = estimate_messages_tokens(&compressed);
            let saved_estimate = display_tokens.saturating_sub(new_estimate);
            tracing::info!(
                "Compressed: {} -> {} messages (~{} tokens saved)",
                n_messages,
                compressed.len(),
                saved_estimate
            );
            tracing::info!("Compression #{} complete", self.compression_count);
        }

        // Quiet the unused `messages` lint when min_tail forces a no-op above.
        drop(messages);
        compressed
    }
}

// ---------------------------------------------------------------------------
// Free helpers.
// ---------------------------------------------------------------------------

const COMPACTION_TEMPLATE: &str = "## Goal\n[What the user is trying to accomplish]\n\n\
## Constraints & Preferences\n[User preferences, coding style, constraints, important decisions]\n\n\
## Progress\n### Done\n[Completed work — include specific file paths, commands run, results obtained]\n### In Progress\n[Work currently underway]\n### Blocked\n[Any blockers or issues encountered]\n\n\
## Key Decisions\n[Important technical decisions and why they were made]\n\n\
## Relevant Files\n[Files read, modified, or created — with brief note on each]\n\n\
## Next Steps\n[What needs to happen next to continue the work]\n\n\
## Critical Context\n[Any specific values, error messages, configuration details, or data that would be lost without explicit preservation]\n\n\
## Tools & Patterns\n[Which tools were used, how they were used effectively, and any tool-specific discoveries]";

/// Normalise summary text to the current compaction handoff format,
/// stripping any legacy / current prefix already present.
pub fn with_summary_prefix(summary: &str) -> String {
    let trimmed = summary.trim();
    let stripped = if let Some(rest) = trimmed.strip_prefix(LEGACY_SUMMARY_PREFIX) {
        rest.trim_start()
    } else if let Some(rest) = trimmed.strip_prefix(SUMMARY_PREFIX) {
        rest.trim_start()
    } else {
        trimmed
    };
    if stripped.is_empty() {
        SUMMARY_PREFIX.to_string()
    } else {
        format!("{SUMMARY_PREFIX}\n{stripped}")
    }
}

fn summary_body_from_handoff(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if let Some(rest) = trimmed.strip_prefix(SUMMARY_PREFIX) {
        Some(rest.trim_start().to_string())
    } else {
        trimmed
            .strip_prefix(LEGACY_SUMMARY_PREFIX)
            .map(|rest| rest.trim_start().to_string())
    }
}

fn is_summary_handoff_message(msg: &Message) -> bool {
    msg.content
        .as_deref()
        .and_then(summary_body_from_handoff)
        .is_some()
}

fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn redact_sensitive_summary_text(raw: &str) -> String {
    let mut out = raw.to_string();
    for pattern in SUMMARY_SECRET_PATTERNS.iter() {
        out = pattern.replace_all(&out, "[redacted]").to_string();
    }
    out
}

fn content_len(msg: &Message) -> usize {
    msg.content.as_deref().map(str::len).unwrap_or(0)
}

fn chars_to_tokens(chars: usize) -> u64 {
    (chars / CHARS_PER_TOKEN) as u64
}

fn truncate_middle(text: &str, max: usize, head: usize, tail: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let head_str = take_chars(text, head);
    let tail_str = take_chars_back(text, tail);
    format!("{head_str}\n...[truncated]...\n{tail_str}")
}

fn take_chars(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) && end < s.len() {
        end += 1;
    }
    s[..end.min(s.len())].to_string()
}

fn take_chars_back(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut start = s.len() - n;
    while !s.is_char_boundary(start) && start > 0 {
        start -= 1;
    }
    s[start..].to_string()
}

fn estimate_messages_tokens(messages: &[Message]) -> u64 {
    let mut total: usize = 0;
    for msg in messages {
        total += content_len(msg) + 10;
        if let Some(tcs) = msg.tool_calls.as_ref() {
            for tc in tcs {
                total += tc.function.name.len() + tc.function.arguments.len();
            }
        }
    }
    chars_to_tokens(total)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

include!("compression/tests.rs");
