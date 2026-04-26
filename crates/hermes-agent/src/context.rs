//! Context management for conversation history.
//!
//! The `ContextManager` tracks messages, enforces budget constraints on the
//! conversation window, and provides context compression via `ContextCompressor`.
//!
//! Also provides SOUL.md personality loading, context file injection, and
//! full system prompt assembly (corresponding to Python `run_agent.py`'s
//! `_build_system_prompt`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use hermes_core::{BudgetConfig, Message, MessageRole};

/// Number of recent messages to preserve during compression.
const DEFAULT_RECENT_MESSAGES: usize = 4;
const MEMORY_ENTRY_DELIMITER: &str = "\n§\n";
const MEMORY_CHAR_LIMIT: usize = 2200;
const USER_CHAR_LIMIT: usize = 1375;

// ---------------------------------------------------------------------------
// ContextCompressor
// ---------------------------------------------------------------------------

/// Compresses conversation history by summarizing older messages.
///
/// When the conversation grows too long, the compressor replaces the middle
/// portion of the history with a single system message containing a summary,
/// while preserving the leading system prompt(s) and the most recent N
/// messages intact.
#[derive(Debug, Clone)]
pub struct ContextCompressor {
    /// Number of the most recent messages to keep unchanged.
    pub recent_messages_count: usize,
}

impl ContextCompressor {
    /// Create a new compressor that keeps the last `recent_messages_count`
    /// messages intact (plus any leading system prompt messages).
    pub fn new(recent_messages_count: usize) -> Self {
        Self {
            recent_messages_count,
        }
    }

    /// Create a compressor with the default retention count (4 recent messages).
    pub fn default_compressor() -> Self {
        Self::new(DEFAULT_RECENT_MESSAGES)
    }

    /// Compress a slice of messages into a shorter `Vec<Message>`.
    ///
    /// The resulting vector has the following layout:
    ///
    /// ```text
    /// [system prompt(s)] [summary system message] [last N messages]
    /// ```
    ///
    /// - All leading `System`-role messages are preserved verbatim.
    /// - Messages between the system prompt block and the last
    ///   `recent_messages_count` messages are condensed into a single
    ///   system message whose content is a truncated plain-text summary.
    /// - The last `recent_messages_count` messages are kept as-is.
    ///
    /// If there are not enough messages to warrant compression, the input
    /// is returned unchanged (as a new `Vec`).
    pub fn compress(&self, messages: &[Message]) -> Vec<Message> {
        if messages.is_empty() {
            return messages.to_vec();
        }

        // 1. Identify the leading system messages (preserved verbatim).
        let system_end = messages
            .iter()
            .take_while(|m| m.role == MessageRole::System)
            .count();

        // 2. The "recent" block: the last N messages (also kept as-is).
        //    recent_start must be at least system_end so the middle block is
        //    never overlapping with the system block.
        let recent_start = messages
            .len()
            .saturating_sub(self.recent_messages_count)
            .max(system_end);

        // 3. The "middle" block: everything between system_end and recent_start.
        //    If the middle block is empty or only has one message, there is
        //    nothing worth compressing — return unchanged.
        let middle_slice = &messages[system_end..recent_start];
        if middle_slice.is_empty() {
            return messages.to_vec();
        }

        // 4. Build a compact summary from the middle messages.
        let summary = Self::build_summary(middle_slice);

        // 5. Assemble the compressed message list.
        let mut compressed = Vec::with_capacity(system_end + 1 + self.recent_messages_count);

        // Preserve leading system messages.
        compressed.extend_from_slice(&messages[..system_end]);

        // Insert the summary as a single system message.
        compressed.push(Message::system(summary));

        // Preserve the most recent messages.
        if recent_start < messages.len() {
            compressed.extend_from_slice(&messages[recent_start..]);
        }

        compressed
    }

    /// Build a plain-text summary string from a slice of messages.
    fn build_summary(messages: &[Message]) -> String {
        const MAX_SUMMARY_CHARS: usize = 2048;
        const MAX_ITEMS_PER_SECTION: usize = 5;
        const MAX_ITEM_CHARS: usize = 180;

        let mut goals: Vec<String> = Vec::new();
        let mut assistant_updates: Vec<String> = Vec::new();
        let mut tool_updates: Vec<String> = Vec::new();

        for msg in messages.iter().rev() {
            let content = msg.content.as_deref().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let compact = Self::compact_whitespace(content);
            let concise = Self::truncate_item(&compact, MAX_ITEM_CHARS);
            if concise.is_empty() {
                continue;
            }

            match msg.role {
                MessageRole::User if goals.len() < MAX_ITEMS_PER_SECTION => {
                    if !goals.contains(&concise) {
                        goals.push(concise);
                    }
                }
                MessageRole::Assistant if assistant_updates.len() < MAX_ITEMS_PER_SECTION => {
                    if !assistant_updates.contains(&concise) {
                        assistant_updates.push(concise);
                    }
                }
                MessageRole::Tool if tool_updates.len() < MAX_ITEMS_PER_SECTION => {
                    if !tool_updates.contains(&concise) {
                        tool_updates.push(concise);
                    }
                }
                _ => {}
            }
        }

        goals.reverse();
        assistant_updates.reverse();
        tool_updates.reverse();

        let mut lines: Vec<String> = vec![
            "[Conversation summary] Earlier conversation compressed into key points:".to_string(),
        ];
        if !goals.is_empty() {
            lines.push("User goals and requests:".to_string());
            for item in &goals {
                lines.push(format!("- {item}"));
            }
        }
        if !assistant_updates.is_empty() {
            lines.push("Assistant commitments and guidance:".to_string());
            for item in &assistant_updates {
                lines.push(format!("- {item}"));
            }
        }
        if !tool_updates.is_empty() {
            lines.push("Tool outputs and execution state:".to_string());
            for item in &tool_updates {
                lines.push(format!("- {item}"));
            }
        }

        if goals.is_empty() && assistant_updates.is_empty() && tool_updates.is_empty() {
            lines.push(format!("- {} message(s) were compressed.", messages.len()));
        }

        let mut out = lines.join("\n");
        if out.chars().count() > MAX_SUMMARY_CHARS {
            out = out.chars().take(MAX_SUMMARY_CHARS).collect::<String>() + "...";
        }
        out
    }

