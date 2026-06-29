use std::cmp::Ordering;
use std::path::Path;

use hermes_billing::{AutoBlendContext, Language};
use thiserror::Error;

use super::block::{PersonaBlock, PersonaBlockKind};
use super::output_directive::{default_output_directive, render_output_directive};

#[derive(Debug, Clone)]
pub struct BlendDecision {
    pub block_kind: PersonaBlockKind,
    pub selected_lang: Language,
    pub reason: String,
}

#[derive(Debug, Error)]
pub enum AutoBlendError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub fn select_lang_for_block(block: &PersonaBlock, ctx: &AutoBlendContext) -> (Language, String) {
    if block.follow_user_locale {
        return (ctx.user_locale, "follow_user_locale=true".into());
    }

    match block.kind {
        PersonaBlockKind::OutputDirective => (
            ctx.user_locale,
            "output_directive always follows user locale".into(),
        ),

        PersonaBlockKind::Instruction => {
            if let Some(&forced) = ctx
                .model_profile
                .task_overrides
                .get(&ctx.vertical_task_category)
            {
                return (
                    forced,
                    format!("task_override for {:?}", ctx.vertical_task_category),
                );
            }

            let best = ctx
                .model_profile
                .language_scores
                .iter()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(Ordering::Equal))
                .map(|(lang, score)| (*lang, *score));

            if let Some((lang, score)) = best {
                (lang, format!("highest language score {score:.2}"))
            } else {
                (
                    ctx.model_profile.primary_lang,
                    "fallback to model primary_lang".into(),
                )
            }
        }

        PersonaBlockKind::Terminology
        | PersonaBlockKind::Examples
        | PersonaBlockKind::StyleHint => {
            let user_score = ctx
                .model_profile
                .language_scores
                .get(&ctx.user_locale)
                .copied()
                .unwrap_or(0.0);
            if user_score >= 0.5 {
                (
                    ctx.user_locale,
                    format!("user locale score {user_score:.2} >= 0.5"),
                )
            } else {
                (
                    ctx.model_profile.primary_lang,
                    format!("user locale score {user_score:.2} < 0.5, fallback primary"),
                )
            }
        }
    }
}

fn resolve_block_content(
    block: &PersonaBlock,
    vertical_dir: &Path,
    selected_lang: Language,
) -> Result<String, AutoBlendError> {
    if let Some(inline) = &block.inline {
        return Ok(inline.clone());
    }

    if let Some(dir_template) = &block.dir_template {
        let lang_tag = selected_lang.tag();
        let dir = dir_template.replace("{lang}", lang_tag);
        let dir_path = vertical_dir.join(dir);
        if !dir_path.exists() {
            return Ok(String::new());
        }
        let mut parts = Vec::new();
        for entry in std::fs::read_dir(&dir_path)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                parts.push(std::fs::read_to_string(entry.path())?);
            }
        }
        parts.sort();
        return Ok(parts.join("\n\n"));
    }

    let lang_tag = selected_lang.tag();
    if let Some(path) = block.variants.get(lang_tag) {
        return Ok(std::fs::read_to_string(vertical_dir.join(path))?);
    }

    if let Some(path) = block.variants.get(ctx_fallback_tag(selected_lang)) {
        return Ok(std::fs::read_to_string(vertical_dir.join(path))?);
    }

    if let Some((_, path)) = block.variants.iter().next() {
        return Ok(std::fs::read_to_string(vertical_dir.join(path))?);
    }

    Ok(String::new())
}

fn ctx_fallback_tag(lang: Language) -> &'static str {
    match lang {
        Language::ZhCN | Language::ZhHant => "zh-CN",
        Language::Ja => "ja",
        Language::En => "en",
    }
}

pub fn blend_persona(
    blocks: &[PersonaBlock],
    ctx: &AutoBlendContext,
    vertical_dir: &Path,
) -> Result<(String, Vec<BlendDecision>), AutoBlendError> {
    let mut sections = Vec::new();
    let mut decisions = Vec::new();
    let mut has_output_directive = false;

    for block in blocks {
        let (selected_lang, reason) = select_lang_for_block(block, ctx);

        let content = if block.kind == PersonaBlockKind::OutputDirective {
            has_output_directive = true;
            let template = block
                .inline
                .as_deref()
                .unwrap_or("Always respond in {{user_locale}}. Use markdown.");
            render_output_directive(template, ctx.user_locale)
        } else {
            resolve_block_content(block, vertical_dir, selected_lang)?
        };

        if !content.trim().is_empty() {
            sections.push(content);
        }

        decisions.push(BlendDecision {
            block_kind: block.kind,
            selected_lang,
            reason,
        });
    }

    if !has_output_directive {
        sections.push(default_output_directive(ctx.user_locale));
    }

    Ok((sections.join("\n\n"), decisions))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use hermes_billing::ModelLanguageProfile;
    use hermes_tasks::TaskCategory;

    use super::*;
    use crate::persona::block::PersonaBlock;

    fn profile() -> ModelLanguageProfile {
        hermes_billing::default_profile("tongyi-qwen-max")
    }

    fn ctx(user: Language, category: TaskCategory) -> AutoBlendContext {
        AutoBlendContext {
            model_profile: profile(),
            user_locale: user,
            vertical_task_category: category,
        }
    }

    #[test]
    fn instruction_uses_task_override_for_code() {
        let block = PersonaBlock {
            kind: PersonaBlockKind::Instruction,
            variants: HashMap::new(),
            follow_user_locale: false,
            inline: None,
            dir_template: None,
        };
        let (lang, reason) =
            select_lang_for_block(&block, &ctx(Language::ZhCN, TaskCategory::Code));
        assert_eq!(lang, Language::En);
        assert!(reason.contains("task_override"));
    }

    #[test]
    fn terminology_falls_back_when_user_score_low() {
        let block = PersonaBlock {
            kind: PersonaBlockKind::Terminology,
            variants: HashMap::new(),
            follow_user_locale: false,
            inline: None,
            dir_template: None,
        };
        let mut profile = profile();
        profile.language_scores.insert(Language::Ja, 0.2);
        let blend_ctx = AutoBlendContext {
            model_profile: profile,
            user_locale: Language::Ja,
            vertical_task_category: TaskCategory::Financial,
        };
        let (lang, _) = select_lang_for_block(&block, &blend_ctx);
        assert_eq!(lang, Language::ZhCN);
    }
}
