//! Context management for conversation history.
//!
//! The `ContextManager` tracks messages, enforces budget constraints on the
//! conversation window, and provides context compression via `ContextCompressor`.
//!
//! Also provides SOUL.md personality loading, context file injection, and
//! full system prompt assembly (corresponding to Python `run_agent.py`'s
//! `_build_system_prompt`).

use std::path::{Path, PathBuf};

use hermes_core::{BudgetConfig, Message, MessageRole};

/// Number of recent messages to preserve during compression.
const DEFAULT_RECENT_MESSAGES: usize = 4;

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
    ///
    /// The strategy is simple truncation: concatenate each message on a
    /// single line in the form `Role: content...` and then truncate the
    /// result to a reasonable maximum length so the summary itself does
    /// not dominate the context budget.
    fn build_summary(messages: &[Message]) -> String {
        const MAX_SUMMARY_CHARS: usize = 2048;

        let mut buf = String::from("[Conversation summary] Earlier conversation:\n");

        for msg in messages {
            let role_label = match msg.role {
                MessageRole::System => "System",
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
                MessageRole::Tool => "Tool",
            };

            let content = msg.content.as_deref().unwrap_or("<no content>");

            // For each message add a single line; truncate long content.
            let remaining = MAX_SUMMARY_CHARS.saturating_sub(buf.len());
            if remaining == 0 {
                break;
            }

            let line = format!("{role_label}: {content}\n");
            if line.len() <= remaining {
                buf.push_str(&line);
            } else {
                // Truncate this line to fit and stop — we've hit the cap.
                buf.push_str(&line[..remaining.saturating_sub(1)]);
                buf.push('\n');
                break;
            }
        }

        buf
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
}

// ---------------------------------------------------------------------------
// SOUL.md personality loading
// ---------------------------------------------------------------------------

/// Default agent identity used when no SOUL.md is found.
const DEFAULT_AGENT_IDENTITY: &str = "You are Hermes, a helpful AI assistant. \
You have access to tools and can help with a wide variety of tasks.";

/// Load the SOUL.md personality file from `~/.hermes/SOUL.md`.
///
/// Returns `None` if the file doesn't exist or can't be read.
pub fn load_soul_md() -> Option<String> {
    let home = dirs::home_dir()?;
    let soul_path = home.join(".hermes").join("SOUL.md");
    load_soul_md_from(&soul_path)
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
    let home = dirs::home_dir()?;
    let personality_path = home
        .join(".hermes")
        .join("personalities")
        .join(format!("{name}.md"));
    load_soul_md_from(&personality_path)
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
}
