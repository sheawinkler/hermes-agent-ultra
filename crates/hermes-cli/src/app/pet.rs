use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use hermes_core::AgentError;

use crate::paths::CliStateRoot;
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PetDock {
    Left,
    #[default]
    Right,
}

impl PetDock {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PetSettings {
    pub enabled: bool,
    pub species: String,
    pub mood: String,
    pub dock: PetDock,
    pub tick_ms: u64,
}

impl Default for PetSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            species: "boba".to_string(),
            mood: "ready".to_string(),
            dock: PetDock::Right,
            tick_ms: 420,
        }
    }
}

impl PetSettings {
    const SPECIES: [&'static str; 6] = ["boba", "bytecat", "otter", "fox", "owl", "capy"];
    const MOODS: [&'static str; 5] = ["ready", "working", "sleepy", "hyped", "chill"];
    const MIN_TICK_MS: u64 = 120;
    const MAX_TICK_MS: u64 = 2000;

    pub fn normalized(mut self) -> Self {
        let species = self.species.trim().to_ascii_lowercase();
        if Self::SPECIES.iter().any(|candidate| *candidate == species) {
            self.species = species;
        } else {
            self.species = Self::default().species;
        }

        let mood = self.mood.trim().to_ascii_lowercase();
        if Self::MOODS.iter().any(|candidate| *candidate == mood) {
            self.mood = mood;
        } else {
            self.mood = Self::default().mood;
        }

        self.tick_ms = self.tick_ms.clamp(Self::MIN_TICK_MS, Self::MAX_TICK_MS);
        self
    }

    pub fn species_catalog() -> &'static [&'static str] {
        &Self::SPECIES
    }

    pub fn mood_catalog() -> &'static [&'static str] {
        &Self::MOODS
    }
}

pub(super) fn pet_settings_path() -> PathBuf {
    CliStateRoot::from_config_dir(None).pet_settings()
}

fn parse_runtime_provider_api_mode(value: &str) -> Option<hermes_agent::agent_loop::ApiMode> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "chat_completions" => Some(hermes_agent::agent_loop::ApiMode::ChatCompletions),
        "anthropic_messages" => Some(hermes_agent::agent_loop::ApiMode::AnthropicMessages),
        "codex_responses" => Some(hermes_agent::agent_loop::ApiMode::CodexResponses),
        "bedrock_converse" => Some(hermes_agent::agent_loop::ApiMode::BedrockConverse),
        _ => None,
    }
}

fn parse_bool_env(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

pub(super) fn default_pet_settings() -> PetSettings {
    let mut settings = PetSettings::default();
    if let Ok(raw) = std::env::var("HERMES_PET") {
        if let Some(enabled) = parse_bool_env(&raw) {
            settings.enabled = enabled;
        }
    }
    if let Ok(raw) = std::env::var("HERMES_PET_SPECIES") {
        settings.species = raw;
    }
    if let Ok(raw) = std::env::var("HERMES_PET_MOOD") {
        settings.mood = raw;
    }
    if let Ok(raw) = std::env::var("HERMES_PET_DOCK") {
        settings.dock = if raw.trim().eq_ignore_ascii_case("left") {
            PetDock::Left
        } else {
            PetDock::Right
        };
    }
    if let Ok(raw) = std::env::var("HERMES_PET_TICK_MS") {
        if let Ok(value) = raw.trim().parse::<u64>() {
            settings.tick_ms = value;
        }
    }
    settings.normalized()
}

pub(super) fn load_pet_settings() -> PetSettings {
    let path = pet_settings_path();
    let from_file = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<PetSettings>(&raw).ok())
        .map(PetSettings::normalized);
    from_file.unwrap_or_else(default_pet_settings)
}

pub(super) fn persist_pet_settings(settings: &PetSettings) -> Result<(), AgentError> {
    let path = pet_settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create pet settings directory '{}': {}",
                parent.display(),
                e
            ))
        })?;
    }
    let body = serde_json::to_string_pretty(settings)
        .map_err(|e| AgentError::Config(format!("pet settings serialization failed: {e}")))?;
    std::fs::write(&path, format!("{body}\n")).map_err(|e| {
        AgentError::Io(format!(
            "Failed to persist pet settings '{}': {}",
            path.display(),
            e
        ))
    })
}
