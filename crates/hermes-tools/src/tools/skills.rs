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
        let legacy = home.join(hermes_config::LEGACY_HOME_DIR).join("skills");
        if legacy.exists() {
            roots.push(legacy);
        }
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

const MAX_NAME_LEN: usize = 64;
const MAX_CONTENT_CHARS: usize = 100_000;
const MAX_FILE_BYTES: usize = 1_048_576;
const ALLOWED_SUBDIRS: &[&str] = &["references", "templates", "scripts", "assets"];

fn valid_name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
}

fn validate_skill_name(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("Skill name is required.".into());
    }
    if name.len() > MAX_NAME_LEN {
        return Some(format!("Skill name exceeds {MAX_NAME_LEN} characters."));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Some(format!(
            "Invalid skill name '{name}'. Must start with a lowercase letter or digit."
        ));
    }
    if !chars.all(valid_name_char) {
        return Some(format!(
            "Invalid skill name '{name}'. Use lowercase letters, numbers, hyphens, dots, and underscores."
        ));
    }
    None
}

fn validate_skill_category(category: &str) -> Option<String> {
    if category.is_empty() {
        return None;
    }
    if category.len() > MAX_NAME_LEN {
        return Some(format!("Category exceeds {MAX_NAME_LEN} characters."));
    }
    if category.contains('/') || category.contains('\\') {
        return Some(format!(
            "Invalid category '{category}'. Categories must be a single directory name."
        ));
    }
    let mut chars = category.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Some(format!(
            "Invalid category '{category}'. Must start with a lowercase letter or digit."
        ));
    }
    if !chars.all(valid_name_char) {
        return Some(format!(
            "Invalid category '{category}'. Use lowercase letters, numbers, hyphens, dots, and underscores."
        ));
    }
    None
}

/// Validate that `content` is a well-formed SKILL.md (frontmatter + non-empty body).
/// Returns an error message or None if valid.
fn validate_frontmatter(content: &str) -> Option<String> {
    if content.trim().is_empty() {
        return Some("Content cannot be empty.".into());
    }
    if !content.starts_with("---") {
        return Some(
            "SKILL.md must start with YAML frontmatter (---). See existing skills for format."
                .into(),
        );
    }
    let rest = &content[3..];
    let Some(end) = rest.find("\n---") else {
        return Some(
            "SKILL.md frontmatter is not closed. Ensure you have a closing '---' line.".into(),
        );
    };
    let yaml_str = &rest[..end];
    let parsed: serde_yaml::Value = match serde_yaml::from_str(yaml_str) {
        Ok(v) => v,
        Err(e) => return Some(format!("YAML frontmatter parse error: {e}")),
    };
    let map = match parsed.as_mapping() {
        Some(m) => m,
        None => return Some("Frontmatter must be a YAML mapping (key: value pairs).".into()),
    };
    if !map.contains_key("name") {
        return Some("Frontmatter must include 'name' field.".into());
    }
    if !map.contains_key("description") {
        return Some("Frontmatter must include 'description' field.".into());
    }
    let body_start = end + 4; // skip "\n---"
    let body = rest[body_start..].trim();
    if body.is_empty() {
        return Some(
            "SKILL.md must have content after the frontmatter (instructions, procedures, etc.)."
                .into(),
        );
    }
    None
}

fn validate_content_size(content: &str, label: &str) -> Option<String> {
    if content.len() > MAX_CONTENT_CHARS {
        return Some(format!(
            "{label} content is {} characters (limit: {MAX_CONTENT_CHARS}). \
             Consider splitting into a smaller SKILL.md with supporting files.",
            content.len()
        ));
    }
    None
}

fn validate_file_path(file_path: &str) -> Option<String> {
    if file_path.is_empty() {
        return Some("file_path is required.".into());
    }
    if has_traversal_component(file_path) {
        return Some("Path traversal ('..') is not allowed.".into());
    }
    let p = Path::new(file_path);
    let first = p.components().next();
    let top = match first {
        Some(Component::Normal(s)) => s.to_string_lossy().into_owned(),
        _ => return Some(format!("Invalid file_path: '{file_path}'")),
    };
    if !ALLOWED_SUBDIRS.contains(&top.as_str()) {
        let allowed = ALLOWED_SUBDIRS.join(", ");
        return Some(format!(
            "File must be under one of: {allowed}. Got: '{file_path}'"
        ));
    }
    if p.components().count() < 2 {
        return Some(format!(
            "Provide a file path, not just a directory. Example: '{top}/myfile.md'"
        ));
    }
    None
}

