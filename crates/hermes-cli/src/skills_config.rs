use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillsConfig {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}
