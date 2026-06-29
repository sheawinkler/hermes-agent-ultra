// ---------------------------------------------------------------------------
// AgentLoopBehaviorConfig (Python-shaped nudge + background review)
// ---------------------------------------------------------------------------

/// Mirrors Python defaults: `memory.nudge_interval` / `skills.creation_nudge_interval`,
/// and implicit background memory/skill review when those intervals fire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentLoopBehaviorConfig {
    #[serde(default = "default_agent_memory_nudge_interval")]
    pub memory_nudge_interval: u32,
    #[serde(default = "default_agent_skill_nudge_interval")]
    pub skill_creation_nudge_interval: u32,
    /// Skip auto-injected workspace/personal context files (SOUL.md, AGENTS.md, etc.).
    /// Useful for batch-style runs where personalized instructions would pollute trajectories.
    #[serde(default = "default_agent_skip_context_files")]
    pub skip_context_files: bool,
    /// Coding posture activation: auto/focus/on/off.
    ///
    /// `auto` injects prompt-only coding guidance on interactive coding surfaces
    /// in a code workspace. `focus` also collapses tools to the lean coding
    /// toolset. `on` forces prompt-only posture; `off` disables it.
    #[serde(default = "default_agent_coding_context")]
    pub coding_context: String,
    /// Toolsets to subtract after the platform/default toolset selection.
    ///
    /// Platform bundle names (`hermes-*`) are interpreted by the Rust planner as
    /// non-core deltas so core tools shared by other presets stay available.
    #[serde(default)]
    pub disabled_toolsets: Vec<String>,
    /// When true (default), spawn the extra LLM pass for memory/skill review — Python has no master off-switch.
    #[serde(default = "default_agent_background_review_enabled")]
    pub background_review_enabled: bool,
    /// Enable always-on workspace code indexing + repo-map context injection.
    #[serde(default = "default_agent_code_index_enabled")]
    pub code_index_enabled: bool,
    /// Maximum files included in repo-map prompt block.
    #[serde(default = "default_agent_code_index_max_files")]
    pub code_index_max_files: usize,
    /// Maximum symbols included in repo-map prompt block.
    #[serde(default = "default_agent_code_index_max_symbols")]
    pub code_index_max_symbols: usize,
    /// Enable LSP-style context injection after file operations.
    #[serde(default = "default_agent_lsp_context_enabled")]
    pub lsp_context_enabled: bool,
    /// Character budget for injected LSP context block.
    #[serde(default = "default_agent_lsp_context_max_chars")]
    pub lsp_context_max_chars: usize,
    /// Emit a preflight context-compression status before the first LLM call.
    #[serde(
        default = "default_agent_preflight_context_compress",
        deserialize_with = "deserialize_boolish"
    )]
    pub preflight_context_compress: bool,
    /// Optional provider request service tier. `fast` maps to provider `priority`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// Upstream-compatible API retry count for provider calls.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "apiMaxRetries"
    )]
    pub api_max_retries: Option<u32>,
    /// Legacy location for `prefill_messages_file`.
    ///
    /// The top-level key is canonical; this field is retained so older CLI and
    /// godmode-generated configs continue to work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefill_messages_file: Option<String>,
}

fn default_agent_memory_nudge_interval() -> u32 {
    10
}

fn default_agent_skill_nudge_interval() -> u32 {
    10
}

fn default_agent_skip_context_files() -> bool {
    false
}

fn default_agent_coding_context() -> String {
    "auto".to_string()
}

fn default_agent_background_review_enabled() -> bool {
    true
}

fn default_agent_code_index_enabled() -> bool {
    true
}

fn default_agent_code_index_max_files() -> usize {
    32
}

fn default_agent_code_index_max_symbols() -> usize {
    160
}

fn default_agent_lsp_context_enabled() -> bool {
    true
}

fn default_agent_lsp_context_max_chars() -> usize {
    2_800
}

fn default_agent_preflight_context_compress() -> bool {
    true
}

impl Default for AgentLoopBehaviorConfig {
    fn default() -> Self {
        Self {
            memory_nudge_interval: default_agent_memory_nudge_interval(),
            skill_creation_nudge_interval: default_agent_skill_nudge_interval(),
            skip_context_files: default_agent_skip_context_files(),
            coding_context: default_agent_coding_context(),
            disabled_toolsets: Vec::new(),
            background_review_enabled: default_agent_background_review_enabled(),
            code_index_enabled: default_agent_code_index_enabled(),
            code_index_max_files: default_agent_code_index_max_files(),
            code_index_max_symbols: default_agent_code_index_max_symbols(),
            lsp_context_enabled: default_agent_lsp_context_enabled(),
            lsp_context_max_chars: default_agent_lsp_context_max_chars(),
            preflight_context_compress: default_agent_preflight_context_compress(),
            service_tier: None,
            api_max_retries: None,
            prefill_messages_file: None,
        }
    }
}

impl AgentLoopBehaviorConfig {
    pub fn normalized_service_tier(&self) -> Option<String> {
        normalize_service_tier(self.service_tier.as_deref())
    }
}

pub fn normalize_service_tier(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim();
    if value.is_empty() {
        return None;
    }
    match value.to_ascii_lowercase().as_str() {
        "fast" | "priority" => Some("priority".to_string()),
        "off" | "normal" | "standard" | "default" | "none" => None,
        other => Some(other.to_string()),
    }
}

fn default_max_turns() -> u32 {
    250
}

fn deserialize_boolish<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    struct BoolishVisitor;

    impl<'de> Visitor<'de> for BoolishVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a bool or a bool-like string")
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
            Ok(value != 0)
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
            Ok(value != 0)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            match value.trim().to_ascii_lowercase().as_str() {
                "" | "0" | "false" | "no" | "off" => Ok(false),
                "1" | "true" | "yes" | "on" => Ok(true),
                other => Err(E::custom(format!("invalid bool-like value `{other}`"))),
            }
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            self.visit_str(&value)
        }
    }

    deserializer.deserialize_any(BoolishVisitor)
}

fn deserialize_option_boolish<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionBoolishVisitor;

    impl<'de> Visitor<'de> for OptionBoolishVisitor {
        type Value = Option<bool>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a bool, bool-like string, or null")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            deserialize_boolish(deserializer).map(Some)
        }
    }

    deserializer.deserialize_option(OptionBoolishVisitor)
}

fn deserialize_string_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringListVisitor;

    impl<'de> Visitor<'de> for StringListVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a string, comma-separated string, or list of strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            self.visit_str(&value)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<String>()? {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    values.push(trimmed.to_string());
                }
            }
            Ok(values)
        }
    }

    deserializer.deserialize_any(StringListVisitor)
}

fn deserialize_provider_model_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ProviderModelListVisitor;

    impl<'de> Visitor<'de> for ProviderModelListVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .write_str("a string, comma-separated string, list of strings, or map of model ids")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            self.visit_str(&value)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<String>()? {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    values.push(trimmed.to_string());
                }
            }
            Ok(values)
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some((key, _ignored)) = map.next_entry::<String, serde::de::IgnoredAny>()? {
                let trimmed = key.trim();
                if !trimmed.is_empty() {
                    values.push(trimmed.to_string());
                }
            }
            Ok(values)
        }
    }

    deserializer.deserialize_any(ProviderModelListVisitor)
}

fn default_tools() -> Vec<String> {
    vec![
        "bash".into(),
        "read".into(),
        "write".into(),
        "edit".into(),
        "glob".into(),
        "grep".into(),
        "web_search".into(),
        "web_fetch".into(),
    ]
}
