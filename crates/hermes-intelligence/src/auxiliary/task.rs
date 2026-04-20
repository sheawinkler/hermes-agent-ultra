//! Catalogue of auxiliary tasks.
//!
//! Each variant identifies a side task that the main agent loop offloads to a
//! cheaper / faster LLM. The enum drives:
//!
//! * env-var override naming (`AUXILIARY_{TASK}_PROVIDER` etc.)
//! * config file lookup keys (`auxiliary.{task}.*`)
//! * defaults for `temperature`, `max_tokens`, and timeout
//! * whether the call requires vision capability
//!
//! New tasks can be added without touching the chain logic — the dispatcher
//! treats `Custom("...")` like any other task name.

use std::time::Duration;

/// Side tasks that the main agent loop routes through the auxiliary client.
///
/// Names are deliberately short / lowercase so they double as env var
/// suffixes (`AUXILIARY_VISION_PROVIDER`, `AUXILIARY_COMPRESSION_MODEL`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AuxiliaryTask {
    /// Conversation context compression / summarization.
    Compression,
    /// Vision / multimodal analysis. Requires a vision-capable provider.
    Vision,
    /// Web page extraction (clean readable text from a fetched URL).
    WebExtract,
    /// Session history search (semantic over past turns).
    SessionSearch,
    /// Skills hub: matching a query against installed skill descriptions.
    SkillsHub,
    /// Generic MCP-tooling helper.
    Mcp,
    /// Long-term memory flush (consolidate scratchpad → durable store).
    FlushMemories,
    /// Conversation title generation.
    Title,
    /// Free-form classification (intent, language, ...).
    Classify,
    /// User-defined task name. Treated as a custom string for config / env lookup.
    Custom(String),
}

impl AuxiliaryTask {
    /// Stable lowercase identifier for env vars, config keys, and logging.
    pub fn as_key(&self) -> &str {
        match self {
            AuxiliaryTask::Compression => "compression",
            AuxiliaryTask::Vision => "vision",
            AuxiliaryTask::WebExtract => "web_extract",
            AuxiliaryTask::SessionSearch => "session_search",
            AuxiliaryTask::SkillsHub => "skills_hub",
            AuxiliaryTask::Mcp => "mcp",
            AuxiliaryTask::FlushMemories => "flush_memories",
            AuxiliaryTask::Title => "title",
            AuxiliaryTask::Classify => "classify",
            AuxiliaryTask::Custom(name) => name.as_str(),
        }
    }

    /// `true` when the task always needs vision-capable providers in the chain.
    pub fn requires_vision(&self) -> bool {
        matches!(self, AuxiliaryTask::Vision)
    }

    /// Sensible default temperature for the task. `None` means "let the
    /// provider pick" which we use for vision / classify (deterministic).
    pub fn default_temperature(&self) -> Option<f64> {
        match self {
            AuxiliaryTask::Vision | AuxiliaryTask::Classify => Some(0.0),
            AuxiliaryTask::Title => Some(0.3),
            AuxiliaryTask::Compression
            | AuxiliaryTask::SessionSearch
            | AuxiliaryTask::WebExtract
            | AuxiliaryTask::FlushMemories => Some(0.2),
            AuxiliaryTask::SkillsHub | AuxiliaryTask::Mcp => Some(0.1),
            AuxiliaryTask::Custom(_) => None,
        }
    }

    /// Default max_tokens cap. `None` means "let the provider pick".
    pub fn default_max_tokens(&self) -> Option<u32> {
        match self {
            AuxiliaryTask::Title | AuxiliaryTask::Classify => Some(64),
            AuxiliaryTask::WebExtract => Some(2048),
            AuxiliaryTask::Compression | AuxiliaryTask::FlushMemories => Some(4096),
            AuxiliaryTask::SkillsHub | AuxiliaryTask::Mcp | AuxiliaryTask::SessionSearch => {
                Some(512)
            }
            AuxiliaryTask::Vision => Some(1024),
            AuxiliaryTask::Custom(_) => None,
        }
    }

    /// Default per-task timeout (overridable via `AUXILIARY_{TASK}_TIMEOUT`).
    pub fn default_timeout(&self) -> Duration {
        match self {
            // Vision and compression tend to run longer.
            AuxiliaryTask::Vision => Duration::from_secs(60),
            AuxiliaryTask::Compression | AuxiliaryTask::FlushMemories => Duration::from_secs(45),
            _ => Duration::from_secs(30),
        }
    }

    /// Construct a task from a free-form string. Returns `Custom` for any
    /// unknown identifier.
    pub fn from_str(name: &str) -> Self {
        match name.trim().to_lowercase().as_str() {
            "compression" => AuxiliaryTask::Compression,
            "vision" => AuxiliaryTask::Vision,
            "web_extract" | "webextract" | "web-extract" => AuxiliaryTask::WebExtract,
            "session_search" | "search" => AuxiliaryTask::SessionSearch,
            "skills_hub" | "skills" => AuxiliaryTask::SkillsHub,
            "mcp" => AuxiliaryTask::Mcp,
            "flush_memories" | "memory_flush" => AuxiliaryTask::FlushMemories,
            "title" => AuxiliaryTask::Title,
            "classify" | "classification" => AuxiliaryTask::Classify,
            other => AuxiliaryTask::Custom(other.to_string()),
        }
    }

    /// Env var suffix derived from [`AuxiliaryTask::as_key`], uppercased.
    /// Example: [`AuxiliaryTask::WebExtract`] → `WEB_EXTRACT`.
    pub fn env_suffix(&self) -> String {
        self.as_key().to_uppercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_known_tasks() {
        for t in [
            AuxiliaryTask::Compression,
            AuxiliaryTask::Vision,
            AuxiliaryTask::WebExtract,
            AuxiliaryTask::SessionSearch,
            AuxiliaryTask::SkillsHub,
            AuxiliaryTask::Mcp,
            AuxiliaryTask::FlushMemories,
            AuxiliaryTask::Title,
            AuxiliaryTask::Classify,
        ] {
            assert_eq!(AuxiliaryTask::from_str(t.as_key()), t);
        }
    }

    #[test]
    fn vision_requires_vision_capability() {
        assert!(AuxiliaryTask::Vision.requires_vision());
        assert!(!AuxiliaryTask::Compression.requires_vision());
    }

    #[test]
    fn unknown_string_becomes_custom() {
        assert_eq!(
            AuxiliaryTask::from_str("foobar"),
            AuxiliaryTask::Custom("foobar".to_string())
        );
    }

    #[test]
    fn env_suffix_is_uppercase() {
        assert_eq!(AuxiliaryTask::WebExtract.env_suffix(), "WEB_EXTRACT");
        assert_eq!(AuxiliaryTask::Vision.env_suffix(), "VISION");
    }
}
