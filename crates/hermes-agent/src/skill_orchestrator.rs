//! Skill orchestrator and slash command system.
//!
//! Scans `~/.hermes/skills/` for SKILL.md files, builds a `/command` → skill
//! mapping, and provides helpers for invoking skills via slash commands and
//! preloading skills for session-wide use.
//!
//! Corresponds to Python `agent/skill_commands.py`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use hermes_core::types::Skill;
use hermes_skills::SkillGuard;
use regex::Regex;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Information about a registered skill command.
#[derive(Debug, Clone)]
pub struct SkillCommandInfo {
    /// Original skill name from frontmatter.
    pub name: String,
    /// Short description.
    pub description: String,
    /// Path to the SKILL.md file.
    pub skill_md_path: PathBuf,
    /// Directory containing the skill.
    pub skill_dir: PathBuf,
}

/// Parsed SKILL.md frontmatter.
#[derive(Debug, Clone, Default)]
pub struct SkillFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub platform: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Frontmatter parsing
// ---------------------------------------------------------------------------

/// Parse YAML frontmatter from a SKILL.md file.
///
/// Expects the file to start with `---\n...\n---\n` followed by the body.
/// Returns (frontmatter, body).
pub fn parse_frontmatter(content: &str) -> (SkillFrontmatter, &str) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (SkillFrontmatter::default(), content);
    }

    // Find the closing ---
    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);

    if let Some(end_idx) = after_first.find("\n---") {
        let yaml_block = &after_first[..end_idx];
        let body_start = end_idx + 4; // skip \n---
        let body = after_first[body_start..].trim_start_matches(['\r', '\n']);

        let fm = parse_yaml_frontmatter(yaml_block);
        (fm, body)
    } else {
        (SkillFrontmatter::default(), content)
    }
}

/// Minimal YAML frontmatter parser (no full YAML dependency needed).
fn parse_yaml_frontmatter(yaml: &str) -> SkillFrontmatter {
    let mut fm = SkillFrontmatter::default();

    for line in yaml.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'');
            match key {
                "name" => fm.name = Some(value.to_string()),
                "description" => fm.description = Some(value.to_string()),
                _ => {}
            }
        }
    }

    fm
}

// ---------------------------------------------------------------------------
// Slug normalization
// ---------------------------------------------------------------------------

lazy_static::lazy_static! {
    static ref SKILL_INVALID_CHARS: Regex = Regex::new(r"[^a-z0-9-]").unwrap();
    static ref SKILL_MULTI_HYPHEN: Regex = Regex::new(r"-{2,}").unwrap();
}

