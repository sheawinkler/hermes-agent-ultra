use std::path::{Path, PathBuf};

use serde::Deserialize;

use hermes_tasks::TaskCategory;
use thiserror::Error;

use crate::persona::{PersonaDefinition, load_persona};

#[derive(Debug, Error)]
pub enum VerticalLoadError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct VerticalMeta {
    pub id: String,
    pub display_name_key: String,
    pub description_key: String,
    pub icon: String,
    pub category: String,
    pub order: u32,
    pub task_category: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FrontmatterDoc {
    meta: VerticalMeta,
}

#[derive(Debug, Clone)]
pub struct VerticalDefinition {
    pub meta: VerticalMeta,
    pub dir: PathBuf,
    pub starters: serde_json::Value,
    pub datasources: serde_json::Value,
    pub persona: PersonaDefinition,
}

pub struct VerticalLoader {
    bundled_root: PathBuf,
}

impl VerticalLoader {
    pub fn bundled() -> Self {
        Self {
            bundled_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundled"),
        }
    }

    pub fn with_root(root: impl AsRef<Path>) -> Self {
        Self {
            bundled_root: root.as_ref().to_path_buf(),
        }
    }

    pub fn list(&self) -> Result<Vec<VerticalDefinition>, VerticalLoadError> {
        let mut out = Vec::new();
        if !self.bundled_root.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&self.bundled_root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                out.push(self.load_vertical(entry.path())?);
            }
        }
        out.sort_by_key(|v| v.meta.order);
        Ok(out)
    }

    pub fn load(&self, id: &str) -> Result<VerticalDefinition, VerticalLoadError> {
        self.load_vertical(self.bundled_root.join(id))
    }

    fn load_vertical(&self, dir: PathBuf) -> Result<VerticalDefinition, VerticalLoadError> {
        let vertical_md = dir.join("VERTICAL.md");
        let content = std::fs::read_to_string(&vertical_md)?;
        let (frontmatter, _) = split_frontmatter(&content);
        let FrontmatterDoc { meta } = toml::from_str(&frontmatter)?;

        let starters = read_json_or_default(&dir.join("starters.json"));
        let datasources = read_json_or_default(&dir.join("datasources.json"));
        let persona = load_persona(&dir, &frontmatter)
            .map_err(|err| VerticalLoadError::Other(err.to_string()))?;

        Ok(VerticalDefinition {
            meta,
            dir,
            starters,
            datasources,
            persona,
        })
    }
}

impl VerticalMeta {
    pub fn task_category_enum(&self) -> Option<TaskCategory> {
        self.task_category
            .as_deref()
            .and_then(TaskCategory::from_str_loose)
    }
}

fn split_frontmatter(content: &str) -> (String, String) {
    if let Some(rest) = content.strip_prefix("---") {
        let rest = rest
            .strip_prefix("\r\n")
            .or_else(|| rest.strip_prefix('\n'))
            .unwrap_or(rest);
        if let Some(end) = rest.find("\n---").or_else(|| rest.find("\r\n---")) {
            let fm = rest[..end].replace("\r\n", "\n").trim().to_string();
            let body_start = end
                + if rest[end..].starts_with("\r\n---") {
                    5
                } else {
                    4
                };
            let body = rest[body_start..]
                .trim_start_matches(['\r', '\n'])
                .to_string();
            return (fm, body);
        }
    }
    (String::new(), content.to_string())
}

fn read_json_or_default(path: &Path) -> serde_json::Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!([]))
}

#[cfg(test)]
mod tests {
    use hermes_billing::{AutoBlendContext, Language, default_profile};
    use hermes_tasks::TaskCategory;

    use super::*;
    use crate::persona::{PersonaStrategy, blend_persona};

    #[test]
    fn bundled_trader_has_persona_blocks() {
        let loader = VerticalLoader::bundled();
        let trader = loader.load("trader").expect("trader vertical");
        assert_eq!(trader.meta.task_category.as_deref(), Some("Financial"));
        assert_eq!(trader.persona.strategy, PersonaStrategy::AutoBlend);
        assert!(trader.persona.blocks.len() >= 2);
    }

    #[test]
    fn trader_auto_blend_produces_prompt() {
        let loader = VerticalLoader::bundled();
        let trader = loader.load("trader").expect("trader vertical");
        let ctx = AutoBlendContext {
            model_profile: default_profile("tongyi-qwen-max"),
            user_locale: Language::ZhCN,
            vertical_task_category: TaskCategory::Financial,
        };
        let (prompt, decisions) =
            blend_persona(&trader.persona.blocks, &ctx, &trader.dir).expect("blend");
        assert!(prompt.contains("A 股"));
        assert!(prompt.contains("zh-CN"));
        assert!(!decisions.is_empty());
    }
}
