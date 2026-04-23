//! Skills management tools: list, view, and manage skills

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, SkillProvider, ToolError, ToolHandler, ToolSchema};

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// SkillsListHandler
// ---------------------------------------------------------------------------

/// Tool for listing all available skills.
pub struct SkillsListHandler {
    provider: Arc<dyn SkillProvider>,
}

impl SkillsListHandler {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl ToolHandler for SkillsListHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let skills = self
            .provider
            .list_skills()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let result: Vec<Value> = skills
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "category": s.category,
                    "description": s.description,
                })
            })
            .collect();

        Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| "[]".to_string()))
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "skills_list",
            "List all available skills with their metadata.",
            JsonSchema::new("object"),
        )
    }
}

// ---------------------------------------------------------------------------
// SkillViewHandler
// ---------------------------------------------------------------------------

/// Tool for viewing a specific skill's content.
pub struct SkillViewHandler {
    provider: Arc<dyn SkillProvider>,
    skill_roots: Vec<PathBuf>,
}

impl SkillViewHandler {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self::with_skill_roots(provider, default_skill_roots())
    }

    pub fn with_skill_roots(provider: Arc<dyn SkillProvider>, skill_roots: Vec<PathBuf>) -> Self {
        Self {
            provider,
            skill_roots,
        }
    }
}

#[async_trait]
impl ToolHandler for SkillViewHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;

        if let Some(file_path) = params.get("file_path").and_then(|v| v.as_str()) {
            // Security hardening (parity with Python): reject obvious traversal
            // patterns before resolving any filesystem paths.
            if has_traversal_component(file_path) {
                return Ok(json!({
                    "success": false,
                    "error": "Path traversal ('..') is not allowed.",
                    "hint": "Use a relative path within the skill directory"
                })
                .to_string());
            }

            let Some(skill_dir) = resolve_skill_dir(name, &self.skill_roots) else {
                return Ok(json!({
                    "success": false,
                    "error": format!("Skill '{}' not found.", name),
                    "hint": "Use skills_list to see all available skills"
                })
                .to_string());
            };

            let target_file = skill_dir.join(file_path);
            if let Err(err) = validate_within_skill_dir(&target_file, &skill_dir) {
                return Ok(json!({
                    "success": false,
                    "error": err,
                    "hint": "Use a relative path within the skill directory"
                })
                .to_string());
            }

            if !target_file.exists() {
                return Ok(json!({
                    "success": false,
                    "error": format!("File '{}' not found in skill '{}'.", file_path, name),
                    "available_files": collect_available_skill_files(&skill_dir),
                    "hint": "Use one of the available file paths listed above"
                })
                .to_string());
            }

            let bytes = match std::fs::read(&target_file) {
                Ok(b) => b,
                Err(e) => {
                    return Ok(json!({
                        "success": false,
                        "error": format!("Failed to read '{}': {}", file_path, e)
                    })
                    .to_string())
                }
            };

            let filename = target_file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file");

            return match String::from_utf8(bytes) {
                Ok(content) => Ok(json!({
                    "success": true,
                    "name": name,
                    "file": file_path,
                    "content": content,
                    "file_type": target_file.extension().and_then(|ext| ext.to_str()).map(|ext| format!(".{}", ext)).unwrap_or_default()
                })
                .to_string()),
                Err(e) => Ok(json!({
                    "success": true,
                    "name": name,
                    "file": file_path,
                    "content": format!("[Binary file: {}, size: {} bytes]", filename, e.as_bytes().len()),
                    "is_binary": true
                })
                .to_string()),
            };
        }

        match self.provider.get_skill(name).await {
            Ok(Some(skill)) => Ok(skill.content.clone()),
            Ok(None) => Err(ToolError::NotFound(format!("Skill '{}' not found", name))),
            Err(e) => Err(ToolError::ExecutionFailed(e.to_string())),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Name of the skill to view"
            }),
        );
        props.insert(
            "file_path".into(),
            json!({
                "type": "string",
                "description": "Optional relative path to a specific file within the skill directory (e.g., references/api.md)"
            }),
        );

        tool_schema(
            "skill_view",
            "View a skill by name, or read a specific file inside the skill with file_path.",
            JsonSchema::object(props, vec!["name".into()]),
        )
    }
}

