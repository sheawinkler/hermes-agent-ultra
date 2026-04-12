//! Skill utility functions for parsing, discovering, and managing skills.
//!
//! Provides helpers for YAML frontmatter extraction, skill directory scanning,
//! condition and config variable parsing, platform matching, and disabled-skill
//! filtering.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Metadata about a discovered skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub path: PathBuf,
    pub content: String,
    pub frontmatter: HashMap<String, Value>,
    pub body: String,
}

/// A condition that controls when a skill is active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCondition {
    pub kind: ConditionKind,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConditionKind {
    Platform,
    EnvVar,
    FileExists,
    Command,
    Custom,
}

/// A configuration variable declared by a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigVar {
    pub name: String,
    pub description: String,
    pub default: Option<String>,
    pub required: bool,
    pub env_var: Option<String>,
}

// ---------------------------------------------------------------------------
// Frontmatter parsing
// ---------------------------------------------------------------------------

/// Parse YAML frontmatter delimited by `---` from a SKILL.md file.
///
/// Returns the parsed frontmatter as a map and the remaining body content.
/// If no frontmatter is found, returns an empty map and the full content.
pub fn parse_frontmatter(content: &str) -> (HashMap<String, Value>, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (HashMap::new(), content.to_string());
    }

    let after_first = &trimmed[3..];
    let Some(end_idx) = after_first.find("\n---") else {
        return (HashMap::new(), content.to_string());
    };

    let yaml_block = &after_first[..end_idx];
    let body_start = end_idx + 4; // skip "\n---"
    let body = after_first[body_start..].trim_start_matches('\n').to_string();

    let frontmatter: HashMap<String, Value> = serde_yaml::from_str(yaml_block)
        .unwrap_or_default();

    (frontmatter, body)
}

// ---------------------------------------------------------------------------
// Skill discovery
// ---------------------------------------------------------------------------

/// Scan multiple directories for skills (each subdirectory containing a SKILL.md).
pub fn discover_skills(dirs: &[PathBuf]) -> Vec<SkillInfo> {
    let mut skills = Vec::new();

    for dir in dirs {
        if !dir.exists() || !dir.is_dir() {
            continue;
        }

        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let skill_md = path.join("SKILL.md");
            if !skill_md.exists() {
                continue;
            }

            let Ok(content) = std::fs::read_to_string(&skill_md) else {
                continue;
            };

            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();

            let (frontmatter, body) = parse_frontmatter(&content);

            skills.push(SkillInfo {
                name,
                path: path.clone(),
                content,
                frontmatter,
                body,
            });
        }

        // Also check for standalone SKILL.md files (not in subdirs)
        let standalone = dir.join("SKILL.md");
        if standalone.exists() {
            if let Ok(content) = std::fs::read_to_string(&standalone) {
                let name = dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let (frontmatter, body) = parse_frontmatter(&content);
                skills.push(SkillInfo {
                    name,
                    path: dir.clone(),
                    content,
                    frontmatter,
                    body,
                });
            }
        }
    }

    skills
}

// ---------------------------------------------------------------------------
// Condition extraction
// ---------------------------------------------------------------------------

/// Extract activation conditions from a skill's frontmatter.
pub fn extract_skill_conditions(skill: &SkillInfo) -> Vec<SkillCondition> {
    let mut conditions = Vec::new();

    if let Some(Value::Array(arr)) = skill.frontmatter.get("conditions") {
        for item in arr {
            if let Some(obj) = item.as_object() {
                let kind = obj
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("custom");
                let value = obj
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();

                let kind = match kind {
                    "platform" => ConditionKind::Platform,
                    "env" | "env_var" => ConditionKind::EnvVar,
                    "file_exists" | "file" => ConditionKind::FileExists,
                    "command" | "cmd" => ConditionKind::Command,
                    _ => ConditionKind::Custom,
                };

                conditions.push(SkillCondition {
                    kind,
                    value: value.to_string(),
                });
            }
        }
    }

    if let Some(Value::String(platform)) = skill.frontmatter.get("platform") {
        conditions.push(SkillCondition {
            kind: ConditionKind::Platform,
            value: platform.clone(),
        });
    }

    conditions
}

