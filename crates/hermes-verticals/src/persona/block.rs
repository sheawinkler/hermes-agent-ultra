use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaBlockKind {
    Instruction,
    Terminology,
    Examples,
    StyleHint,
    OutputDirective,
}

impl PersonaBlockKind {
    pub fn from_str_loose(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "instruction" => Some(Self::Instruction),
            "terminology" => Some(Self::Terminology),
            "examples" => Some(Self::Examples),
            "style_hint" | "stylehint" => Some(Self::StyleHint),
            "output_directive" | "outputdirective" => Some(Self::OutputDirective),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaBlock {
    pub kind: PersonaBlockKind,
    #[serde(default)]
    pub variants: HashMap<String, String>,
    #[serde(default)]
    pub follow_user_locale: bool,
    #[serde(default)]
    pub inline: Option<String>,
    #[serde(default)]
    pub dir_template: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonaStrategy {
    Static,
    AutoBlend,
}

impl PersonaStrategy {
    pub fn from_str_loose(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "static" => Some(Self::Static),
            "auto_blend" | "autoblend" => Some(Self::AutoBlend),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaDefinition {
    pub strategy: PersonaStrategy,
    pub blocks: Vec<PersonaBlock>,
}