    fn compact_whitespace(input: &str) -> String {
        input.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn truncate_item(input: &str, max_chars: usize) -> String {
        if input.is_empty() {
            return String::new();
        }
        let mut clipped = input.to_string();
        if let Some((idx, _)) = input
            .char_indices()
            .find(|(_, c)| matches!(c, '.' | '!' | '?' | '\n'))
        {
            clipped = input[..=idx].to_string();
        }
        if clipped.chars().count() <= max_chars {
            return clipped;
        }
        clipped.chars().take(max_chars).collect::<String>() + "..."
    }
}

impl Default for ContextCompressor {
    fn default() -> Self {
        Self::default_compressor()
    }
}

// ---------------------------------------------------------------------------
// ContextManager
// ---------------------------------------------------------------------------

/// Manages conversation history with budget-aware truncation and compression.
#[derive(Debug, Clone)]
pub struct ContextManager {
    messages: Vec<Message>,
    /// Maximum total characters for the conversation (excluding system messages).
    max_context_chars: usize,
    /// Compressor used when the context exceeds the budget threshold.
    compressor: ContextCompressor,
}

impl ContextManager {
    /// Create a new context manager with a character budget.
    pub fn new(max_context_chars: usize) -> Self {
        Self {
            messages: Vec::new(),
            max_context_chars,
            compressor: ContextCompressor::default(),
        }
    }

    /// Create a context manager with default budget (200k chars).
    pub fn default_budget() -> Self {
        Self::new(200_000)
    }

    /// Create a context manager with a custom compressor.
    pub fn with_compressor(max_context_chars: usize, compressor: ContextCompressor) -> Self {
        Self {
            messages: Vec::new(),
            max_context_chars,
            compressor,
        }
    }

    /// Add a message to the conversation history.
    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Get a reference to the current messages.
    pub fn get_messages(&self) -> &[Message] {
        &self.messages
    }

    /// Get a mutable reference to the messages.
    pub fn get_messages_mut(&mut self) -> &mut Vec<Message> {
        &mut self.messages
    }

    /// Return the number of messages in the history.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Return whether the history is empty.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Truncate the conversation history to fit within the budget.
    ///
    /// Always preserves:
    /// - The system prompt (first message if it has role System)
    /// - The last user message
    /// - The most recent assistant messages
    ///
    /// Older messages are removed from the middle until the total fits.
    pub fn truncate_to_budget(&mut self, budget: &BudgetConfig) {
        let max_chars = budget.max_aggregate_chars.min(self.max_context_chars);
        let total_chars: usize = self
            .messages
            .iter()
            .map(|m| m.content.as_deref().map(|c| c.len()).unwrap_or(0))
            .sum();

        if total_chars <= max_chars {
            return;
        }

        // Identify system messages (to preserve) and the last user message
        let system_end = self
            .messages
            .iter()
            .take_while(|m| m.role == MessageRole::System)
            .count();

        // Keep removing oldest non-system messages until we're under budget
        loop {
            let current_total: usize = self
                .messages
                .iter()
                .map(|m| m.content.as_deref().map(|c| c.len()).unwrap_or(0))
                .sum();

            if current_total <= max_chars {
                break;
            }

            // Find the first removable message (after system messages, not the last one)
            if self.messages.len() <= system_end + 2 {
                // Can't remove any more without losing essential context
                break;
            }

            // Remove the oldest non-system message
            let remove_idx = system_end;
            self.messages.remove(remove_idx);
        }
    }

    /// Compress the conversation history when it exceeds the budget.
    ///
    /// If the total character count of all messages exceeds 80% of
    /// `max_context_chars`, the `ContextCompressor` is invoked to produce a
    /// shorter message list where older messages are replaced by a single
    /// summary system message and the most recent messages are kept intact.
    pub fn compress(&mut self) {
        let threshold = (self.max_context_chars as f64 * 0.8) as usize;
        let total = self.total_chars();

        if total <= threshold {
            tracing::debug!(
                total_chars = total,
                threshold,
                "Context under compression threshold, skipping"
            );
            return;
        }

        tracing::info!(
            total_chars = total,
            threshold,
            "Compressing conversation history (exceeds 80% budget)"
        );

        let compressed = self.compressor.compress(&self.messages);
        self.messages = compressed;
    }

    /// Reset the conversation history, clearing all messages.
    pub fn reset(&mut self) {
        self.messages.clear();
    }

    /// Calculate the total character count of all message content.
    pub fn total_chars(&self) -> usize {
        self.messages
            .iter()
            .map(|m| m.content.as_deref().map(|c| c.len()).unwrap_or(0))
            .sum()
    }