// ---------------------------------------------------------------------------
// Config variable extraction
// ---------------------------------------------------------------------------

/// Extract configuration variables declared in a skill's frontmatter.
pub fn extract_skill_config_vars(skill: &SkillInfo) -> Vec<ConfigVar> {
    let mut vars = Vec::new();

    let config_key = skill
        .frontmatter
        .get("config_vars")
        .or_else(|| skill.frontmatter.get("config"));

    if let Some(Value::Array(arr)) = config_key {
        for item in arr {
            if let Some(obj) = item.as_object() {
                let name = obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let description = obj
                    .get("description")
                    .or_else(|| obj.get("desc"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let default = obj
                    .get("default")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let required = obj
                    .get("required")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let env_var = obj
                    .get("env_var")
                    .or_else(|| obj.get("env"))
                    .and_then(|v| v.as_str())
                    .map(String::from);

                if !name.is_empty() {
                    vars.push(ConfigVar {
                        name,
                        description,
                        default,
                        required,
                        env_var,
                    });
                }
            }
        }
    }

    vars
}

/// Resolve config variable values from environment and defaults.
pub fn resolve_skill_config_values(
    vars: &[ConfigVar],
    env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut resolved = HashMap::new();

    for var in vars {
        // Priority: env override -> env_var -> default
        if let Some(val) = env.get(&var.name) {
            resolved.insert(var.name.clone(), val.clone());
        } else if let Some(env_name) = &var.env_var {
            if let Some(val) = env.get(env_name) {
                resolved.insert(var.name.clone(), val.clone());
            } else if let Ok(val) = std::env::var(env_name) {
                resolved.insert(var.name.clone(), val);
            } else if let Some(def) = &var.default {
                resolved.insert(var.name.clone(), def.clone());
            }
        } else if let Some(def) = &var.default {
            resolved.insert(var.name.clone(), def.clone());
        }
    }

    resolved
}

// ---------------------------------------------------------------------------
// Index files
// ---------------------------------------------------------------------------

/// Iterate over skill index files (skills.json, index.json) in a directory.
pub fn iter_skill_index_files(dir: &Path) -> Vec<PathBuf> {
    let candidates = ["skills.json", "index.json", "skills.yaml", "index.yaml"];
    candidates
        .iter()
        .map(|name| dir.join(name))
        .filter(|p| p.exists())
        .collect()
}

// ---------------------------------------------------------------------------
// Disabled skills
// ---------------------------------------------------------------------------

/// Extract the set of disabled skill names from a config value.
///
/// Looks for `disabled_skills` as an array of strings.
pub fn get_disabled_skill_names(config: &Value) -> HashSet<String> {
    config
        .get("disabled_skills")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Platform matching
// ---------------------------------------------------------------------------

/// Check if a skill matches the given platform (e.g. "darwin", "linux", "windows").
///
/// If the skill has no platform constraint, it matches all platforms.
pub fn match_platform(skill: &SkillInfo, platform: &str) -> bool {
    match skill.frontmatter.get("platform") {
        None => true,
        Some(Value::String(p)) => {
            let p = p.to_lowercase();
            let platform = platform.to_lowercase();
            p == platform || p == "all" || p == "*"
        }
        Some(Value::Array(arr)) => {
            let platform = platform.to_lowercase();
            arr.iter().any(|v| {
                v.as_str()
                    .map(|s| s.to_lowercase() == platform)
                    .unwrap_or(false)
            })
        }
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter_with_yaml() {
        let content = "---\nname: test\nversion: \"1.0\"\n---\n# Body\nHello world";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.get("name").and_then(|v| v.as_str()), Some("test"));
        assert!(body.starts_with("# Body"));
    }

    #[test]
    fn test_parse_frontmatter_none() {
        let content = "# No frontmatter\nJust content";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.is_empty());
        assert_eq!(body, content);
    }

    #[test]
    fn test_match_platform_any() {
        let skill = SkillInfo {
            name: "test".into(),
            path: PathBuf::from("/tmp"),
            content: String::new(),
            frontmatter: HashMap::new(),
            body: String::new(),
        };
        assert!(match_platform(&skill, "darwin"));
        assert!(match_platform(&skill, "linux"));
    }

    #[test]
    fn test_match_platform_specific() {
        let mut fm = HashMap::new();
        fm.insert("platform".into(), Value::String("darwin".into()));
        let skill = SkillInfo {
            name: "mac-only".into(),
            path: PathBuf::from("/tmp"),
            content: String::new(),
            frontmatter: fm,
            body: String::new(),
        };
        assert!(match_platform(&skill, "darwin"));
        assert!(!match_platform(&skill, "linux"));
    }

    #[test]
    fn test_match_platform_array() {
        let mut fm = HashMap::new();
        fm.insert(
            "platform".into(),
            serde_json::json!(["darwin", "linux"]),
        );
        let skill = SkillInfo {
            name: "unix".into(),
            path: PathBuf::from("/tmp"),
            content: String::new(),
            frontmatter: fm,
            body: String::new(),
        };
        assert!(match_platform(&skill, "darwin"));
        assert!(match_platform(&skill, "linux"));
        assert!(!match_platform(&skill, "windows"));
    }

    #[test]
    fn test_disabled_skill_names() {
        let config = serde_json::json!({
            "disabled_skills": ["skill-a", "skill-b"]
        });
        let disabled = get_disabled_skill_names(&config);
        assert!(disabled.contains("skill-a"));
        assert!(disabled.contains("skill-b"));
        assert!(!disabled.contains("skill-c"));
    }

    #[test]
    fn test_disabled_skill_names_empty() {
        let config = serde_json::json!({});
        let disabled = get_disabled_skill_names(&config);
        assert!(disabled.is_empty());
    }

    #[test]
    fn test_extract_config_vars() {
        let mut fm = HashMap::new();
        fm.insert(
            "config_vars".into(),
            serde_json::json!([
                {"name": "API_KEY", "description": "API key", "required": true, "env_var": "MY_API_KEY"},
                {"name": "TIMEOUT", "desc": "Timeout in seconds", "default": "30"}
            ]),
        );
        let skill = SkillInfo {
            name: "test".into(),
            path: PathBuf::from("/tmp"),
            content: String::new(),
            frontmatter: fm,
            body: String::new(),
        };
        let vars = extract_skill_config_vars(&skill);
        assert_eq!(vars.len(), 2);
        assert!(vars[0].required);
        assert_eq!(vars[0].env_var.as_deref(), Some("MY_API_KEY"));
        assert_eq!(vars[1].default.as_deref(), Some("30"));
    }

    #[test]
    fn test_resolve_config_values() {
        let vars = vec![
            ConfigVar {
                name: "API_KEY".into(),
                description: "key".into(),
                default: Some("default-key".into()),
                required: true,
                env_var: None,
            },
            ConfigVar {
                name: "TIMEOUT".into(),
                description: "timeout".into(),
                default: Some("30".into()),
                required: false,
                env_var: None,
            },
        ];
        let mut env = HashMap::new();
        env.insert("API_KEY".into(), "real-key".into());
        let resolved = resolve_skill_config_values(&vars, &env);
        assert_eq!(resolved.get("API_KEY").unwrap(), "real-key");
        assert_eq!(resolved.get("TIMEOUT").unwrap(), "30");
    }

    #[test]
    fn test_extract_conditions_platform() {
        let mut fm = HashMap::new();
        fm.insert("platform".into(), Value::String("darwin".into()));
        let skill = SkillInfo {
            name: "test".into(),
            path: PathBuf::from("/tmp"),
            content: String::new(),
            frontmatter: fm,
            body: String::new(),
        };
        let conds = extract_skill_conditions(&skill);
        assert_eq!(conds.len(), 1);
        assert_eq!(conds[0].kind, ConditionKind::Platform);
        assert_eq!(conds[0].value, "darwin");
    }

    #[test]
    fn test_iter_skill_index_files_empty() {
        let dir = PathBuf::from("/nonexistent/dir");
        let files = iter_skill_index_files(&dir);
        assert!(files.is_empty());
    }
}