fn default_skill_roots() -> Vec<PathBuf> {
    let mut roots = vec![hermes_config::skills_dir()];
    if let Some(home) = user_home_dir() {
        roots.push(home.join(".hermes").join("skills"));
    }
    roots.sort();
    roots.dedup();
    roots
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(PathBuf::from)
        })
}

fn has_traversal_component(path: &str) -> bool {
    let p = Path::new(path);
    if p.is_absolute() {
        return true;
    }
    p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

fn resolve_skill_dir(name: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    if has_traversal_component(name) {
        return None;
    }
    let name_is_path_like = name.contains('/') || name.contains('\\');
    for root in roots.iter().filter(|r| r.exists()) {
        let direct = root.join(name);
        if direct.is_dir() && direct.join("SKILL.md").exists() {
            return Some(direct);
        }
        if name_is_path_like {
            continue;
        }
        if let Some(found) = find_skill_dir_by_name(root, name) {
            return Some(found);
        }
    }
    None
}

fn find_skill_dir_by_name(root: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name() != Some(OsStr::new("SKILL.md")) {
                continue;
            }
            let Some(parent) = path.parent() else {
                continue;
            };
            let Some(dirname) = parent.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if dirname == name {
                return Some(parent.to_path_buf());
            }
        }
    }
    None
}

fn validate_within_skill_dir(path: &Path, root: &Path) -> Result<(), String> {
    let root_resolved = root
        .canonicalize()
        .map_err(|e| format!("Path escapes skill directory boundary: {}", e))?;

    // Walk up to the nearest existing ancestor so we can still detect
    // symlink escapes for non-existing leaf paths.
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            break;
        }
    }
    if !probe.exists() {
        return Err("Invalid file path".to_string());
    }

    let probe_resolved = probe
        .canonicalize()
        .map_err(|e| format!("Path escapes skill directory boundary: {}", e))?;

    if probe_resolved.strip_prefix(&root_resolved).is_err() {
        return Err("Path escapes skill directory boundary.".to_string());
    }
    Ok(())
}

fn collect_available_skill_files(skill_dir: &Path) -> Value {
    let mut categories: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    categories.insert("references", Vec::new());
    categories.insert("templates", Vec::new());
    categories.insert("assets", Vec::new());
    categories.insert("scripts", Vec::new());
    categories.insert("other", Vec::new());

    let mut stack = vec![skill_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name() == Some(OsStr::new("SKILL.md")) {
                continue;
            }
            let Ok(rel) = path.strip_prefix(skill_dir) else {
                continue;
            };
            let rel_str = normalize_relative_path(rel);
            if rel_str.is_empty() {
                continue;
            }

            if rel_str.starts_with("references/") {
                categories
                    .get_mut("references")
                    .expect("category exists")
                    .push(rel_str);
                continue;
            }
            if rel_str.starts_with("templates/") {
                categories
                    .get_mut("templates")
                    .expect("category exists")
                    .push(rel_str);
                continue;
            }
            if rel_str.starts_with("assets/") {
                categories
                    .get_mut("assets")
                    .expect("category exists")
                    .push(rel_str);
                continue;
            }
            if rel_str.starts_with("scripts/") {
                categories
                    .get_mut("scripts")
                    .expect("category exists")
                    .push(rel_str);
                continue;
            }

            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(
                ext.as_str(),
                "md" | "py" | "yaml" | "yml" | "json" | "tex" | "sh"
            ) {
                categories
                    .get_mut("other")
                    .expect("category exists")
                    .push(rel_str);
            }
        }
    }

    for files in categories.values_mut() {
        files.sort();
    }

    let mut out = serde_json::Map::new();
    for (category, files) in categories {
        if !files.is_empty() {
            out.insert(category.to_string(), json!(files));
        }
    }
    Value::Object(out)
}