    /// Configured maximum context character budget (same units as [`Self::total_chars`]).
    pub fn max_context_chars(&self) -> usize {
        self.max_context_chars
    }
}

// ---------------------------------------------------------------------------
// SOUL.md personality loading
// ---------------------------------------------------------------------------

/// Default agent identity used when no SOUL.md is found.
pub const DEFAULT_AGENT_IDENTITY: &str = "You are Hermes Agent, an intelligent AI assistant created by Nous Research. \
You are helpful, knowledgeable, and direct. You assist users with a wide range of tasks including answering questions, \
writing and editing code, analyzing information, creative work, and executing actions via your tools. \
You communicate clearly, admit uncertainty when appropriate, and prioritize being genuinely useful over being verbose \
unless otherwise directed below. Be targeted and efficient in your exploration and investigations.";

const BUILTIN_PERSONALITY_CODER: &str = "You are operating in the `coder` persona.\n\
Prioritize correctness, explicit assumptions, and deterministic execution steps.\n\
When editing code, prefer small verifiable changes and explain trade-offs briefly.\n\
Always include concrete verification steps (tests, build, or runtime checks).";

const BUILTIN_PERSONALITY_WRITER: &str = "You are operating in the `writer` persona.\n\
Prioritize clarity, structure, and audience-aware phrasing.\n\
Use concise sections, strong topic sentences, and remove unnecessary repetition.\n\
When asked to revise, preserve meaning while improving readability and flow.";

const BUILTIN_PERSONALITY_ANALYST: &str = "You are operating in the `analyst` persona.\n\
Prioritize evidence, explicit reasoning, and clear uncertainty bounds.\n\
Break complex problems into assumptions, observations, and conclusions.\n\
When data is missing, state what is unknown and propose the next highest-value check.";

const BUILTIN_PERSONALITY_CONCISE: &str = "You are operating in the `concise` persona.\n\
Prioritize brevity and directness while preserving technical correctness.\n\
Prefer short, action-oriented responses with minimal filler.\n\
When detail is needed, keep structure tight and focused on execution.";

const BUILTIN_PERSONALITY_CREATIVE: &str = "You are operating in the `creative` persona.\n\
Favor original framing, vivid but clear language, and idea generation.\n\
Offer multiple strong options when brainstorming.\n\
Balance creativity with practical constraints and actionable next steps.";

const BUILTIN_PERSONALITY_TECHNICAL: &str = "You are operating in the `technical` persona.\n\
Prioritize precision, systems-level thinking, and implementation detail.\n\
Expose assumptions, interfaces, and edge cases explicitly.\n\
When proposing changes, include concrete verification paths.";

const BUILTIN_PERSONALITY_COMPANION: &str = "You are operating in the `companion` persona.\n\
Prioritize warmth, active listening, and non-judgmental support.\n\
When users share personal concerns, briefly reflect what you heard, ask one clarifying question when needed, and offer practical next-step options.\n\
Stay calm and concise; do not posture as a therapist or crisis professional.";

const BUILTIN_PERSONALITY_DECISION_COACH: &str = "You are operating in the `decision-coach` persona.\n\
Prioritize structured decision support and clear trade-offs.\n\
For difficult choices, gather constraints first, compare options side-by-side, and surface the highest-leverage next action.\n\
Avoid prescribing a single answer unless the user explicitly asks for a recommendation.";

const BUILTIN_PERSONALITY_REFLECTIVE: &str = "You are operating in the `reflective` persona.\n\
Prioritize thoughtful dialogue, concise synthesis, and grounded follow-up questions.\n\
Use one focused question at a time to help the user clarify goals, assumptions, or emotions before advising.\n\
Balance empathy with factual precision and actionable guidance.";

const BUILTIN_PERSONALITY_SECURITY_AUDITOR: &str =
    "You are operating in the `security-auditor` persona.\n\
Prioritize threat modeling, abuse-path detection, and least-privilege recommendations.\n\
For every meaningful change, identify probable attack surfaces, data exposure risk, and concrete mitigations.\n\
Separate confirmed vulnerabilities from hypotheses and provide verification steps for each claim.";

const BUILTIN_PERSONALITY_RELEASE_MANAGER: &str =
    "You are operating in the `release-manager` persona.\n\
Prioritize production readiness, rollback safety, and explicit release gates.\n\
Frame decisions with launch criteria, test coverage confidence, operational risk, and contingency plan.\n\
When uncertainty remains, recommend the smallest safe release unit and next validation step.";

const BUILTIN_PERSONALITY_OPS_SRE: &str = "You are operating in the `ops-sre` persona.\n\
Prioritize reliability, observability, and measurable service behavior.\n\
Start incident/problem analysis from signals (logs, metrics, traces), then isolate likely failure domains.\n\
Recommend changes that reduce blast radius, improve recovery time, and keep runbooks actionable.";

const BUILTIN_PERSONALITY_MCP_INTEGRATOR: &str =
    "You are operating in the `mcp-integrator` persona.\n\
Prioritize connector compatibility, capability mapping, and deterministic tool interfaces.\n\
When integrating providers or plugins, reason explicitly about protocol contracts, auth flow, and failure handling.\n\
Favor minimal, testable integration steps with clear schema/version boundaries.";

const BUILTIN_PERSONALITY_QUANT_RESEARCHER: &str =
    "You are operating in the `quant-researcher` persona.\n\
Prioritize hypothesis-driven analysis, risk-adjusted thinking, and falsifiable experiment design.\n\
For strategy questions, define assumptions, market regime sensitivity, and evaluation metrics before conclusions.\n\
Never present profitability as guaranteed; separate observed edge from speculative inference.";

const BUILTIN_PERSONALITY_PERFORMANCE_ENGINEER: &str =
    "You are operating in the `performance-engineer` persona.\n\
Prioritize latency, throughput, and resource-efficiency constraints.\n\
Use profiling-first diagnosis, quantify bottlenecks, and propose benchmarkable optimizations.\n\
Prefer changes that preserve correctness while improving p50/p95/p99 behavior under realistic load.";

const BUILTIN_PERSONALITY_RESEARCH_SCOUT: &str =
    "You are operating in the `research-scout` persona.\n\
Prioritize evidence discovery, source quality, and synthesis clarity.\n\
For novel or uncertain topics, gather multiple primary sources, extract convergent facts, and mark disagreement areas.\n\
Return concise findings with recommended next experiments or validation checks.";

const BUILTIN_PERSONALITY_NAMES: &[&str] = &[
    "coder",
    "writer",
    "analyst",
    "concise",
    "creative",
    "technical",
    "companion",
    "decision-coach",
    "reflective",
    "security-auditor",
    "release-manager",
    "ops-sre",
    "mcp-integrator",
    "quant-researcher",
    "performance-engineer",
    "research-scout",
];

const BUILTIN_PERSONALITY_DESCRIPTIONS: &[(&str, &str)] = &[
    (
        "coder",
        "Use when you want implementation-heavy answers, patches, and concrete debugging steps.",
    ),
    (
        "writer",
        "Use when you want polished narrative writing, drafts, or editorial tone shaping.",
    ),
    (
        "analyst",
        "Use when you need structured reasoning, trade-off analysis, and decision framing.",
    ),
    (
        "concise",
        "Use when you want short, direct responses with minimal extra explanation.",
    ),
    (
        "creative",
        "Use when you want idea generation, exploration, and novel framing options.",
    ),
    (
        "technical",
        "Use when precision matters: architecture, systems behavior, and implementation detail.",
    ),
    (
        "companion",
        "Use when you want supportive, non-judgmental dialogue with practical next steps.",
    ),
    (
        "decision-coach",
        "Use when choosing between options and you want constraints + trade-offs made explicit.",
    ),
    (
        "reflective",
        "Use when clarifying goals or emotions before committing to a recommendation.",
    ),
    (
        "security-auditor",
        "Use when you need threat modeling, abuse-path checks, and concrete security mitigations.",
    ),
    (
        "release-manager",
        "Use when you need launch gates, rollback planning, and production-readiness decisions.",
    ),
    (
        "ops-sre",
        "Use when debugging reliability issues through logs/metrics and improving operational resilience.",
    ),
    (
        "mcp-integrator",
        "Use when wiring connectors/tools and validating protocol contracts, auth, and compatibility.",
    ),
    (
        "quant-researcher",
        "Use when evaluating strategy hypotheses with risk-aware metrics and falsifiable tests.",
    ),
    (
        "performance-engineer",
        "Use when optimizing latency/throughput with profiling-backed, benchmarkable changes.",
    ),
    (
        "research-scout",
        "Use when rapidly synthesizing high-quality sources and planning next validation steps.",
    ),
];

/// Load the SOUL.md personality file from `~/.hermes/SOUL.md`.
///
/// Returns `None` if the file doesn't exist or can't be read.
pub fn load_soul_md() -> Option<String> {
    let home = dirs::home_dir()?;
    let soul_path = home.join(".hermes").join("SOUL.md");
    load_soul_md_from(&soul_path)
}

fn resolve_hermes_home(hermes_home_override: Option<&str>) -> PathBuf {
    hermes_home_override
        .map(PathBuf::from)
        .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
        .unwrap_or_else(|| PathBuf::from(".hermes"))
}

fn builtin_personality(name: &str) -> Option<&'static str> {
    match name {
        "coder" => Some(BUILTIN_PERSONALITY_CODER),
        "writer" => Some(BUILTIN_PERSONALITY_WRITER),
        "analyst" => Some(BUILTIN_PERSONALITY_ANALYST),
        "concise" => Some(BUILTIN_PERSONALITY_CONCISE),
        "creative" => Some(BUILTIN_PERSONALITY_CREATIVE),
        "technical" => Some(BUILTIN_PERSONALITY_TECHNICAL),
        "companion" => Some(BUILTIN_PERSONALITY_COMPANION),
        "decision-coach" | "decision_coach" => Some(BUILTIN_PERSONALITY_DECISION_COACH),
        "reflective" => Some(BUILTIN_PERSONALITY_REFLECTIVE),
        "security-auditor" | "security_auditor" => Some(BUILTIN_PERSONALITY_SECURITY_AUDITOR),
        "release-manager" | "release_manager" => Some(BUILTIN_PERSONALITY_RELEASE_MANAGER),
        "ops-sre" | "ops_sre" => Some(BUILTIN_PERSONALITY_OPS_SRE),
        "mcp-integrator" | "mcp_integrator" => Some(BUILTIN_PERSONALITY_MCP_INTEGRATOR),
        "quant-researcher" | "quant_researcher" => Some(BUILTIN_PERSONALITY_QUANT_RESEARCHER),
        "performance-engineer" | "performance_engineer" => {
            Some(BUILTIN_PERSONALITY_PERFORMANCE_ENGINEER)
        }
        "research-scout" | "research_scout" => Some(BUILTIN_PERSONALITY_RESEARCH_SCOUT),
        _ => None,
    }
}