/// Normalize a skill name into a clean hyphen-separated slug.
fn slugify_skill_name(name: &str) -> String {
    let lower = name.to_lowercase().replace([' ', '_'], "-");
    let cleaned = SKILL_INVALID_CHARS.replace_all(&lower, "");
    let deduped = SKILL_MULTI_HYPHEN.replace_all(&cleaned, "-");
    deduped.trim_matches('-').to_string()
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
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// SkillOrchestrator
// ---------------------------------------------------------------------------

/// Manages skill discovery, slash command resolution, and skill invocation.
pub struct SkillOrchestrator {
    /// Mapping from `/command-name` to skill info.
    commands: HashMap<String, SkillCommandInfo>,
    /// Base skills directory (typically `~/.hermes/skills/`).
    skills_dir: PathBuf,
}

impl SkillOrchestrator {
    /// Create a new orchestrator with the given skills directory.
    pub fn new(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            commands: HashMap::new(),
            skills_dir: skills_dir.into(),
        }
    }

    /// Create an orchestrator using the default `~/.hermes/skills/` directory.
    pub fn default_dir() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::new(home.join(".hermes").join("skills"))
    }

    /// Scan the skills directory and build the command mapping.
    ///
    /// Returns a reference to the updated commands map.
    pub fn scan_skill_commands(&mut self) -> &HashMap<String, SkillCommandInfo> {
        self.commands.clear();

        if !self.skills_dir.exists() {
            return &self.commands;
        }

        let mut seen_names = std::collections::HashSet::new();

        self.scan_directory(&self.skills_dir.clone(), &mut seen_names);

        &self.commands
    }

    /// Recursively scan a directory for SKILL.md files.
    fn scan_directory(&mut self, dir: &Path, seen_names: &mut std::collections::HashSet<String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();

            // Skip hidden directories
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }

            if path.is_dir() {
                // Check for SKILL.md in this directory
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    self.register_skill(&skill_md, seen_names);
                }
                // Also recurse into subdirectories
                self.scan_directory(&path, seen_names);
            }
        }
    }

    /// Register a single SKILL.md file.
    fn register_skill(
        &mut self,
        skill_md_path: &Path,
        seen_names: &mut std::collections::HashSet<String>,
    ) {
        let content = match std::fs::read_to_string(skill_md_path) {
            Ok(c) => c,
            Err(_) => return,
        };

        let (frontmatter, body) = parse_frontmatter(&content);

        let name = frontmatter.name.unwrap_or_else(|| {
            skill_md_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
        });

        if seen_names.contains(&name) {
            return;
        }
        seen_names.insert(name.clone());

        let description = frontmatter.description.unwrap_or_else(|| {
            // Use first non-heading, non-empty line from body
            body.lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .map(|l| l.chars().take(80).collect())
                .unwrap_or_else(|| format!("Invoke the {} skill", name))
        });

        let cmd_name = slugify_skill_name(&name);
        if cmd_name.is_empty() {
            return;
        }
        if let Err(err) = security_gate_skill_content(&name, body) {
            tracing::warn!("Skipping skill '{}' due to security policy: {}", name, err);
            return;
        }

        let skill_dir = skill_md_path
            .parent()
            .unwrap_or(skill_md_path)
            .to_path_buf();

        self.commands.insert(
            format!("/{cmd_name}"),
            SkillCommandInfo {
                name,
                description,
                skill_md_path: skill_md_path.to_path_buf(),
                skill_dir,
            },
        );
    }

    /// Get the current commands mapping.
    pub fn get_commands(&self) -> &HashMap<String, SkillCommandInfo> {
        &self.commands
    }

    /// Resolve a user-typed `/command` to its canonical key.
    ///
    /// Hyphens and underscores are treated interchangeably.
    pub fn resolve_skill_command_key(&self, command: &str) -> Option<String> {
        if command.is_empty() {
            return None;
        }
        let normalized = format!("/{}", command.trim_start_matches('/').replace('_', "-"));
        if self.commands.contains_key(&normalized) {
            Some(normalized)
        } else {
            None
        }
    }

    /// Build the user message content for a skill slash command invocation.
    pub fn build_skill_invocation_message(
        &self,
        cmd_key: &str,
        user_instruction: &str,
    ) -> Option<String> {
        let info = self.commands.get(cmd_key)?;

        let content = match std::fs::read_to_string(&info.skill_md_path) {
            Ok(c) => c,
            Err(_) => return Some(format!("[Failed to load skill: {}]", info.name)),
        };

        let (_, body) = parse_frontmatter(&content);
        if let Err(err) = security_gate_skill_content(&info.name, body) {
            return Some(format!(
                "[Blocked skill '{}' by security policy: {}]",
                info.name, err
            ));
        }

        let mut parts = vec![
            format!(
                "[SYSTEM: The user has invoked the \"{}\" skill, indicating they want \
                 you to follow its instructions. The full skill content is loaded below.]",
                info.name
            ),
            String::new(),
            body.trim().to_string(),
        ];

        if !user_instruction.is_empty() {
            parts.push(String::new());
            parts.push(format!(
                "The user has provided the following instruction alongside the skill invocation: {}",
                user_instruction
            ));
        }

        Some(parts.join("\n"))
    }

    /// Load multiple skills for session-wide preloading.
    ///
    /// Returns (prompt_text, loaded_skill_names, missing_identifiers).
    pub fn build_preloaded_skills_prompt(
        &self,
        identifiers: &[String],
    ) -> (String, Vec<String>, Vec<String>) {
        let mut prompt_parts = Vec::new();
        let mut loaded_names = Vec::new();
        let mut missing = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for identifier in identifiers {
            let identifier = identifier.trim();
            if identifier.is_empty() || seen.contains(identifier) {
                continue;
            }
            seen.insert(identifier.to_string());

            // Try to resolve as a command key
            let cmd_key = self.resolve_skill_command_key(identifier);
            let info = cmd_key.as_ref().and_then(|k| self.commands.get(k));

            match info {
                Some(info) => {
                    let content = match std::fs::read_to_string(&info.skill_md_path) {
                        Ok(c) => c,
                        Err(_) => {
                            missing.push(identifier.to_string());
                            continue;
                        }
                    };
                    let (_, body) = parse_frontmatter(&content);
                    if let Err(err) = security_gate_skill_content(&info.name, body) {
                        tracing::warn!(
                            "Skipping preloaded skill '{}' due to security policy: {}",
                            info.name,
                            err
                        );
                        missing.push(identifier.to_string());
                        continue;
                    }

                    let activation = format!(
                        "[SYSTEM: The user launched this CLI session with the \"{}\" skill \
                         preloaded. Treat its instructions as active guidance for the duration \
                         of this session unless the user overrides them.]",
                        info.name
                    );
                    prompt_parts.push(format!("{}\n\n{}", activation, body.trim()));
                    loaded_names.push(info.name.clone());
                }
                None => {
                    missing.push(identifier.to_string());
                }
            }
        }

        (prompt_parts.join("\n\n"), loaded_names, missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_parse_frontmatter_with_yaml() {
        let content = "---\nname: my-skill\ndescription: A test skill\n---\n# Body\nHello world";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.name.as_deref(), Some("my-skill"));
        assert_eq!(fm.description.as_deref(), Some("A test skill"));
        assert!(body.contains("Hello world"));
    }

    #[test]
    fn test_parse_frontmatter_without_yaml() {
        let content = "# Just a body\nNo frontmatter here.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.name.is_none());
        assert!(body.contains("Just a body"));
    }

    #[test]
    fn test_slugify_skill_name() {
        assert_eq!(slugify_skill_name("My Cool Skill"), "my-cool-skill");
        assert_eq!(slugify_skill_name("claude_code"), "claude-code");
        assert_eq!(slugify_skill_name("gif+search"), "gifsearch");
        assert_eq!(slugify_skill_name("---test---"), "test");
    }

    #[test]
    fn test_resolve_skill_command_key() {
        let mut orch = SkillOrchestrator::new("/tmp/nonexistent");
        orch.commands.insert(
            "/my-skill".to_string(),
            SkillCommandInfo {
                name: "my-skill".to_string(),
                description: "test".to_string(),
                skill_md_path: PathBuf::from("/tmp/skill/SKILL.md"),
                skill_dir: PathBuf::from("/tmp/skill"),
            },
        );

        // Exact match
        assert_eq!(
            orch.resolve_skill_command_key("my-skill"),
            Some("/my-skill".to_string())
        );
        // Underscore → hyphen normalization
        assert_eq!(
            orch.resolve_skill_command_key("my_skill"),
            Some("/my-skill".to_string())
        );
        // With leading slash
        assert_eq!(
            orch.resolve_skill_command_key("/my-skill"),
            Some("/my-skill".to_string())
        );
        // Non-existent
        assert!(orch.resolve_skill_command_key("nonexistent").is_none());
        // Empty
        assert!(orch.resolve_skill_command_key("").is_none());
    }

    #[test]
    fn test_scan_skill_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("test-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: test-skill\ndescription: A test\n---\n# Test\nDo the thing.",
        )
        .unwrap();

        let mut orch = SkillOrchestrator::new(tmp.path());
        orch.scan_skill_commands();

        assert!(orch.commands.contains_key("/test-skill"));
        let info = &orch.commands["/test-skill"];
        assert_eq!(info.name, "test-skill");
        assert_eq!(info.description, "A test");
    }

    #[test]
    fn test_build_skill_invocation_message() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("greet");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: greet\ndescription: Greeting skill\n---\nSay hello to the user.",
        )
        .unwrap();

        let mut orch = SkillOrchestrator::new(tmp.path());
        orch.scan_skill_commands();

        let msg = orch
            .build_skill_invocation_message("/greet", "in French")
            .unwrap();
        assert!(msg.contains("greet"));
        assert!(msg.contains("Say hello to the user."));
        assert!(msg.contains("in French"));
    }

    #[test]
    fn test_build_preloaded_skills_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("coder");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: coder\ndescription: Coding assistant\n---\nWrite clean code.",
        )
        .unwrap();

        let mut orch = SkillOrchestrator::new(tmp.path());
        orch.scan_skill_commands();

        let (prompt, loaded, missing) =
            orch.build_preloaded_skills_prompt(&["coder".to_string(), "nonexistent".to_string()]);

        assert_eq!(loaded, vec!["coder"]);
        assert_eq!(missing, vec!["nonexistent"]);
        assert!(prompt.contains("Write clean code."));
        assert!(prompt.contains("preloaded"));
    }
}
