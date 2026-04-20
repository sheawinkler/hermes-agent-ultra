//! Strongly-typed metadata structs for the models.dev registry.
//!
//! Mirrors the dataclasses in `agent/models_dev.py`:
//! [`ModelInfo`] is the rich model record, [`ProviderInfo`] is the
//! provider-level header, and [`ModelCapabilities`] is the simpler
//! defaults-baked-in view used by lightweight callers.

use serde::{Deserialize, Serialize};

/// Full metadata for a single model from models.dev.
///
/// Field-by-field port of `ModelInfo` in `agent/models_dev.py`. All numeric
/// limits use 0 as "unknown" to match Python's int(0) sentinel; `cost_*`
/// fields use 0.0 with `None` reserved for "field not present at all".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub family: String,
    /// models.dev provider ID (e.g. `"anthropic"`, `"google"`).
    pub provider_id: String,

    // Capabilities
    pub reasoning: bool,
    pub tool_call: bool,
    /// Supports image/file attachments (vision).
    pub attachment: bool,
    pub temperature: bool,
    pub structured_output: bool,
    pub open_weights: bool,

    // Modalities (`("text", "image", "pdf", ...)`).
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,

    // Limits (0 = unknown).
    pub context_window: u64,
    pub max_output: u64,
    pub max_input: Option<u64>,

    // Cost per million tokens, USD.
    pub cost_input: f64,
    pub cost_output: f64,
    pub cost_cache_read: Option<f64>,
    pub cost_cache_write: Option<f64>,

    pub knowledge_cutoff: String,
    pub release_date: String,
    /// `"alpha"`, `"beta"`, `"deprecated"`, or `""`.
    pub status: String,
    /// `false`, `true`, or a string field name like `"reasoning_content"`.
    pub interleaved: InterleavedFlag,
}

/// Polymorphic `interleaved` field — Python returns either a bool or a dict
/// like `{"field": "reasoning_content"}`. We collapse the dict variant down
/// to the field name string since that's the only consumer pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InterleavedFlag {
    Bool(bool),
    /// e.g. `{"field": "reasoning_content"}` → `Some("reasoning_content")`.
    Field(String),
}

impl Default for InterleavedFlag {
    fn default() -> Self {
        InterleavedFlag::Bool(false)
    }
}

impl InterleavedFlag {
    pub fn is_enabled(&self) -> bool {
        match self {
            InterleavedFlag::Bool(b) => *b,
            InterleavedFlag::Field(_) => true,
        }
    }
}

impl ModelInfo {
    /// Returns true when at least one cost component is non-zero.
    pub fn has_cost_data(&self) -> bool {
        self.cost_input > 0.0 || self.cost_output > 0.0
    }

    /// Vision support — explicit `attachment` flag OR `image` in input modalities.
    pub fn supports_vision(&self) -> bool {
        self.attachment || self.input_modalities.iter().any(|m| m == "image")
    }

    pub fn supports_pdf(&self) -> bool {
        self.input_modalities.iter().any(|m| m == "pdf")
    }

    pub fn supports_audio_input(&self) -> bool {
        self.input_modalities.iter().any(|m| m == "audio")
    }

    /// Human-readable cost string, e.g. `"$3.00/M in, $15.00/M out"`.
    pub fn format_cost(&self) -> String {
        if !self.has_cost_data() {
            return "unknown".to_string();
        }
        let mut parts = vec![
            format!("${:.2}/M in", self.cost_input),
            format!("${:.2}/M out", self.cost_output),
        ];
        if let Some(cache) = self.cost_cache_read {
            parts.push(format!("cache read ${cache:.2}/M"));
        }
        parts.join(", ")
    }

    /// Human-readable capabilities, e.g. `"reasoning, tools, vision, PDF"`.
    pub fn format_capabilities(&self) -> String {
        let mut caps = Vec::new();
        if self.reasoning {
            caps.push("reasoning");
        }
        if self.tool_call {
            caps.push("tools");
        }
        if self.supports_vision() {
            caps.push("vision");
        }
        if self.supports_pdf() {
            caps.push("PDF");
        }
        if self.supports_audio_input() {
            caps.push("audio");
        }
        if self.structured_output {
            caps.push("structured output");
        }
        if self.open_weights {
            caps.push("open weights");
        }
        if caps.is_empty() {
            "basic".to_string()
        } else {
            caps.join(", ")
        }
    }
}

/// Full metadata for a provider from models.dev.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderInfo {
    /// models.dev provider ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Env var names that hold the API key, in order of preference.
    pub env: Vec<String>,
    /// Base URL for the API.
    pub api: String,
    /// Documentation URL.
    pub doc: String,
    pub model_count: usize,
}

/// Compact capability struct with sensible defaults — matches Python
/// `ModelCapabilities`.
///
/// Defaults (`supports_tools = true`, `context_window = 200_000`, etc.) come
/// from the Python reference and reflect "modern frontier model" assumptions
/// for callers that just want a quick yes/no without a full lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_reasoning: bool,
    pub context_window: u64,
    pub max_output_tokens: u64,
    pub model_family: String,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            supports_tools: true,
            supports_vision: false,
            supports_reasoning: false,
            context_window: 200_000,
            max_output_tokens: 8_192,
            model_family: String::new(),
        }
    }
}