/// Return the built-in personality names available without user files.
pub fn builtin_personality_names() -> &'static [&'static str] {
    BUILTIN_PERSONALITY_NAMES
}

/// Return one-line usage guidance for each built-in personality.
pub fn builtin_personality_descriptions() -> &'static [(&'static str, &'static str)] {
    BUILTIN_PERSONALITY_DESCRIPTIONS
}

/// Resolve a named personality by checking user files first, then built-ins.
///
/// Resolution order:
/// 1) `<hermes_home>/personalities/<name>.md` (or `$HERMES_HOME`, then `~/.hermes`)
/// 2) built-in personas (see [`builtin_personality_names`])
pub fn resolve_personality(name: &str, hermes_home_override: Option<&str>) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let slug = trimmed.to_ascii_lowercase();
    let personality_path = resolve_hermes_home(hermes_home_override)
        .join("personalities")
        .join(format!("{slug}.md"));
    if let Some(content) = load_soul_md_from(&personality_path) {
        return Some(content);
    }
    builtin_personality(&slug).map(ToString::to_string)
}

/// Load a SOUL.md file from a specific path.
pub fn load_soul_md_from(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) if !content.trim().is_empty() => Some(content),
        _ => None,
    }
}

/// Load a named personality from `~/.hermes/personalities/<name>.md`.
pub fn switch_personality(name: &str) -> Option<String> {
    resolve_personality(name, None)
}

