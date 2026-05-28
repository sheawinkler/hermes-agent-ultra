//! Per-channel prompts, skills, and topic resolution (P2-5 / P2-6 / P2-7).

use std::collections::HashMap;

use hermes_config::PlatformConfig;

/// One channel → skills binding from config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSkillBinding {
    pub id: String,
    pub skills: Vec<String>,
}

/// Resolve a channel-specific system prompt.
pub fn resolve_channel_prompt(
    prompts: &HashMap<String, String>,
    channel_id: &str,
    parent_id: Option<&str>,
) -> Option<String> {
    if let Some(p) = prompts.get(channel_id) {
        return Some(p.clone());
    }
    parent_id.and_then(|pid| prompts.get(pid)).cloned()
}

/// Resolve channel-bound skills (deduped, order preserved).
pub fn resolve_channel_skills(
    bindings: &[ChannelSkillBinding],
    channel_id: &str,
    parent_id: Option<&str>,
) -> Option<Vec<String>> {
    let skills = bindings
        .iter()
        .find(|b| b.id == channel_id)
        .or_else(|| {
            parent_id.and_then(|pid| bindings.iter().find(|b| b.id == pid))
        })
        .map(|b| b.skills.clone())?;
    if skills.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for s in skills {
        if !out.iter().any(|x| x == &s) {
            out.push(s);
        }
    }
    Some(out)
}

pub fn parse_channel_prompts(platform_cfg: &PlatformConfig) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(value) = platform_cfg.extra.get("channel_prompts") else {
        return map;
    };
    let Some(obj) = value.as_object() else {
        return map;
    };
    for (k, v) in obj {
        if let Some(text) = v.as_str() {
            if !text.trim().is_empty() {
                map.insert(k.clone(), text.to_string());
            }
        }
    }
    map
}

pub fn parse_channel_skill_bindings(platform_cfg: &PlatformConfig) -> Vec<ChannelSkillBinding> {
    let Some(value) = platform_cfg.extra.get("channel_skill_bindings") else {
        return Vec::new();
    };
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let id = obj
            .get("id")
            .and_then(|v| v.as_str().map(String::from))
            .or_else(|| obj.get("id").and_then(|v| v.as_u64().map(|n| n.to_string())));
        let Some(id) = id.filter(|s| !s.is_empty()) else {
            continue;
        };
        let mut skills = Vec::new();
        if let Some(s) = obj.get("skill").and_then(|v| v.as_str()) {
            skills.push(s.to_string());
        }
        if let Some(arr) = obj.get("skills").and_then(|v| v.as_array()) {
            for s in arr {
                if let Some(text) = s.as_str() {
                    skills.push(text.to_string());
                }
            }
        }
        if !skills.is_empty() {
            out.push(ChannelSkillBinding { id, skills });
        }
    }
    out
}

pub fn parse_history_backfill_limit(platform_cfg: &PlatformConfig) -> u32 {
    platform_cfg
        .extra
        .get("history_backfill")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(u32::MAX as u64) as u32)
        .or_else(|| {
            std::env::var("DISCORD_HISTORY_BACKFILL")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_exact_overrides_parent() {
        let mut prompts = HashMap::new();
        prompts.insert("200".into(), "Forum".into());
        prompts.insert("999".into(), "Thread".into());
        assert_eq!(
            resolve_channel_prompt(&prompts, "999", Some("200")).as_deref(),
            Some("Thread")
        );
    }

    #[test]
    fn skills_dedup_preserves_order() {
        let bindings = vec![ChannelSkillBinding {
            id: "100".into(),
            skills: vec!["a".into(), "b".into(), "a".into(), "c".into()],
        }];
        assert_eq!(
            resolve_channel_skills(&bindings, "100", None),
            Some(vec!["a".into(), "b".into(), "c".into()])
        );
    }
}