fn err_json(msg: impl Into<String>) -> String {
    serde_json::to_string(&json!({"success": false, "error": msg.into()}))
        .unwrap_or_else(|_| r#"{"success":false,"error":"serialization error"}"#.to_string())
}

/// Tool for creating, updating, and deleting skills.
pub struct SkillManageHandler {
    provider: Arc<dyn SkillProvider>,
    skill_roots: Vec<PathBuf>,
}

impl SkillManageHandler {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self::with_skill_roots(provider, default_skill_roots())
    }

    pub fn with_skill_roots(provider: Arc<dyn SkillProvider>, skill_roots: Vec<PathBuf>) -> Self {
        Self {
            provider,
            skill_roots,
        }
    }

    fn user_skills_dir(&self) -> PathBuf {
        self.skill_roots
            .first()
            .cloned()
            .unwrap_or_else(|| hermes_config::skills_dir())
    }
}

#[async_trait]
impl ToolHandler for SkillManageHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;

        match action {
            "create" => {
                let content = match params.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => return Ok(err_json("content is required for 'create'.")),
                };
                let category = params.get("category").and_then(|v| v.as_str());

                if let Some(e) = validate_skill_name(name) {
                    return Ok(err_json(e));
                }
                if let Some(cat) = category {
                    if let Some(e) = validate_skill_category(cat) {
                        return Ok(err_json(e));
                    }
                }
                if let Some(e) = validate_frontmatter(content) {
                    return Ok(err_json(e));
                }
                if let Some(e) = validate_content_size(content, "SKILL.md") {
                    return Ok(err_json(e));
                }

                // Check collision across all roots
                if resolve_skill_dir(name, &self.skill_roots).is_some() {
                    return Ok(err_json(format!(
                        "A skill named '{name}' already exists. Use action='edit' to update it."
                    )));
                }

                let skill_dir = match category {
                    Some(cat) => self.user_skills_dir().join(cat).join(name),
                    None => self.user_skills_dir().join(name),
                };
                if let Err(e) = std::fs::create_dir_all(&skill_dir) {
                    return Ok(err_json(format!("Failed to create skill directory: {e}")));
                }
                if let Err(e) = std::fs::write(skill_dir.join("SKILL.md"), content) {
                    return Ok(err_json(format!("Failed to write SKILL.md: {e}")));
                }

                hermes_insights::notify_skill_changed(&skill_dir, hermes_insights::SkillChangeKind::Agent);

                let mut result = json!({
                    "success": true,
                    "message": format!("Skill '{name}' created."),
                    "skill_md": skill_dir.join("SKILL.md").to_string_lossy(),
                });
                if let Some(cat) = category {
                    result["category"] = json!(cat);
                }
                result["hint"] = json!(format!(
                    "To add reference files, use skill_manage(action='write_file', name='{name}', file_path='references/example.md', file_content='...')"
                ));
                Ok(serde_json::to_string(&result).unwrap_or_else(|_| err_json("serialization error")))
            }

            "edit" => {
                let content = match params.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => return Ok(err_json("content is required for 'edit'.")),
                };
                if let Some(e) = validate_frontmatter(content) {
                    return Ok(err_json(e));
                }
                if let Some(e) = validate_content_size(content, "SKILL.md") {
                    return Ok(err_json(e));
                }

                let Some(skill_dir) = resolve_skill_dir(name, &self.skill_roots) else {
                    return Ok(err_json(format!(
                        "Skill '{name}' not found. Use skills_list() to see available skills."
                    )));
                };
                if let Err(e) = std::fs::write(skill_dir.join("SKILL.md"), content) {
                    return Ok(err_json(format!("Failed to write SKILL.md: {e}")));
                }

                hermes_insights::notify_skill_changed(&skill_dir, hermes_insights::SkillChangeKind::Agent);

                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "message": format!("Skill '{name}' updated."),
                    "path": skill_dir.to_string_lossy(),
                }))
                .unwrap_or_else(|_| err_json("serialization error")))
            }

            "patch" => {
                let old_string = match params.get("old_string").and_then(|v| v.as_str()) {
                    Some(s) if !s.is_empty() => s,
                    _ => return Ok(err_json("old_string is required for 'patch'.")),
                };
                let new_string = match params.get("new_string").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => return Ok(err_json("new_string is required for 'patch'. Use empty string to delete matched text.")),
                };
                let replace_all = params
                    .get("replace_all")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let file_path = params.get("file_path").and_then(|v| v.as_str());

                let Some(skill_dir) = resolve_skill_dir(name, &self.skill_roots) else {
                    return Ok(err_json(format!("Skill '{name}' not found.")));
                };

                let target = if let Some(fp) = file_path {
                    if let Some(e) = validate_file_path(fp) {
                        return Ok(err_json(e));
                    }
                    let t = skill_dir.join(fp);
                    if let Err(e) = validate_within_skill_dir(&t, &skill_dir) {
                        return Ok(err_json(e));
                    }
                    t
                } else {
                    skill_dir.join("SKILL.md")
                };

                if !target.exists() {
                    let label = file_path.unwrap_or("SKILL.md");
                    return Ok(err_json(format!("File not found: {label}")));
                }

                let content = match std::fs::read_to_string(&target) {
                    Ok(c) => c,
                    Err(e) => return Ok(err_json(format!("Failed to read file: {e}"))),
                };

                let count = content.matches(old_string).count();
                if count == 0 {
                    let preview = &content[..content.len().min(500)];
                    return Ok(serde_json::to_string(&json!({
                        "success": false,
                        "error": "old_string not found in file.",
                        "file_preview": preview,
                    }))
                    .unwrap_or_else(|_| err_json("serialization error")));
                }
                if count > 1 && !replace_all {
                    return Ok(err_json(format!(
                        "old_string matches {count} occurrences. Use replace_all=true or provide more context to make it unique."
                    )));
                }

                let new_content = if replace_all {
                    content.replace(old_string, new_string)
                } else {
                    content.replacen(old_string, new_string, 1)
                };

                let label = file_path.unwrap_or("SKILL.md");
                if let Some(e) = validate_content_size(&new_content, label) {
                    return Ok(err_json(e));
                }
                if file_path.is_none() {
                    if let Some(e) = validate_frontmatter(&new_content) {
                        return Ok(err_json(format!(
                            "Patch would break SKILL.md structure: {e}"
                        )));
                    }
                }

                if let Err(e) = std::fs::write(&target, &new_content) {
                    return Ok(err_json(format!("Failed to write file: {e}")));
                }

                if file_path.is_none() {
                    hermes_insights::notify_skill_changed(
                        &skill_dir,
                        hermes_insights::SkillChangeKind::Agent,
                    );
                }

                let replacements = if replace_all { count } else { 1 };
                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "message": format!(
                        "Patched {label} in skill '{name}' ({replacements} replacement{}).",
                        if replacements == 1 { "" } else { "s" }
                    ),
                }))
                .unwrap_or_else(|_| err_json("serialization error")))
            }

            "delete" => {
                let absorbed_into = params.get("absorbed_into").and_then(|v| v.as_str());

                if resolve_skill_dir(name, &self.skill_roots).is_none() {
                    return Ok(err_json(format!("Skill '{name}' not found.")));
                }

                self.provider
                    .delete_skill(name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

                let mut message = format!("Skill '{name}' deleted.");
                if let Some(target) = absorbed_into {
                    if !target.is_empty() {
                        message.push_str(&format!(" Content absorbed into '{target}'."));
                    }
                }
                Ok(serde_json::to_string(&json!({"success": true, "message": message}))
                    .unwrap_or_else(|_| err_json("serialization error")))
            }

            "write_file" => {
                let file_path = match params.get("file_path").and_then(|v| v.as_str()) {
                    Some(fp) => fp,
                    None => return Ok(err_json("file_path is required for 'write_file'. Example: 'references/api-guide.md'")),
                };
                let file_content = match params.get("file_content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => return Ok(err_json("file_content is required for 'write_file'.")),
                };

                if let Some(e) = validate_file_path(file_path) {
                    return Ok(err_json(e));
                }
                if file_content.len() > MAX_FILE_BYTES {
                    return Ok(err_json(format!(
                        "File content is {} bytes (limit: {MAX_FILE_BYTES} bytes / 1 MiB).",
                        file_content.len()
                    )));
                }
                if let Some(e) = validate_content_size(file_content, file_path) {
                    return Ok(err_json(e));
                }

                let Some(skill_dir) = resolve_skill_dir(name, &self.skill_roots) else {
                    return Ok(err_json(format!(
                        "Skill '{name}' not found. Create it first with action='create'."
                    )));
                };

                let target = skill_dir.join(file_path);
                if let Err(e) = validate_within_skill_dir(&target, &skill_dir) {
                    return Ok(err_json(e));
                }
                if let Some(parent) = target.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return Ok(err_json(format!("Failed to create directory: {e}")));
                    }
                }
                if let Err(e) = std::fs::write(&target, file_content) {
                    return Ok(err_json(format!("Failed to write file: {e}")));
                }

                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "message": format!("File '{file_path}' written to skill '{name}'."),
                    "path": target.to_string_lossy(),
                }))
                .unwrap_or_else(|_| err_json("serialization error")))
            }

            "remove_file" => {
                let file_path = match params.get("file_path").and_then(|v| v.as_str()) {
                    Some(fp) => fp,
                    None => return Ok(err_json("file_path is required for 'remove_file'.")),
                };

                if let Some(e) = validate_file_path(file_path) {
                    return Ok(err_json(e));
                }

                let Some(skill_dir) = resolve_skill_dir(name, &self.skill_roots) else {
                    return Ok(err_json(format!("Skill '{name}' not found.")));
                };

                let target = skill_dir.join(file_path);
                if let Err(e) = validate_within_skill_dir(&target, &skill_dir) {
                    return Ok(err_json(e));
                }
                if !target.exists() {
                    let available = collect_available_skill_files(&skill_dir);
                    return Ok(serde_json::to_string(&json!({
                        "success": false,
                        "error": format!("File '{file_path}' not found in skill '{name}'."),
                        "available_files": available,
                    }))
                    .unwrap_or_else(|_| err_json("serialization error")));
                }

                if let Err(e) = std::fs::remove_file(&target) {
                    return Ok(err_json(format!("Failed to remove file: {e}")));
                }

                // Clean up empty subdirectory
                if let Some(parent) = target.parent() {
                    if parent != skill_dir && parent.exists() {
                        let _ = std::fs::remove_dir(parent);
                    }
                }

                Ok(serde_json::to_string(&json!({
                    "success": true,
                    "message": format!("File '{file_path}' removed from skill '{name}'."),
                }))
                .unwrap_or_else(|_| err_json("serialization error")))
            }

            other => Ok(err_json(format!(
                "Unknown action '{other}'. Use: create, edit, patch, delete, write_file, remove_file"
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["create", "edit", "patch", "delete", "write_file", "remove_file"],
                "description": "The action to perform."
            }),
        );
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Skill name (lowercase, hyphens/underscores, max 64 chars). Must match an existing skill for patch/edit/delete/write_file/remove_file."
            }),
        );
        props.insert(
            "content".into(),
            json!({
                "type": "string",
                "description": "Full SKILL.md content (YAML frontmatter + markdown body). Required for 'create' and 'edit'."
            }),
        );
        props.insert(
            "old_string".into(),
            json!({
                "type": "string",
                "description": "Text to find in the file (required for 'patch'). Must be unique unless replace_all=true."
            }),
        );
        props.insert(
            "new_string".into(),
            json!({
                "type": "string",
                "description": "Replacement text (required for 'patch'). Can be empty string to delete the matched text."
            }),
        );
        props.insert(
            "replace_all".into(),
            json!({
                "type": "boolean",
                "description": "For 'patch': replace all occurrences instead of requiring a unique match (default: false)."
            }),
        );
        props.insert(
            "category".into(),
            json!({
                "type": "string",
                "description": "Optional category for organizing the skill (e.g., 'devops'). Only used with 'create'."
            }),
        );
        props.insert(
            "file_path".into(),
            json!({
                "type": "string",
                "description": "Path to a supporting file within the skill directory. For 'write_file'/'remove_file': required, must be under references/, templates/, scripts/, or assets/. For 'patch': optional, defaults to SKILL.md if omitted."
            }),
        );
        props.insert(
            "file_content".into(),
            json!({
                "type": "string",
                "description": "Content for the file. Required for 'write_file'."
            }),
        );
        props.insert(
            "absorbed_into".into(),
            json!({
                "type": "string",
                "description": "For 'delete' only: pass the umbrella skill name when this skill's content was merged into another, or empty string when pruning with no forwarding target."
            }),
        );

        tool_schema(
            "skill_manage",
            "Manage skills (create, edit, patch, delete, write_file, remove_file). \
             Skills are procedural memory — reusable approaches for recurring task types. \
             New skills are created in ~/.hermes-agent-ultra/skills/; existing skills can be modified wherever they live.",
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
    async fn test_skill_manage_create_invalid_no_frontmatter() {
        let handler = SkillManageHandler::new(Arc::new(MockSkillProvider));
        let result = handler
            .execute(json!({"action": "create", "name": "new-skill", "content": "no frontmatter here"}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["success"], Value::Bool(false));
        assert!(v["error"].as_str().unwrap().contains("frontmatter"));
    }

    #[tokio::test]
    async fn test_skill_manage_create_invalid_name() {
        let handler = SkillManageHandler::new(Arc::new(MockSkillProvider));
        let result = handler
            .execute(json!({"action": "create", "name": "Bad Name!", "content": "---\nname: x\ndescription: y\n---\nbody"}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["success"], Value::Bool(false));
        assert!(v["error"].as_str().unwrap().to_ascii_lowercase().contains("invalid skill name"));
    }

    #[tokio::test]
    async fn test_skill_manage_unknown_action() {
        let handler = SkillManageHandler::new(Arc::new(MockSkillProvider));
        let result = handler
            .execute(json!({"action": "auto_create", "name": "foo"}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["success"], Value::Bool(false));
        assert!(v["error"].as_str().unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn test_skill_manage_patch_not_found() {
        let handler = SkillManageHandler::new(Arc::new(MockSkillProvider));
        let result = handler
            .execute(json!({
                "action": "patch",
                "name": "nonexistent-skill",
                "old_string": "foo",
                "new_string": "bar"
            }))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["success"], Value::Bool(false));
    }

    #[tokio::test]
    async fn test_skill_manage_create_and_patch() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        fs::create_dir_all(&skills_root).unwrap();

        let handler = SkillManageHandler::with_skill_roots(
            Arc::new(MockSkillProvider),
            vec![skills_root.clone()],
        );

        let content = "---\nname: test-skill\ndescription: A test skill\n---\n\nDo the thing.\n";
        let result = handler
            .execute(json!({"action": "create", "name": "test-skill", "content": content}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["success"], Value::Bool(true), "create failed: {result}");

        // patch it
        let result = handler
            .execute(json!({
                "action": "patch",
                "name": "test-skill",
                "old_string": "Do the thing.",
                "new_string": "Do the other thing."
            }))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["success"], Value::Bool(true), "patch failed: {result}");

        let patched = fs::read_to_string(skills_root.join("test-skill/SKILL.md")).unwrap();
        assert!(patched.contains("Do the other thing."));
    }

    #[tokio::test]
    async fn test_skill_manage_write_and_remove_file() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let skill_dir = skills_root.join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: test\n---\n\nbody\n",
        )
        .unwrap();

        let handler = SkillManageHandler::with_skill_roots(
            Arc::new(MockSkillProvider),
            vec![skills_root.clone()],
        );

        let result = handler
            .execute(json!({
                "action": "write_file",
                "name": "my-skill",
                "file_path": "references/api.md",
                "file_content": "# API docs"
            }))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["success"], Value::Bool(true), "write_file failed: {result}");
        assert!(skill_dir.join("references/api.md").exists());

        let result = handler
            .execute(json!({
                "action": "remove_file",
                "name": "my-skill",
                "file_path": "references/api.md"
            }))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["success"], Value::Bool(true), "remove_file failed: {result}");
        assert!(!skill_dir.join("references/api.md").exists());
    }

    #[tokio::test]
    async fn test_skill_manage_write_file_traversal_blocked() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let skill_dir = skills_root.join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: test\n---\n\nbody\n",
        )
        .unwrap();

        let handler = SkillManageHandler::with_skill_roots(
            Arc::new(MockSkillProvider),
            vec![skills_root],
        );
        let result = handler
            .execute(json!({
                "action": "write_file",
                "name": "my-skill",
                "file_path": "../../evil.sh",
                "file_content": "rm -rf /"
            }))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["success"], Value::Bool(false));
        assert!(v["error"].as_str().unwrap().to_ascii_lowercase().contains("traversal") || v["error"].as_str().unwrap().contains("allowed"));
    }
}
