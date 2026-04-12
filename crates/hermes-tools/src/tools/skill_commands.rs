//! Skill slash-command support.
//!
//! Skills can declare `/command` handlers in their frontmatter. This module
//! provides a registry that maps slash commands to skills, enabling users to
//! invoke skill-provided functionality directly from the REPL.

use std::collections::HashMap;

use super::skill_utils::SkillInfo;

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
    use std::path::PathBuf;

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
}
