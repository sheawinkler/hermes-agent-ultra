//! Skill slash-command support.
//!
//! Skills can declare `/command` handlers in their frontmatter. This module
//! provides a registry that maps slash commands to skills, enabling users to
//! invoke skill-provided functionality directly from the REPL.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use hermes_core::Skill;
use hermes_skills::SkillGuard;

use super::skill_utils::{discover_skills, match_platform, SkillInfo};

// ---------------------------------------------------------------------------
// SkillCommand
// ---------------------------------------------------------------------------

/// A slash command defined by a skill.
#[derive(Debug, Clone)]
pub struct SkillCommand {
    /// The command name without the `/` prefix (e.g. "plan", "review").
    pub name: String,
    /// Description shown in help output.
    pub description: String,
    /// The skill that owns this command.
    pub skill_name: String,
    /// Template content to inject when the command is invoked.
    /// May contain `{args}` placeholder for user-provided arguments.
    pub template: String,
}

// ---------------------------------------------------------------------------
// SkillCommandRegistry
// ---------------------------------------------------------------------------

/// Registry of slash commands contributed by skills.
pub struct SkillCommandRegistry {
    commands: HashMap<String, SkillCommand>,
}

/// Runtime configuration for resolving installed skills as slash commands.
#[derive(Debug, Clone)]
pub struct SkillCommandResolverConfig {
    /// Skill roots to scan. Each root may contain nested `SKILL.md` files.
    pub roots: Vec<PathBuf>,
    /// Optional allow-list of skill names or command slugs.
    pub enabled: Vec<String>,
    /// Optional deny-list of skill names or command slugs.
    pub disabled: Vec<String>,
    /// Platform label used for skill frontmatter filtering.
    pub platform: Option<String>,
}

impl Default for SkillCommandResolverConfig {
    fn default() -> Self {
        Self {
            roots: default_skill_roots(),
            enabled: Vec::new(),
            disabled: Vec::new(),
            platform: Some(std::env::consts::OS.to_string()),
        }
    }
}

/// Resolved installed-skill slash invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSlashInvocation {
    /// Canonical command key, including leading slash.
    pub command: String,
    /// Skill name from frontmatter or the skill directory.
    pub skill_name: String,
    /// Short description from frontmatter, if present.
    pub description: String,
    /// Path to the resolved `SKILL.md`.
    pub skill_md_path: PathBuf,
    /// User message content to send into the agent loop.
    pub message: String,
}

impl SkillCommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
        }
    }

    /// Register a single skill command.
    pub fn register(&mut self, cmd: SkillCommand) {
        self.commands.insert(cmd.name.clone(), cmd);
    }

    /// Look up a command by name.
    pub fn get(&self, name: &str) -> Option<&SkillCommand> {
        self.commands.get(name)
    }

    /// List all registered skill commands.
    pub fn list(&self) -> Vec<&SkillCommand> {
        self.commands.values().collect()
    }

    /// Return command names for auto-complete.
    pub fn command_names(&self) -> Vec<String> {
        self.commands.keys().map(|k| format!("/{}", k)).collect()
    }

    /// Number of registered commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

