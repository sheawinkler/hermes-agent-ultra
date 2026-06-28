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
unless otherwise directed below. Be targeted and efficient in your exploration and investigations. \
When the user asks for an actionable task, execute immediately: run the first concrete step with available tools, \
then continue until completion. Do not stop at intent-only narration such as 'I'll proceed' without performing work.";

const LEGACY_INSTALLER_SOUL_TEMPLATE: &str = "# Hermes Agent Persona\n\n<!--\nCustomize this file to control how Hermes communicates.\nThis file is loaded every message; no restart needed.\nDelete this file (or leave it empty) to use the default personality.\n-->";

const LEGACY_UPSTREAM_SOUL_TEMPLATE_WITH_EXAMPLES: &str = "# Hermes Agent Persona\n\n<!--\nThis file defines the agent's personality and tone.\nThe agent will embody whatever you write here.\nEdit this to customize how Hermes communicates with you.\n\nExamples:\n  - \"You are a warm, playful assistant who uses kaomoji occasionally.\"\n  - \"You are a concise technical expert. No fluff, just facts.\"\n  - \"You speak like a friendly coworker who happens to know everything.\"\n\nThis file is loaded fresh each message -- no restart needed.\nDelete the contents (or this file) to use the default personality.\n-->";

const LEGACY_UPSTREAM_SOUL_TEMPLATE: &str = "# Hermes Agent Persona\n\n<!--\nThis file defines the agent's personality and tone.\nThe agent will embody whatever you write here.\nEdit this to customize how Hermes communicates with you.\n\nThis file is loaded fresh each message -- no restart needed.\nDelete the contents (or this file) to use the default personality.\n-->";

const LEGACY_SOUL_TEMPLATES: &[&str] = &[
    LEGACY_INSTALLER_SOUL_TEMPLATE,
    LEGACY_UPSTREAM_SOUL_TEMPLATE_WITH_EXAMPLES,
    LEGACY_UPSTREAM_SOUL_TEMPLATE,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoulSeedOutcome {
    Created,
    UpgradedLegacy,
    Preserved,
}

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

fn normalize_soul_template(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_start_matches('\u{feff}')
        .trim()
        .to_string()
}

pub fn is_legacy_template_soul(text: &str) -> bool {
    let normalized = normalize_soul_template(text);
    LEGACY_SOUL_TEMPLATES
        .iter()
        .any(|template| normalized == normalize_soul_template(template))
}

pub fn ensure_default_soul_md(hermes_home: &Path) -> std::io::Result<SoulSeedOutcome> {
    std::fs::create_dir_all(hermes_home)?;
    let soul_path = hermes_home.join("SOUL.md");
    match std::fs::read_to_string(&soul_path) {
        Ok(existing) => {
            if is_legacy_template_soul(&existing) {
                std::fs::write(&soul_path, DEFAULT_AGENT_IDENTITY)?;
                Ok(SoulSeedOutcome::UpgradedLegacy)
            } else {
                Ok(SoulSeedOutcome::Preserved)
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&soul_path, DEFAULT_AGENT_IDENTITY)?;
            Ok(SoulSeedOutcome::Created)
        }
        Err(_) => Ok(SoulSeedOutcome::Preserved),
    }
}

/// Load the SOUL.md personality file from the active Hermes home.
///
/// Returns `None` if the file doesn't exist or can't be read.
pub fn load_soul_md() -> Option<String> {
    load_soul_md_from_home(None)
}

pub fn load_soul_md_from_home(hermes_home_override: Option<&str>) -> Option<String> {
    let soul_path = resolve_hermes_home(hermes_home_override).join("SOUL.md");
    load_soul_md_from(&soul_path)
}

fn resolve_hermes_home(hermes_home_override: Option<&str>) -> PathBuf {
    hermes_home_override
        .map(PathBuf::from)
        .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
        .or_else(|| {
            std::env::var("HERMES_AGENT_ULTRA_HOME")
                .ok()
                .map(PathBuf::from)
        })
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
        Ok(content) if !content.trim().is_empty() && !is_legacy_template_soul(&content) => {
            Some(content)
        }
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

include!("context/tests.rs");