fn normalize_relative_path(path: &Path) -> String {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

// ---------------------------------------------------------------------------
// SkillManageHandler
// ---------------------------------------------------------------------------

/// Tool for creating, updating, and deleting skills.
pub struct SkillManageHandler {
    provider: Arc<dyn SkillProvider>,
}

impl SkillManageHandler {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl ToolHandler for SkillManageHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "create" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let content = params.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'content' parameter".into()))?;
                let category = params.get("category").and_then(|v| v.as_str());

                self.provider.create_skill(name, content, category)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' created successfully", name))
            }
            "update" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let content = params.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'content' parameter".into()))?;

                self.provider.update_skill(name, content)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' updated successfully", name))
            }
            "delete" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;

                self.provider.delete_skill(name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' deleted successfully", name))
            }
            "auto_create" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let summary = params.get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Auto-generated from completed task.");
                let steps = params.get("steps")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let mut content = format!("# {}\n\n## Summary\n{}\n\n## Steps\n", name, summary);
                for (idx, s) in steps.iter().enumerate() {
                    content.push_str(&format!("{}. {}\n", idx + 1, s));
                }
                self.provider.create_skill(name, &content, Some("auto-generated"))
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' auto-created", name))
            }
            "self_improve" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let feedback = params.get("feedback")
                    .and_then(|v| v.as_str())
                    .unwrap_or("No feedback provided.");
                let existing = self.provider.get_skill(name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .ok_or_else(|| ToolError::NotFound(format!("Skill '{}' not found", name)))?;
                let improved = format!("{}\n\n## Improvement Feedback\n{}\n", existing.content, feedback);
                self.provider.update_skill(name, &improved)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' improved", name))
            }
            "sync" => Ok("Skill sync request accepted (provider-specific hub sync path).".to_string()),
            "install_builtins" => {
                let builtins = [
                    "planning","debugging","refactoring","testing","docs","git","review","web-research",
                    "terminal","file-edit","security","performance","api-design","db-migrations",
                    "incident-response","release","prompting","agent-orchestration","mcp","gateway",
                    "voice-mode","cron","memory","session-search","tool-authoring","skill-authoring"
                ];
                let mut created = 0usize;
                for name in builtins {
                    let exists = self.provider.get_skill(name)
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                        .is_some();
                    if !exists {
                        let content = format!("# {}\n\n1. Understand\n2. Execute\n3. Verify\n", name);
                        self.provider.create_skill(name, &content, Some("builtin"))
                            .await
                            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                        created += 1;
                    }
                }
                Ok(format!("Installed {} built-in skills", created))
            }
            other => Err(ToolError::InvalidParams(format!("Unknown action: '{}'. Use create/update/delete/auto_create/self_improve/sync/install_builtins.", other))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("action".into(), json!({
            "type": "string",
            "description": "Action to perform",
            "enum": ["create", "update", "delete", "auto_create", "self_improve", "sync", "install_builtins"]
        }));
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Name of the skill"
            }),
        );
        props.insert(
            "content".into(),
            json!({
                "type": "string",
                "description": "Skill content (for create/update)"
            }),
        );
        props.insert(
            "category".into(),
            json!({
                "type": "string",
                "description": "Skill category (for create)"
            }),
        );
        props.insert(
            "summary".into(),
            json!({
                "type": "string",
                "description": "Task summary used by auto_create"
            }),
        );
        props.insert(
            "steps".into(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Step list for auto_create"
            }),
        );
        props.insert(
            "feedback".into(),
            json!({
                "type": "string",
                "description": "Feedback used by self_improve"
            }),
        );

        tool_schema(
            "skill_manage",
            "Create, update, or delete skills.",
            JsonSchema::object(props, vec!["action".into(), "name".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{AgentError, Skill, SkillMeta};
    use std::fs;
    use tempfile::tempdir;

    struct MockSkillProvider;
    #[async_trait]
    impl SkillProvider for MockSkillProvider {
        async fn create_skill(
            &self,
            name: &str,
            content: &str,
            category: Option<&str>,
        ) -> Result<Skill, AgentError> {
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: category.map(String::from),
                description: None,
            })
        }
        async fn get_skill(&self, name: &str) -> Result<Option<Skill>, AgentError> {
            Ok(Some(Skill {
                name: name.into(),
                content: "skill content".into(),
                category: None,
                description: None,
            }))
        }
        async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError> {
            Ok(vec![SkillMeta {
                name: "test".into(),
                category: None,
                description: None,
            }])
        }
        async fn update_skill(&self, name: &str, content: &str) -> Result<Skill, AgentError> {
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: None,
                description: None,
            })
        }
        async fn delete_skill(&self, _name: &str) -> Result<(), AgentError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_skills_list() {
        let handler = SkillsListHandler::new(Arc::new(MockSkillProvider));
        let result = handler.execute(json!({})).await.unwrap();
        assert!(result.contains("test"));
    }

    #[tokio::test]
    async fn test_skill_view() {
        let handler = SkillViewHandler::new(Arc::new(MockSkillProvider));
        let result = handler.execute(json!({"name": "test"})).await.unwrap();
        assert_eq!(result, "skill content");
    }

    #[tokio::test]
    async fn test_skill_view_file_path_reads_reference_file() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let skill_dir = skills_root.join("test-skill");
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "# Test Skill").unwrap();
        fs::write(skill_dir.join("references/api.md"), "API docs here").unwrap();

        let handler =
            SkillViewHandler::with_skill_roots(Arc::new(MockSkillProvider), vec![skills_root]);
        let result = handler
            .execute(json!({"name": "test-skill", "file_path": "references/api.md"}))
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(payload["success"], Value::Bool(true));
        assert_eq!(
            payload["content"],
            Value::String("API docs here".to_string())
        );
    }

    #[tokio::test]
    async fn test_skill_view_file_path_blocks_dotdot_traversal() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let skill_dir = skills_root.join("test-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "# Test Skill").unwrap();
        fs::write(tmp.path().join(".env"), "SECRET_API_KEY=sk-do-not-leak").unwrap();

        let handler =
            SkillViewHandler::with_skill_roots(Arc::new(MockSkillProvider), vec![skills_root]);
        let result = handler
            .execute(json!({"name": "test-skill", "file_path": "../../.env"}))
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(payload["success"], Value::Bool(false));
        let error = payload["error"].as_str().unwrap_or("").to_ascii_lowercase();
        assert!(error.contains("traversal"));
        assert!(!result.contains("sk-do-not-leak"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_skill_view_file_path_blocks_symlink_escape() {
        use std::os::unix::fs as unix_fs;

        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let skill_dir = skills_root.join("test-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "# Test Skill").unwrap();
        let secret = tmp.path().join("secret.txt");
        fs::write(&secret, "TOP SECRET DATA").unwrap();
        unix_fs::symlink(&secret, skill_dir.join("evil-link")).unwrap();

        let handler =
            SkillViewHandler::with_skill_roots(Arc::new(MockSkillProvider), vec![skills_root]);
        let result = handler
            .execute(json!({"name": "test-skill", "file_path": "evil-link"}))
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(payload["success"], Value::Bool(false));
        let error = payload["error"].as_str().unwrap_or("").to_ascii_lowercase();
        assert!(error.contains("escapes") || error.contains("boundary"));
    }

    #[tokio::test]
    async fn test_skill_manage_create() {
        let handler = SkillManageHandler::new(Arc::new(MockSkillProvider));
        let result = handler
            .execute(json!({"action": "create", "name": "new_skill", "content": "hello"}))
            .await
            .unwrap();
        assert!(result.contains("created"));
    }
}