// ---------------------------------------------------------------------------
// Context files
// ---------------------------------------------------------------------------

/// Load context files from `~/.hermes/context/` directory.
///
/// Returns concatenated content of all `.md` and `.txt` files found.
pub fn load_context_files(hermes_home: &Path) -> String {
    let context_dir = hermes_home.join("context");
    if !context_dir.exists() {
        return String::new();
    }

    let mut parts = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&context_dir) {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| matches!(e, "md" | "txt"))
                        .unwrap_or(false)
            })
            .collect();

        files.sort();

        for file in files {
            if let Ok(content) = std::fs::read_to_string(&file) {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
            }
        }
    }

    parts.join("\n\n")
}

// ---------------------------------------------------------------------------
// Built-in memory snapshot (MEMORY.md / USER.md)
// ---------------------------------------------------------------------------

fn parse_memory_entries(raw: &str) -> Vec<String> {
    raw.split(MEMORY_ENTRY_DELIMITER)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn dedup_entries(entries: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in entries {
        if seen.insert(entry.clone()) {
            out.push(entry);
        }
    }
    out
}

fn render_memory_block(target: &str, entries: &[String]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let (label, limit) = if target == "user" {
        ("USER PROFILE (who the user is)", USER_CHAR_LIMIT)
    } else {
        ("MEMORY (your personal notes)", MEMORY_CHAR_LIMIT)
    };
    let content = entries.join(MEMORY_ENTRY_DELIMITER);
    let current = content.chars().count();
    let pct = if limit > 0 {
        ((current * 100) / limit).min(100)
    } else {
        0
    };
    Some(format!(
        "{label} [{pct}% - {current}/{limit} chars]\n{content}"
    ))
}

/// Load the built-in memory snapshot used in the system prompt.
///
/// This mirrors Python's MemoryStore snapshot semantics at session start:
/// the returned blocks are read once and treated as a frozen prompt view.
pub fn load_builtin_memory_snapshot(
    hermes_home_override: Option<&str>,
) -> (Option<String>, Option<String>) {
    let base = hermes_home_override
        .map(PathBuf::from)
        .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
        .unwrap_or_else(|| PathBuf::from(".hermes"));
    let memory_dir = base.join("memories");
    let memory_path = memory_dir.join("MEMORY.md");
    let user_path = memory_dir.join("USER.md");

    let memory_entries = std::fs::read_to_string(&memory_path)
        .ok()
        .map(|s| dedup_entries(parse_memory_entries(&s)))
        .unwrap_or_default();
    let user_entries = std::fs::read_to_string(&user_path)
        .ok()
        .map(|s| dedup_entries(parse_memory_entries(&s)))
        .unwrap_or_default();

    (
        render_memory_block("memory", &memory_entries),
        render_memory_block("user", &user_entries),
    )
}

// ---------------------------------------------------------------------------
// SystemPromptBuilder
// ---------------------------------------------------------------------------

/// Builder for assembling the full system prompt from multiple layers.
///
/// Layers (in order):
/// 1. SOUL.md personality (or default identity)
/// 2. User/gateway system prompt (if provided)
/// 3. Memory context block (from MemoryManager)
/// 4. Tool descriptions / guidance
/// 5. Skill prompts (if preloaded)
/// 6. Context files from `.hermes/context/`
/// 7. Current date/time
/// 8. Working directory info
pub struct SystemPromptBuilder {
    parts: Vec<String>,
    /// Cached assembled prompt.
    cached: Option<String>,
}

impl SystemPromptBuilder {
    /// Create a new empty builder.
    pub fn new() -> Self {
        Self {
            parts: Vec::new(),
            cached: None,
        }
    }

    /// Add the SOUL.md personality or default identity.
    pub fn with_personality(mut self, soul_content: Option<&str>) -> Self {
        self.parts
            .push(soul_content.unwrap_or(DEFAULT_AGENT_IDENTITY).to_string());
        self.cached = None;
        self
    }

    /// Add a user/gateway system prompt.
    pub fn with_system_message(mut self, message: &str) -> Self {
        if !message.trim().is_empty() {
            self.parts.push(message.to_string());
            self.cached = None;
        }
        self
    }

    /// Add memory context block.
    pub fn with_memory_context(mut self, memory_block: &str) -> Self {
        if !memory_block.trim().is_empty() {
            self.parts.push(memory_block.to_string());
            self.cached = None;
        }
        self
    }

    /// Add tool guidance text.
    pub fn with_tool_guidance(mut self, guidance: &str) -> Self {
        if !guidance.trim().is_empty() {
            self.parts.push(guidance.to_string());
            self.cached = None;
        }
        self
    }

    /// Add preloaded skill prompts.
    pub fn with_skills_prompt(mut self, skills_prompt: &str) -> Self {
        if !skills_prompt.trim().is_empty() {
            self.parts.push(skills_prompt.to_string());
            self.cached = None;
        }
        self
    }

    /// Add context files content.
    pub fn with_context_files(mut self, context_content: &str) -> Self {
        if !context_content.trim().is_empty() {
            self.parts.push(context_content.to_string());
            self.cached = None;
        }
        self
    }

    /// Add current date/time and optional metadata.
    pub fn with_timestamp(mut self, model: Option<&str>, provider: Option<&str>) -> Self {
        let now = chrono::Local::now();
        let mut line = format!(
            "Conversation started: {}",
            now.format("%A, %B %d, %Y %I:%M %p")
        );
        if let Some(m) = model {
            line.push_str(&format!("\nModel: {m}"));
        }
        if let Some(p) = provider {
            line.push_str(&format!("\nProvider: {p}"));
        }
        self.parts.push(line);
        self.cached = None;
        self
    }

    /// Add an arbitrary text block.
    pub fn with_block(mut self, block: &str) -> Self {
        if !block.trim().is_empty() {
            self.parts.push(block.to_string());
            self.cached = None;
        }
        self
    }

    /// Build and cache the assembled system prompt.
    pub fn build(&mut self) -> &str {
        if self.cached.is_none() {
            self.cached = Some(self.parts.join("\n\n"));
        }
        self.cached.as_deref().unwrap_or("")
    }

    /// Invalidate the cached prompt (call after personality/config changes).
    pub fn invalidate(&mut self) {
        self.cached = None;
    }

    /// Get the cached prompt without rebuilding.
    pub fn cached(&self) -> Option<&str> {
        self.cached.as_deref()
    }
}

impl Default for SystemPromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_get_messages() {
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::system("You are helpful"));
        cm.add_message(Message::user("Hello"));

        assert_eq!(cm.len(), 2);
        assert_eq!(cm.get_messages()[0].role, MessageRole::System);
        assert_eq!(cm.get_messages()[1].role, MessageRole::User);
    }

    #[test]
    fn test_reset() {
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::user("Hello"));
        assert!(!cm.is_empty());

        cm.reset();
        assert!(cm.is_empty());
    }

    #[test]
    fn test_truncate_preserves_system_messages() {
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::system("System prompt"));

        for i in 0..50 {
            cm.add_message(Message::assistant(format!("Response {i}")));
        }
        cm.add_message(Message::user("Final question"));

        let budget = BudgetConfig {
            max_result_size_chars: 100_000,
            max_aggregate_chars: 200,
        };
        cm.truncate_to_budget(&budget);

        // System prompt should still be first
        assert_eq!(cm.get_messages()[0].role, MessageRole::System);
        // Last message should be the user question
        assert_eq!(cm.get_messages().last().unwrap().role, MessageRole::User);
    }

    // ---- ContextCompressor tests ----

    #[test]
    fn test_compressor_no_op_when_small() {
        let compressor = ContextCompressor::new(4);
        let msgs = vec![
            Message::system("You are helpful"),
            Message::user("Hi"),
            Message::assistant("Hello!"),
        ];
        let result = compressor.compress(&msgs);
        // Only 3 messages total, recent_count=4 covers all, middle is empty => no-op.
        assert_eq!(result.len(), msgs.len());
    }

    #[test]
    fn test_compressor_replaces_middle_with_summary() {
        let compressor = ContextCompressor::new(2);
        let msgs = vec![
            Message::system("System prompt"),
            Message::user("Question 1"),
            Message::assistant("Answer 1"),
            Message::user("Question 2"),
            Message::assistant("Answer 2"),
            Message::user("Question 3"),
            Message::assistant("Answer 3"),
        ];

        let result = compressor.compress(&msgs);

        // Expected layout: [system prompt] [summary] [last 2 messages]
        assert!(result.len() <= msgs.len());
        // First message is still the system prompt.
        assert_eq!(result[0].role, MessageRole::System);
        assert_eq!(result[0].content.as_deref(), Some("System prompt"));
        // Second message is the summary (also System role).
        assert_eq!(result[1].role, MessageRole::System);
        assert!(result[1]
            .content
            .as_deref()
            .unwrap_or("")
            .contains("[Conversation summary]"));
        // Last 2 messages preserved.
        assert_eq!(result[result.len() - 2].role, MessageRole::User);
        assert_eq!(result[result.len() - 1].role, MessageRole::Assistant);
    }

    #[test]
    fn test_context_manager_compress_under_threshold() {
        // With 10k budget, 80% = 8k. Short messages won't trigger compression.
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::system("System prompt"));
        cm.add_message(Message::user("Hi"));
        cm.add_message(Message::assistant("Hello!"));

        let len_before = cm.len();
        cm.compress();
        assert_eq!(cm.len(), len_before);
    }

    #[test]
    fn test_context_manager_compress_over_threshold() {
        // Budget large enough for the summary + recent messages, but small
        // enough that the 80% threshold (240 chars) is exceeded by the
        // full conversation.
        let mut cm = ContextManager::with_compressor(
            300, // 300 chars budget => 80% threshold = 240 chars
            ContextCompressor::new(2),
        );
        cm.add_message(Message::system("System prompt"));
        // Add enough messages to exceed 240 chars.
        for i in 0..10 {
            cm.add_message(Message::user(format!(
                "This is question number {i} with enough text to be long"
            )));
            cm.add_message(Message::assistant(format!(
                "This is answer number {i} also fairly long to fill up the budget"
            )));
        }
        cm.add_message(Message::user("Short q"));
        cm.add_message(Message::assistant("Short a"));

        let len_before = cm.len();
        assert!(
            cm.total_chars() > 240,
            "should be over threshold before compress"
        );
        cm.compress();
        // After compression the message count should be smaller.
        assert!(
            cm.len() < len_before,
            "compression should reduce message count"
        );
        // System prompt preserved.
        assert_eq!(
            cm.get_messages()[0].content.as_deref(),
            Some("System prompt")
        );
        // A summary message should appear after the system prompt.
        assert!(cm.get_messages().len() >= 2);
        assert_eq!(cm.get_messages()[1].role, MessageRole::System);
        assert!(cm.get_messages()[1]
            .content
            .as_deref()
            .unwrap_or("")
            .contains("[Conversation summary]"));
    }

    #[test]
    fn test_build_summary_truncates() {
        let msgs: Vec<Message> = (0..100)
            .map(|i| Message::user(format!("Message number {i} with some extra content")))
            .collect();
        let summary = ContextCompressor::build_summary(&msgs);
        // Summary must be capped at ~2048 chars.
        assert!(summary.len() <= 2100, "summary should be roughly capped");
        assert!(summary.contains("[Conversation summary]"));
    }

    // ---- SOUL.md and SystemPromptBuilder tests ----

    #[test]
    fn test_load_soul_md_from_nonexistent() {
        let result = load_soul_md_from(Path::new("/tmp/nonexistent/SOUL.md"));
        assert!(result.is_none());
    }

    #[test]
    fn test_load_soul_md_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        let soul_path = tmp.path().join("SOUL.md");
        std::fs::write(&soul_path, "You are a pirate assistant.").unwrap();

        let result = load_soul_md_from(&soul_path);
        assert_eq!(result.as_deref(), Some("You are a pirate assistant."));
    }

    #[test]
    fn test_load_soul_md_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let soul_path = tmp.path().join("SOUL.md");
        std::fs::write(&soul_path, "   ").unwrap();

        let result = load_soul_md_from(&soul_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_personality_builtin() {
        let coder = resolve_personality("coder", None).unwrap_or_default();
        assert!(coder.contains("`coder` persona"));
    }

    #[test]
    fn test_resolve_personality_prefers_user_file() {
        let tmp = tempfile::tempdir().unwrap();
        let personalities_dir = tmp.path().join("personalities");
        std::fs::create_dir_all(&personalities_dir).unwrap();
        std::fs::write(
            personalities_dir.join("coder.md"),
            "Custom coder persona from user profile.",
        )
        .unwrap();

        let resolved = resolve_personality("coder", Some(tmp.path().to_string_lossy().as_ref()))
            .unwrap_or_default();
        assert_eq!(resolved, "Custom coder persona from user profile.");
    }

    #[test]
    fn test_load_context_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx_dir = tmp.path().join("context");
        std::fs::create_dir_all(&ctx_dir).unwrap();
        std::fs::write(ctx_dir.join("01-rules.md"), "Rule 1: Be helpful").unwrap();
        std::fs::write(ctx_dir.join("02-style.txt"), "Use formal tone").unwrap();
        std::fs::write(ctx_dir.join("ignored.json"), "{}").unwrap();

        let content = load_context_files(tmp.path());
        assert!(content.contains("Rule 1: Be helpful"));
        assert!(content.contains("Use formal tone"));
        assert!(!content.contains("{}"));
    }

    #[test]
    fn test_load_context_files_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let content = load_context_files(tmp.path());
        assert!(content.is_empty());
    }

    #[test]
    fn test_system_prompt_builder() {
        let mut builder = SystemPromptBuilder::new()
            .with_personality(Some("You are Hermes."))
            .with_system_message("Be concise.")
            .with_memory_context("<memory-context>User likes Rust</memory-context>")
            .with_timestamp(Some("gpt-4o"), None);

        let prompt = builder.build();
        assert!(prompt.contains("You are Hermes."));
        assert!(prompt.contains("Be concise."));
        assert!(prompt.contains("User likes Rust"));
        assert!(prompt.contains("gpt-4o"));
    }

    #[test]
    fn test_system_prompt_builder_default_identity() {
        let mut builder = SystemPromptBuilder::new().with_personality(None);

        let prompt = builder.build();
        assert!(prompt.contains("Hermes"));
    }

    #[test]
    fn test_system_prompt_builder_caching() {
        let mut builder = SystemPromptBuilder::new().with_personality(Some("Test"));

        // First build
        let p1 = builder.build().to_string();
        // Second build should return cached
        let p2 = builder.build().to_string();
        assert_eq!(p1, p2);

        // Invalidate
        builder.invalidate();
        assert!(builder.cached().is_none());

        // Rebuild
        let p3 = builder.build().to_string();
        assert_eq!(p1, p3);
    }

    #[test]
    fn test_system_prompt_builder_skips_empty() {
        let mut builder = SystemPromptBuilder::new()
            .with_personality(Some("Identity"))
            .with_system_message("")
            .with_memory_context("   ")
            .with_tool_guidance("")
            .with_skills_prompt("");

        let prompt = builder.build();
        assert_eq!(prompt, "Identity");
    }

    #[test]
    fn test_load_builtin_memory_snapshot_reads_memories_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let memories = tmp.path().join("memories");
        std::fs::create_dir_all(&memories).unwrap();
        std::fs::write(
            memories.join("MEMORY.md"),
            "Use ripgrep for search\n§\nUse ripgrep for search\n§\nWorkspace is Rust",
        )
        .unwrap();
        std::fs::write(
            memories.join("USER.md"),
            "Name: Alice\n§\nPrefers concise Chinese",
        )
        .unwrap();

        let (memory_block, user_block) =
            load_builtin_memory_snapshot(Some(tmp.path().to_string_lossy().as_ref()));

        let memory_block = memory_block.unwrap_or_default();
        let user_block = user_block.unwrap_or_default();
        assert!(memory_block.contains("MEMORY (your personal notes)"));
        assert!(memory_block.contains("Use ripgrep for search"));
        assert!(memory_block.contains("Workspace is Rust"));
        assert_eq!(memory_block.matches("Use ripgrep for search").count(), 1);
        assert!(user_block.contains("USER PROFILE (who the user is)"));
        assert!(user_block.contains("Name: Alice"));
    }

    #[test]
    fn test_persona_snapshot_coder() {
        let p = resolve_personality("coder", None).unwrap();
        assert!(p.contains("`coder` persona"));
        assert!(p.contains("correctness"));
    }

    #[test]
    fn test_persona_snapshot_writer() {
        let p = resolve_personality("writer", None).unwrap();
        assert!(p.contains("`writer` persona"));
        assert!(p.contains("clarity"));
    }

    #[test]
    fn test_persona_snapshot_analyst() {
        let p = resolve_personality("analyst", None).unwrap();
        assert!(p.contains("`analyst` persona"));
        assert!(p.contains("evidence"));
    }

    #[test]
    fn test_persona_snapshot_concise() {
        let p = resolve_personality("concise", None).unwrap();
        assert!(p.contains("`concise` persona"));
        assert!(p.contains("brevity"));
    }

    #[test]
    fn test_persona_snapshot_creative() {
        let p = resolve_personality("creative", None).unwrap();
        assert!(p.contains("`creative` persona"));
        assert!(p.contains("idea generation"));
    }

    #[test]
    fn test_persona_snapshot_technical() {
        let p = resolve_personality("technical", None).unwrap();
        assert!(p.contains("`technical` persona"));
        assert!(p.contains("systems-level"));
    }

    #[test]
    fn test_persona_snapshot_companion() {
        let p = resolve_personality("companion", None).unwrap();
        assert!(p.contains("`companion` persona"));
        assert!(p.contains("active listening"));
    }

    #[test]
    fn test_persona_snapshot_decision_coach() {
        let p = resolve_personality("decision-coach", None).unwrap();
        assert!(p.contains("`decision-coach` persona"));
        assert!(p.contains("trade-offs"));
    }

    #[test]
    fn test_persona_snapshot_reflective() {
        let p = resolve_personality("reflective", None).unwrap();
        assert!(p.contains("`reflective` persona"));
        assert!(p.contains("follow-up questions"));
    }

    #[test]
    fn test_persona_snapshot_security_auditor() {
        let p = resolve_personality("security-auditor", None).unwrap();
        assert!(p.contains("`security-auditor` persona"));
        assert!(p.contains("threat modeling"));
    }

    #[test]
    fn test_persona_snapshot_release_manager() {
        let p = resolve_personality("release-manager", None).unwrap();
        assert!(p.contains("`release-manager` persona"));
        assert!(p.contains("rollback"));
    }

    #[test]
    fn test_persona_snapshot_ops_sre() {
        let p = resolve_personality("ops-sre", None).unwrap();
        assert!(p.contains("`ops-sre` persona"));
        assert!(p.contains("observability"));
    }

    #[test]
    fn test_persona_snapshot_mcp_integrator() {
        let p = resolve_personality("mcp-integrator", None).unwrap();
        assert!(p.contains("`mcp-integrator` persona"));
        assert!(p.contains("protocol contracts"));
    }

    #[test]
    fn test_persona_snapshot_quant_researcher() {
        let p = resolve_personality("quant-researcher", None).unwrap();
        assert!(p.contains("`quant-researcher` persona"));
        assert!(p.contains("risk-adjusted"));
    }

    #[test]
    fn test_persona_snapshot_performance_engineer() {
        let p = resolve_personality("performance-engineer", None).unwrap();
        assert!(p.contains("`performance-engineer` persona"));
        assert!(p.contains("profiling-first"));
    }

    #[test]
    fn test_persona_snapshot_research_scout() {
        let p = resolve_personality("research-scout", None).unwrap();
        assert!(p.contains("`research-scout` persona"));
        assert!(p.contains("source quality"));
    }

    #[test]
    fn test_builtin_personality_names_contains_new_modes() {
        let names = builtin_personality_names();
        assert!(names.contains(&"companion"));
        assert!(names.contains(&"decision-coach"));
        assert!(names.contains(&"reflective"));
        assert!(names.contains(&"security-auditor"));
        assert!(names.contains(&"release-manager"));
        assert!(names.contains(&"ops-sre"));
        assert!(names.contains(&"mcp-integrator"));
        assert!(names.contains(&"quant-researcher"));
        assert!(names.contains(&"performance-engineer"));
        assert!(names.contains(&"research-scout"));
    }

    #[test]
    fn test_builtin_personality_descriptions_cover_all_names() {
        let names = builtin_personality_names();
        let descriptions = builtin_personality_descriptions();
        assert_eq!(descriptions.len(), names.len());
        for (name, desc) in descriptions {
            assert!(names.contains(name));
            assert!(!desc.trim().is_empty());
        }
    }

    #[test]
    fn test_persona_unknown_returns_none() {
        assert!(resolve_personality("pirate", None).is_none());
    }

    #[test]
    fn test_persona_system_prompt_deltas() {
        let base = {
            let mut b = SystemPromptBuilder::new().with_personality(None);
            b.build().to_string()
        };

        let coder_prompt = {
            let p = resolve_personality("coder", None).unwrap();
            let mut b = SystemPromptBuilder::new().with_personality(Some(&p));
            b.build().to_string()
        };

        assert!(!base.contains("`coder` persona"));
        assert!(coder_prompt.contains("`coder` persona"));
        assert!(coder_prompt.contains("correctness"));
    }
}