impl Default for SkillCommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Default installed skill roots shared by CLI and gateway surfaces.
pub fn default_skill_roots() -> Vec<PathBuf> {
    let mut roots = vec![hermes_config::skills_dir()];
    if let Some(home) = std::env::var("HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
    {
        roots.push(home.join(".hermes").join("skills"));
    }
    roots.sort();
    roots.dedup();
    roots
}

fn normalize_selector(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('/')
        .replace(['_', ' '], "-")
        .to_ascii_lowercase()
}

fn slugify_skill_command_name(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.trim().chars() {
        let next = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if matches!(ch, '-' | '_' | ' ') {
            Some('-')
        } else {
            None
        };
        let Some(ch) = next else {
            continue;
        };
        if ch == '-' {
            if out.is_empty() || last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        out.push(ch);
    }
    out.trim_matches('-').to_string()
}

fn selector_set(values: &[String]) -> HashSet<String> {
    values
        .iter()
        .map(|value| normalize_selector(value))
        .filter(|value| !value.is_empty())
        .collect()
}

fn skill_allowed(
    skill_name: &str,
    command_slug: &str,
    enabled: &[String],
    disabled: &[String],
) -> bool {
    let name_key = normalize_selector(skill_name);
    let cmd_key = normalize_selector(command_slug);
    let disabled = selector_set(disabled);
    if disabled.contains(&name_key) || disabled.contains(&cmd_key) {
        return false;
    }
    let enabled = selector_set(enabled);
    enabled.is_empty() || enabled.contains(&name_key) || enabled.contains(&cmd_key)
}

fn platform_aliases(platform: Option<&str>) -> Vec<String> {
    let raw = platform
        .unwrap_or(std::env::consts::OS)
        .trim()
        .to_ascii_lowercase();
    match raw.as_str() {
        "macos" | "darwin" => vec!["macos".to_string(), "darwin".to_string()],
        "windows" | "win32" => vec!["windows".to_string(), "win32".to_string()],
        "linux" => vec!["linux".to_string()],
        "" => vec![std::env::consts::OS.to_string()],
        other => vec![other.to_string()],
    }
}

fn skill_matches_platform(skill: &SkillInfo, platform: Option<&str>) -> bool {
    platform_aliases(platform)
        .iter()
        .any(|alias| match_platform(skill, alias))
}

fn security_gate_skill_content(name: &str, body: &str) -> Result<(), String> {
    let probe = Skill {
        name: name.to_string(),
        content: body.to_string(),
        category: Some("external".to_string()),
        description: None,
    };
    SkillGuard::default()
        .scan_security_only(&probe)
        .map_err(|err| err.to_string())
}

/// Build the agent-facing message for a skill slash command invocation.
pub fn build_skill_slash_invocation_message(
    skill_name: &str,
    skill_body: &str,
    user_instruction: &str,
) -> String {
    let mut parts = vec![
        format!(
            "[SYSTEM: The user has invoked the \"{}\" skill, indicating they want you to follow its instructions. The full skill content is loaded below.]",
            skill_name
        ),
        String::new(),
        skill_body.trim().to_string(),
    ];

    if !user_instruction.trim().is_empty() {
        parts.push(String::new());
        parts.push(format!(
            "The user has provided the following instruction alongside the skill invocation: {}",
            user_instruction.trim()
        ));
    }

    parts.join("\n")
}

/// Resolve an otherwise-unknown slash command against installed `SKILL.md` files.
///
/// Built-in and quick-command handlers should run before this resolver. The
/// resolver only returns `Some` when the command slug matches a discovered
/// installed skill.
pub fn resolve_installed_skill_slash_command(
    command: &str,
    args: &str,
    config: &SkillCommandResolverConfig,
) -> Result<Option<SkillSlashInvocation>, String> {
    let requested = normalize_selector(command);
    if requested.is_empty() {
        return Ok(None);
    }

    let skills = discover_skills(&config.roots);
    for skill in skills {
        if !skill_matches_platform(&skill, config.platform.as_deref()) {
            continue;
        }
        let slug = slugify_skill_command_name(&skill.name);
        if slug.is_empty() || slug != requested {
            continue;
        }
        if !skill_allowed(&skill.name, &slug, &config.enabled, &config.disabled) {
            return Ok(None);
        }
        security_gate_skill_content(&skill.name, &skill.body)?;
        let skill_md_path = skill.path.join("SKILL.md");
        let description = skill
            .frontmatter
            .get("description")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let message = build_skill_slash_invocation_message(&skill.name, &skill.body, args);
        return Ok(Some(SkillSlashInvocation {
            command: format!("/{slug}"),
            skill_name: skill.name,
            description,
            skill_md_path,
            message,
        }));
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// Registration helpers
// ---------------------------------------------------------------------------

/// Scan skills for `/command` definitions in their frontmatter and register them.
///
/// Frontmatter format:
/// ```yaml
/// commands:
///   - name: plan
///     description: "Generate an execution plan"
///     template: "Create a detailed plan for: {args}"
///   - name: review
///     description: "Review code changes"
///     template: "Review the following: {args}"
/// ```
pub fn register_skill_commands(skills: &[SkillInfo]) -> SkillCommandRegistry {
    let mut registry = SkillCommandRegistry::new();

    for skill in skills {
        let Some(commands_val) = skill.frontmatter.get("commands") else {
            continue;
        };

        let Some(commands_arr) = commands_val.as_array() else {
            continue;
        };

        for cmd_val in commands_arr {
            let Some(obj) = cmd_val.as_object() else {
                continue;
            };

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
            let template = obj
                .get("template")
                .and_then(|v| v.as_str())
                .unwrap_or("{args}")
                .to_string();

            if !name.is_empty() {
                registry.register(SkillCommand {
                    name,
                    description,
                    skill_name: skill.name.clone(),
                    template,
                });
            }
        }
    }

    registry
}

/// Handle a skill command invocation.
///
/// Returns the skill content to inject into the conversation, with `{args}`
/// replaced by the user's arguments.
pub fn handle_skill_command(
    registry: &SkillCommandRegistry,
    command: &str,
    args: &str,
) -> Option<String> {
    let cmd_name = command.trim_start_matches('/');
    let cmd = registry.get(cmd_name)?;
    let content = cmd.template.replace("{args}", args);
    Some(content)
}

/// Get skills that respond to the `/plan` command.
pub fn get_plan_command_skills(skills: &[SkillInfo]) -> Vec<&SkillInfo> {
    skills
        .iter()
        .filter(|skill| {
            skill
                .frontmatter
                .get("commands")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter().any(|cmd| {
                        cmd.as_object()
                            .and_then(|o| o.get("name"))
                            .and_then(|v| v.as_str())
                            .map(|n| n == "plan")
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn make_skill_with_commands() -> SkillInfo {
        let mut frontmatter = HashMap::new();
        frontmatter.insert(
            "commands".to_string(),
            serde_json::json!([
                {
                    "name": "plan",
                    "description": "Generate a plan",
                    "template": "Create a detailed plan for: {args}"
                },
                {
                    "name": "review",
                    "description": "Review code",
                    "template": "Review this code: {args}"
                }
            ]),
        );

        SkillInfo {
            name: "planning".to_string(),
            path: PathBuf::from("/skills/planning"),
            content: String::new(),
            frontmatter,
            body: String::new(),
        }
    }

    fn make_skill_no_commands() -> SkillInfo {
        SkillInfo {
            name: "basic".to_string(),
            path: PathBuf::from("/skills/basic"),
            content: String::new(),
            frontmatter: HashMap::new(),
            body: String::new(),
        }
    }

    fn write_skill(root: &std::path::Path, dir: &str, frontmatter: &str, body: &str) {
        let skill_dir = root.join(dir);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\n{frontmatter}\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn test_register_skill_commands() {
        let skills = vec![make_skill_with_commands(), make_skill_no_commands()];
        let registry = register_skill_commands(&skills);
        assert_eq!(registry.len(), 2);
        assert!(registry.get("plan").is_some());
        assert!(registry.get("review").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_handle_skill_command() {
        let skills = vec![make_skill_with_commands()];
        let registry = register_skill_commands(&skills);

        let result = handle_skill_command(&registry, "/plan", "build a REST API");
        assert_eq!(
            result.unwrap(),
            "Create a detailed plan for: build a REST API"
        );
    }

    #[test]
    fn test_handle_unknown_command() {
        let registry = SkillCommandRegistry::new();
        let result = handle_skill_command(&registry, "/unknown", "args");
        assert!(result.is_none());
    }

    #[test]
    fn test_get_plan_command_skills() {
        let skills = vec![make_skill_with_commands(), make_skill_no_commands()];
        let plan_skills = get_plan_command_skills(&skills);
        assert_eq!(plan_skills.len(), 1);
        assert_eq!(plan_skills[0].name, "planning");
    }

    #[test]
    fn test_command_names() {
        let skills = vec![make_skill_with_commands()];
        let registry = register_skill_commands(&skills);
        let names = registry.command_names();
        assert!(names.contains(&"/plan".to_string()));
        assert!(names.contains(&"/review".to_string()));
    }

    #[test]
    fn test_empty_registry() {
        let registry = SkillCommandRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.list().is_empty());
    }

    #[test]
    fn resolves_installed_skill_by_slug_and_builds_agent_message() {
        let tmp = tempdir().unwrap();
        write_skill(
            tmp.path(),
            "release-captain",
            "name: Release Captain\ndescription: Ship releases safely",
            "# Release Captain\n1. Inspect changes\n2. Verify gates",
        );
        let config = SkillCommandResolverConfig {
            roots: vec![tmp.path().to_path_buf()],
            platform: Some("linux".to_string()),
            ..SkillCommandResolverConfig::default()
        };

        let resolved = resolve_installed_skill_slash_command("/release_captain", "cut v1", &config)
            .unwrap()
            .expect("skill command");

        assert_eq!(resolved.command, "/release-captain");
        assert_eq!(resolved.skill_name, "Release Captain");
        assert_eq!(resolved.description, "Ship releases safely");
        assert!(resolved.message.contains("Release Captain"));
        assert!(resolved.message.contains("Verify gates"));
        assert!(resolved.message.contains("cut v1"));
    }

    #[test]
    fn resolver_respects_enabled_disabled_and_platform_filters() {
        let tmp = tempdir().unwrap();
        write_skill(
            tmp.path(),
            "mac-helper",
            "name: mac-helper\nplatform: darwin",
            "# Mac Helper\n1. Use macOS behavior",
        );
        write_skill(
            tmp.path(),
            "beta",
            "name: beta\ndescription: Beta skill",
            "# Beta\n1. Do beta work",
        );

        let linux = SkillCommandResolverConfig {
            roots: vec![tmp.path().to_path_buf()],
            platform: Some("linux".to_string()),
            ..SkillCommandResolverConfig::default()
        };
        assert!(
            resolve_installed_skill_slash_command("/mac-helper", "", &linux)
                .unwrap()
                .is_none()
        );

        let disabled = SkillCommandResolverConfig {
            roots: vec![tmp.path().to_path_buf()],
            disabled: vec!["beta".to_string()],
            platform: Some("linux".to_string()),
            ..SkillCommandResolverConfig::default()
        };
        assert!(
            resolve_installed_skill_slash_command("/beta", "", &disabled)
                .unwrap()
                .is_none()
        );

        let enabled = SkillCommandResolverConfig {
            roots: vec![tmp.path().to_path_buf()],
            enabled: vec!["beta".to_string()],
            platform: Some("linux".to_string()),
            ..SkillCommandResolverConfig::default()
        };
        assert!(resolve_installed_skill_slash_command("/beta", "", &enabled)
            .unwrap()
            .is_some());
        assert!(
            resolve_installed_skill_slash_command("/mac-helper", "", &enabled)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn resolver_blocks_dangerous_skill_content() {
        let tmp = tempdir().unwrap();
        write_skill(
            tmp.path(),
            "danger",
            "name: danger",
            "# Danger\n1. Run rm -rf /",
        );
        let config = SkillCommandResolverConfig {
            roots: vec![tmp.path().to_path_buf()],
            platform: Some("linux".to_string()),
            ..SkillCommandResolverConfig::default()
        };

        let err = resolve_installed_skill_slash_command("/danger", "", &config)
            .expect_err("dangerous skill should be blocked");
        assert!(err.contains("Guard violation:"));
        assert!(err.contains("Blocked content:"));
    }
}
