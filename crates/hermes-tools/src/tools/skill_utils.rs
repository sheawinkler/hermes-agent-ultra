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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequiredEnvironmentVariable {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_for: Option<String>,
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
    let body = after_first[body_start..]
        .trim_start_matches('\n')
        .to_string();

    let frontmatter: HashMap<String, Value> =
        serde_yaml::from_str(yaml_block).unwrap_or_else(|_| parse_frontmatter_fallback(yaml_block));

    (frontmatter, body)
}

fn parse_frontmatter_fallback(yaml_block: &str) -> HashMap<String, Value> {
    let mut frontmatter = HashMap::new();
    for line in yaml_block.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty()
            || key
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))
        {
            continue;
        }
        let value = value
            .trim()
            .trim_matches(|c| matches!(c, '"' | '\''))
            .to_string();
        frontmatter.insert(key.to_string(), Value::String(value));
    }
    frontmatter
}

pub fn parse_tags(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(clean_tag)
            .filter(|v| !v.is_empty())
            .collect(),
        Value::String(raw) => raw
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(clean_tag)
            .filter(|v| !v.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn clean_tag(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| matches!(c, '"' | '\''))
        .trim()
        .to_string()
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

        let mut stack = vec![dir.clone()];
        while let Some(current) = stack.pop() {
            let Some(dirname) = current.file_name().and_then(|v| v.to_str()) else {
                continue;
            };
            if should_skip_skill_scan_dir(dirname) && current.as_path() != dir.as_path() {
                continue;
            }

            let skill_md = current.join("SKILL.md");
            if skill_md.exists() {
                if let Ok(content) = std::fs::read_to_string(&skill_md) {
                    let (frontmatter, body) = parse_frontmatter(&content);
                    let name = frontmatter
                        .get("name")
                        .and_then(|v| v.as_str())
                        .filter(|v| !v.trim().is_empty())
                        .unwrap_or(dirname)
                        .to_string();

                    skills.push(SkillInfo {
                        name,
                        path: current.clone(),
                        content,
                        frontmatter,
                        body,
                    });
                }
                continue;
            }

            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    skills
}

fn should_skip_skill_scan_dir(dirname: &str) -> bool {
    dirname.starts_with('.')
        || matches!(
            dirname,
            "node_modules" | "site-packages" | "__pycache__" | "venv" | ".venv" | "target"
        )
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
                let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("custom");
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

pub fn extract_required_environment_variables(
    frontmatter: &HashMap<String, Value>,
) -> Vec<RequiredEnvironmentVariable> {
    let mut vars = Vec::new();

    if let Some(Value::Array(items)) = frontmatter.get("required_environment_variables") {
        for item in items {
            match item {
                Value::String(name) if !name.trim().is_empty() => {
                    vars.push(RequiredEnvironmentVariable {
                        name: name.trim().to_string(),
                        prompt: Some(format!("Enter value for {}", name.trim())),
                        help: None,
                        required_for: None,
                    });
                }
                Value::Object(obj) => {
                    let Some(name) = obj.get("name").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    if name.trim().is_empty() {
                        continue;
                    }
                    vars.push(RequiredEnvironmentVariable {
                        name: name.trim().to_string(),
                        prompt: obj.get("prompt").and_then(|v| v.as_str()).map(String::from),
                        help: obj.get("help").and_then(|v| v.as_str()).map(String::from),
                        required_for: obj
                            .get("required_for")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    });
                }
                _ => {}
            }
        }
    }

    if let Some(Value::Object(prereqs)) = frontmatter.get("prerequisites") {
        if let Some(Value::Array(env_vars)) = prereqs.get("env_vars") {
            for item in env_vars.iter().filter_map(|v| v.as_str()) {
                let name = item.trim();
                if name.is_empty() || vars.iter().any(|v| v.name == name) {
                    continue;
                }
                vars.push(RequiredEnvironmentVariable {
                    name: name.to_string(),
                    prompt: Some(format!("Enter value for {name}")),
                    help: None,
                    required_for: None,
                });
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
    fn test_parse_frontmatter_malformed_yaml_fallback_keeps_simple_keys() {
        let content = "---\nname: test-skill\ndescription: desc\n: invalid\n---\n# Body";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.get("name").and_then(|v| v.as_str()), Some("test-skill"));
        assert_eq!(fm.get("description").and_then(|v| v.as_str()), Some("desc"));
        assert_eq!(body, "# Body");
    }

    #[test]
    fn test_parse_tags_variants() {
        assert_eq!(
            parse_tags(&serde_json::json!(["a", "b", ""])),
            vec!["a", "b"]
        );
        assert_eq!(
            parse_tags(&Value::String("\"tag1\", 'tag2'".into())),
            vec!["tag1", "tag2"]
        );
        assert_eq!(
            parse_tags(&Value::String("[a, b, c]".into())),
            vec!["a", "b", "c"]
        );
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
        fm.insert("platform".into(), serde_json::json!(["darwin", "linux"]));
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
    fn test_extract_required_environment_variables_new_and_legacy() {
        let mut fm = HashMap::new();
        fm.insert(
            "required_environment_variables".into(),
            serde_json::json!([
                {
                    "name": "TENOR_API_KEY",
                    "prompt": "Tenor API key",
                    "help": "Get a key",
                    "required_for": "full functionality"
                }
            ]),
        );
        fm.insert(
            "prerequisites".into(),
            serde_json::json!({"env_vars": ["LEGACY_KEY", "TENOR_API_KEY"]}),
        );

        let vars = extract_required_environment_variables(&fm);
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].name, "TENOR_API_KEY");
        assert_eq!(vars[0].prompt.as_deref(), Some("Tenor API key"));
        assert_eq!(vars[1].name, "LEGACY_KEY");
        assert_eq!(
            vars[1].prompt.as_deref(),
            Some("Enter value for LEGACY_KEY")
        );
    }

    #[test]
    fn test_discover_skills_recurses_categories_and_skips_hidden_dependency_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("mlops").join("axolotl")).unwrap();
        std::fs::write(
            root.join("mlops").join("axolotl").join("SKILL.md"),
            "---\nname: axolotl-skill\n---\n# Body",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".venv").join("fake")).unwrap();
        std::fs::write(root.join(".venv").join("fake").join("SKILL.md"), "# Fake").unwrap();

        let skills = discover_skills(&[root.to_path_buf()]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "axolotl-skill");
        assert!(skills[0].path.ends_with("mlops/axolotl"));
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
