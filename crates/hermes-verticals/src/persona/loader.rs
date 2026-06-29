use std::path::Path;

use thiserror::Error;

use super::block::{PersonaBlock, PersonaBlockKind, PersonaDefinition, PersonaStrategy};

#[derive(Debug, Error)]
pub enum PersonaLoadError {
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, serde::Deserialize)]
struct PersonaSection {
    #[serde(default)]
    strategy: Option<String>,
    #[serde(default)]
    blocks: Vec<PersonaBlockRaw>,
}

#[derive(Debug, serde::Deserialize)]
struct PersonaBlockRaw {
    kind: String,
    #[serde(default)]
    follow_user_locale: bool,
    #[serde(default)]
    variants: std::collections::HashMap<String, String>,
    #[serde(default)]
    dir_template: Option<String>,
    #[serde(default)]
    inline: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct VerticalFrontmatter {
    persona: Option<PersonaSection>,
}

pub fn parse_persona_from_frontmatter(
    frontmatter: &str,
) -> Result<PersonaDefinition, PersonaLoadError> {
    let normalized = frontmatter.replace("\r\n", "\n");
    let doc: VerticalFrontmatter = toml::from_str(normalized.trim())?;
    let persona = doc
        .persona
        .ok_or_else(|| PersonaLoadError::Other("missing [persona]".into()))?;

    let strategy = persona
        .strategy
        .as_deref()
        .and_then(PersonaStrategy::from_str_loose)
        .unwrap_or(PersonaStrategy::AutoBlend);

    let mut blocks = Vec::new();
    for raw in persona.blocks {
        let kind = PersonaBlockKind::from_str_loose(&raw.kind).ok_or_else(|| {
            PersonaLoadError::Other(format!("unknown persona block kind: {}", raw.kind))
        })?;
        blocks.push(PersonaBlock {
            kind,
            variants: raw.variants,
            follow_user_locale: raw.follow_user_locale,
            inline: raw.inline,
            dir_template: raw.dir_template,
        });
    }

    if blocks.is_empty() {
        return Err(PersonaLoadError::Other(
            "persona.blocks must contain at least one block".into(),
        ));
    }

    Ok(PersonaDefinition { strategy, blocks })
}

pub fn load_legacy_persona_files(vertical_dir: &Path) -> PersonaDefinition {
    let mut variants = std::collections::HashMap::new();
    for (tag, file) in [("en", "persona.en.md"), ("zh-CN", "persona.zh-CN.md")] {
        if vertical_dir.join(file).exists() {
            variants.insert(tag.into(), file.into());
        }
    }

    PersonaDefinition {
        strategy: PersonaStrategy::Static,
        blocks: vec![PersonaBlock {
            kind: PersonaBlockKind::Instruction,
            variants,
            follow_user_locale: false,
            inline: None,
            dir_template: None,
        }],
    }
}

pub fn load_persona(
    vertical_dir: &Path,
    frontmatter: &str,
) -> Result<PersonaDefinition, PersonaLoadError> {
    match parse_persona_from_frontmatter(frontmatter) {
        Ok(def) => Ok(def),
        Err(PersonaLoadError::Other(msg)) if msg.contains("missing [persona]") => {
            Ok(load_legacy_persona_files(vertical_dir))
        }
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persona::block::PersonaBlockKind;

    #[test]
    fn parses_persona_blocks() {
        let fm = r#"
[persona]
strategy = "auto_blend"

[[persona.blocks]]
kind = "instruction"
follow_user_locale = false
variants = { en = "instruction.en.md", "zh-CN" = "instruction.zh-CN.md" }

[[persona.blocks]]
kind = "output_directive"
follow_user_locale = true
inline = 'Always respond in {{user_locale}}.'
"#;
        let def = parse_persona_from_frontmatter(fm).expect("parse");
        assert_eq!(
            def.strategy,
            super::super::block::PersonaStrategy::AutoBlend
        );
        assert_eq!(def.blocks.len(), 2);
        assert_eq!(def.blocks[0].kind, PersonaBlockKind::Instruction);
        assert_eq!(def.blocks[1].kind, PersonaBlockKind::OutputDirective);
    }
}
