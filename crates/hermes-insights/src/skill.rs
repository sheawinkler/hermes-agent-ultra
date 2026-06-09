//! Extract de-identified skill patterns from local SKILL.md files.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use regex::Regex;
use std::sync::LazyLock;

use hermes_skills::read_hub_lock;

use crate::sanitize::{contains_residual_pii, sanitize_text, slugify_name};
use crate::types::{
    SkillProvenance, SkillReferenceSnippet, SkillStructure, SkillTriggerHints,
    WorkPackageSkillPayload, sha256_hex,
};

static HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^#{1,6}\s+(.+)$").unwrap());
static ORDERED_STEP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*\d+\.\s+").unwrap());

const MAX_REFERENCE_FILES: usize = 24;
const MAX_REFERENCE_BYTES: usize = 8 * 1024;

/// Subdirectories allowed under a skill root (aligned with `skill_manage` / authoring docs).
const ALLOWED_SKILL_SUBDIRS: &[&str] = &["references", "templates", "scripts", "assets"];

/// Text-like extensions uploaded after sanitization; binary assets (images, fonts) are skipped.
const UPLOADABLE_TEXT_EXTENSIONS: &[&str] =
    &["md", "txt", "markdown", "py", "yaml", "yml", "json", "tex", "sh"];

#[derive(Debug, Clone, Copy)]
pub enum SkillChangeKind {
    Agent,
    User,
}

#[derive(Clone)]
pub struct SkillPatternOptions {
    pub include_body: bool,
    pub from_background_review: bool,
    pub domain_keys: Vec<String>,
    pub binding_role: String,
    pub provenance: SkillProvenance,
}

impl SkillPatternOptions {
    pub fn default_for_work_package() -> Self {
        Self {
            include_body: true,
            from_background_review: false,
            domain_keys: Vec::new(),
            binding_role: "primary".to_string(),
            provenance: SkillProvenance::AgentCreated,
        }
    }

    pub fn from_change_kind(kind: SkillChangeKind) -> Self {
        Self {
            include_body: true,
            from_background_review: false,
            domain_keys: Vec::new(),
            binding_role: "primary".to_string(),
            provenance: match kind {
                SkillChangeKind::Agent => SkillProvenance::AgentCreated,
                SkillChangeKind::User => SkillProvenance::UserCreated,
            },
        }
    }
}

/// Returns None if skill is hub/bundled, guard-high, or fails sanitization.
pub fn build_work_package_skill(
    skill_dir: &Path,
    skills_root: &Path,
    options: &SkillPatternOptions,
) -> Option<WorkPackageSkillPayload> {
    if is_hub_or_bundled_skill(skill_dir, skills_root) {
        return None;
    }
    let skill_md = skill_dir.join("SKILL.md");
    let content = std::fs::read_to_string(&skill_md).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    if run_guard_high_severity(&content) {
        return None;
    }
    let (name, _category, description) = parse_frontmatter(&content);
    let display_name = sanitize_text(&name);
    if display_name.is_empty() || contains_residual_pii(&display_name) {
        return None;
    }
    let name_slug = slugify_name(&name);
    let description_redacted = sanitize_text(&description);
    if contains_residual_pii(&description_redacted) {
        return None;
    }
    let domain_keys = if options.domain_keys.is_empty() {
        vec![format!("topic:{name_slug}")]
    } else {
        options.domain_keys.clone()
    };
    let body = body_for_contribution(&content);
    let references_redacted = collect_skill_auxiliary_files(skill_dir);
    let structure = extract_structure(&body);
    let tool_chain = extract_tool_chain(&body);
    let content_version = sha256_hex(content.as_bytes());
    let pattern_id = sha256_hex(
        format!(
            "{name_slug}|{}|{}|{}",
            tool_chain.join(","),
            structure.step_count,
            structure.headings.join("|")
        )
        .as_bytes(),
    );
    let redacted_body = if options.include_body {
        let body_redacted = sanitize_text(&body);
        if body_redacted.is_empty() || contains_residual_pii(&body_redacted) {
            return None;
        }
        Some(body_redacted)
    } else {
        None
    };
    Some(WorkPackageSkillPayload {
        pattern_id,
        display_name,
        name_slug: name_slug.clone(),
        binding_role: options.binding_role.clone(),
        domain_keys,
        description_redacted,
        structure,
        tool_chain,
        trigger_hints: SkillTriggerHints {
            slash_command: Some(name_slug),
            from_background_review: options.from_background_review,
        },
        provenance: options.provenance,
        content_version,
        redacted_body,
        references_redacted,
    })
}

/// Locate a local skill directory by sanitized frontmatter `name` slug.
pub fn find_skill_dir_by_slug(skills_root: &Path, name_slug: &str) -> Option<PathBuf> {
    let mut found = None;
    walk_skill_dirs(skills_root, &mut |skill_dir| {
        if found.is_some() {
            return;
        }
        let Ok(content) = std::fs::read_to_string(skill_dir.join("SKILL.md")) else {
            return;
        };
        let (name, _, _) = parse_frontmatter(&content);
        if slugify_name(&name) == name_slug {
            found = Some(skill_dir.to_path_buf());
        }
    });
    found
}

/// Rebuild upload options from an existing outbox work-package skill payload.
pub fn skill_options_from_work_package_payload(
    payload: &serde_json::Value,
    include_body: bool,
) -> SkillPatternOptions {
    let domain_keys = payload
        .get("domain_keys")
        .or_else(|| payload.get("skill").and_then(|s| s.get("domain_keys")))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let binding_role = payload
        .get("binding_role")
        .or_else(|| payload.get("skill").and_then(|s| s.get("binding_role")))
        .and_then(|v| v.as_str())
        .unwrap_or("primary")
        .to_string();
    let provenance = match payload
        .get("provenance")
        .or_else(|| payload.get("skill").and_then(|s| s.get("provenance")))
        .and_then(|v| v.as_str())
    {
        Some("user_created") => SkillProvenance::UserCreated,
        _ => SkillProvenance::AgentCreated,
    };
    let from_background_review = payload
        .get("trigger_hints")
        .or_else(|| payload.get("skill").and_then(|s| s.get("trigger_hints")))
        .and_then(|h| h.get("from_background_review"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    SkillPatternOptions {
        include_body,
        from_background_review,
        domain_keys,
        binding_role,
        provenance,
    }
}

/// Walk each skill directory at most once (canonical path + `name_slug`).
pub fn walk_unique_skill_dirs<F>(skills_root: &Path, mut f: F)
where
    F: FnMut(&Path),
{
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let mut seen_slug: HashSet<String> = HashSet::new();
    walk_skill_dirs(skills_root, &mut |skill_dir| {
        let dir_key = skill_dir
            .canonicalize()
            .unwrap_or_else(|_| skill_dir.to_path_buf());
        if !seen_dirs.insert(dir_key) {
            return;
        }
        if let Ok(content) = std::fs::read_to_string(skill_dir.join("SKILL.md")) {
            let (name, _, _) = parse_frontmatter(&content);
            let slug = slugify_name(&name);
            if !seen_slug.insert(slug) {
                return;
            }
        }
        f(skill_dir);
    });
}

fn collect_skill_auxiliary_files(skill_dir: &Path) -> Vec<SkillReferenceSnippet> {
    let mut paths = Vec::new();
    for subdir in ALLOWED_SKILL_SUBDIRS {
        let root = skill_dir.join(subdir);
        if root.is_dir() {
            collect_uploadable_files_recursive(&root, &mut paths);
        }
    }
    paths.sort();

    let mut snippets = Vec::new();
    for path in paths {
        if snippets.len() >= MAX_REFERENCE_FILES {
            break;
        }
        let Some(rel) = auxiliary_relative_path(skill_dir, &path) else {
            continue;
        };
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if raw.len() > MAX_REFERENCE_BYTES * 2 {
            continue;
        }
        let content_redacted = sanitize_text(&raw);
        if content_redacted.is_empty() || contains_residual_pii(&content_redacted) {
            continue;
        }
        if content_redacted.len() > MAX_REFERENCE_BYTES {
            continue;
        }
        snippets.push(SkillReferenceSnippet {
            relative_path: rel,
            content_redacted,
        });
    }
    snippets
}

fn collect_uploadable_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path.is_file() {
            out.push(path);
        }
    }
    subdirs.sort();
    for subdir in subdirs {
        collect_uploadable_files_recursive(&subdir, out);
    }
}

fn auxiliary_relative_path(skill_dir: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(skill_dir).ok()?;
    let rel_str = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if rel_str.is_empty() {
        return None;
    }
    let top = rel_str.split('/').next()?;
    if !ALLOWED_SKILL_SUBDIRS.contains(&top) {
        return None;
    }
    let file_name = path.file_name()?.to_str()?;
    if file_name.starts_with('.') {
        return None;
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !UPLOADABLE_TEXT_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    Some(rel_str)
}

fn is_hub_or_bundled_skill(skill_dir: &Path, skills_root: &Path) -> bool {
    let lock = read_hub_lock(skills_root);
    let skill_name = skill_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    lock.installed.iter().any(|e| {
        e.name.to_ascii_lowercase() == skill_name || PathBuf::from(&e.install_path) == skill_dir
    })
}

fn run_guard_high_severity(content: &str) -> bool {
    let findings = hermes_skills::scan_content("SKILL.md", content);
    findings
        .iter()
        .any(|f| f.severity.eq_ignore_ascii_case("high"))
}

pub fn parse_frontmatter_for_slug(content: &str) -> (String, Option<String>, String) {
    parse_frontmatter(content)
}

fn parse_frontmatter(content: &str) -> (String, Option<String>, String) {
    let mut name = String::new();
    let mut category = None;
    let mut description = String::new();
    if content.starts_with("---") {
        if let Some(end) = content[3..].find("\n---") {
            let fm = &content[3..3 + end];
            for line in fm.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    match k.trim() {
                        "name" => name = v.trim().trim_matches('"').to_string(),
                        "category" => {
                            category = Some(v.trim().trim_matches('"').to_string());
                        }
                        "description" => description = v.trim().trim_matches('"').to_string(),
                        _ => {}
                    }
                }
            }
        }
    }
    if name.is_empty() {
        name = "skill".to_string();
    }
    (name, category, description)
}

fn body_after_frontmatter(content: &str) -> String {
    if content.starts_with("---") {
        if let Some(end) = content[3..].find("\n---") {
            let rest = &content[3 + end + 4..];
            return rest.to_string();
        }
    }
    content.to_string()
}

fn body_for_contribution(content: &str) -> String {
    strip_references_section(&body_after_frontmatter(content))
}

/// Remove the References *index section* from SKILL.md body; file contents go in `references_redacted`.
fn strip_references_section(body: &str) -> String {
    let lower = body.to_ascii_lowercase();
    let cut = lower
        .find("\n## references")
        .or_else(|| lower.find("\n# references"))
        .unwrap_or(body.len());
    body[..cut].to_string()
}

fn extract_structure(body: &str) -> SkillStructure {
    let headings: Vec<String> = HEADING_RE
        .captures_iter(body)
        .filter_map(|c| c.get(1).map(|m| sanitize_text(m.as_str())))
        .take(20)
        .collect();
    let step_count = ORDERED_STEP_RE.find_iter(body).count() as u32;
    let lower = body.to_ascii_lowercase();
    SkillStructure {
        headings,
        step_count,
        mentions_subagent: lower.contains("delegate_task") || lower.contains("sub-agent"),
        mentions_cron: lower.contains("cron") || lower.contains("/cron"),
        mentions_mcp: lower.contains("mcp"),
    }
}

fn extract_tool_chain(body: &str) -> Vec<String> {
    const TOOLS: &[&str] = &[
        "skill_manage",
        "skills_list",
        "skill_view",
        "terminal",
        "web_search",
        "write_file",
        "read_file",
        "patch",
        "delegate_task",
        "contextlattice_search",
        "contextlattice_write",
    ];
    let lower = body.to_ascii_lowercase();
    let mut chain = Vec::new();
    for tool in TOOLS {
        if lower.contains(tool) {
            chain.push((*tool).to_string());
        }
    }
    chain
}

pub fn walk_skill_dirs<F>(root: &Path, f: &mut F)
where
    F: FnMut(&Path),
{
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().and_then(|s| s.to_str()) == Some(".hub") {
            continue;
        }
        if path.join("SKILL.md").is_file() {
            f(&path);
            continue;
        }
        if let Ok(sub) = std::fs::read_dir(&path) {
            for sub_entry in sub.filter_map(|e| e.ok()) {
                let sub_path = sub_entry.path();
                if sub_path.is_dir() && sub_path.join("SKILL.md").is_file() {
                    f(&sub_path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn builds_pattern_from_skill_md() {
        let tmp = TempDir::new().unwrap();
        let skills_root = tmp.path().join("skills");
        let skill_dir = skills_root.join("demo-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: Run parity tests\n---\n\
             ## Steps\n1. Run cargo test\n2. Use skill_manage\n",
        )
        .unwrap();
        let opts = SkillPatternOptions::from_change_kind(SkillChangeKind::Agent);
        let pattern = build_work_package_skill(&skill_dir, &skills_root, &opts).unwrap();
        assert_eq!(pattern.display_name, "demo-skill");
        assert!(pattern.redacted_body.is_some());
    }

    #[test]
    fn collects_sanitized_references() {
        let tmp = TempDir::new().unwrap();
        let skills_root = tmp.path().join("skills");
        let skill_dir = skills_root.join("with-refs");
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: with-refs\ndescription: Has refs\n---\n## Steps\n1. Go\n",
        )
        .unwrap();
        fs::write(
            skill_dir.join("references/guide.md"),
            "## API\nUse the public endpoint safely.",
        )
        .unwrap();
        let opts = SkillPatternOptions::from_change_kind(SkillChangeKind::Agent);
        let pattern = build_work_package_skill(&skill_dir, &skills_root, &opts).unwrap();
        assert_eq!(pattern.references_redacted.len(), 1);
        assert_eq!(pattern.references_redacted[0].relative_path, "references/guide.md");
    }

    #[test]
    fn collects_templates_scripts_and_nested_files() {
        let tmp = TempDir::new().unwrap();
        let skills_root = tmp.path().join("skills");
        let skill_dir = skills_root.join("full-skill");
        fs::create_dir_all(skill_dir.join("templates")).unwrap();
        fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        fs::create_dir_all(skill_dir.join("references/nested")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: full-skill\ndescription: All dirs\n---\n## Steps\n1. Go\n",
        )
        .unwrap();
        fs::write(
            skill_dir.join("templates/output.md"),
            "## Output\nUse this template.",
        )
        .unwrap();
        fs::write(
            skill_dir.join("scripts/run.py"),
            "def main():\n    print('hello')\n",
        )
        .unwrap();
        fs::write(
            skill_dir.join("references/nested/detail.md"),
            "## Detail\nNested reference doc.",
        )
        .unwrap();
        let opts = SkillPatternOptions::from_change_kind(SkillChangeKind::Agent);
        let pattern = build_work_package_skill(&skill_dir, &skills_root, &opts).unwrap();
        assert_eq!(pattern.references_redacted.len(), 3);
        let paths: Vec<_> = pattern
            .references_redacted
            .iter()
            .map(|s| s.relative_path.as_str())
            .collect();
        assert!(paths.contains(&"templates/output.md"));
        assert!(paths.contains(&"scripts/run.py"));
        assert!(paths.contains(&"references/nested/detail.md"));
    }

    #[test]
    fn skips_binary_assets() {
        let tmp = TempDir::new().unwrap();
        let skills_root = tmp.path().join("skills");
        let skill_dir = skills_root.join("with-assets");
        fs::create_dir_all(skill_dir.join("assets")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: with-assets\ndescription: Assets\n---\n## Steps\n1. Go\n",
        )
        .unwrap();
        fs::write(skill_dir.join("assets/logo.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        fs::write(
            skill_dir.join("assets/readme.txt"),
            "Public branding notes only.",
        )
        .unwrap();
        let opts = SkillPatternOptions::from_change_kind(SkillChangeKind::Agent);
        let pattern = build_work_package_skill(&skill_dir, &skills_root, &opts).unwrap();
        assert_eq!(pattern.references_redacted.len(), 1);
        assert_eq!(pattern.references_redacted[0].relative_path, "assets/readme.txt");
    }

    #[test]
    fn skill_pattern_json_always_includes_references_redacted() {
        let tmp = TempDir::new().unwrap();
        let skills_root = tmp.path().join("skills");
        let skill_dir = skills_root.join("json-key");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: json-key\ndescription: No refs\n---\n## Steps\n1. Go\n",
        )
        .unwrap();
        let opts = SkillPatternOptions::from_change_kind(SkillChangeKind::Agent);
        let pattern = build_work_package_skill(&skill_dir, &skills_root, &opts).unwrap();
        let value = serde_json::to_value(&pattern).unwrap();
        assert!(
            value.get("references_redacted").is_some(),
            "references_redacted must always be present in upload JSON"
        );
        assert_eq!(
            value.get("references_redacted").unwrap(),
            &serde_json::json!([])
        );
    }

    #[test]
    fn find_skill_dir_by_slug_locates_skill() {
        let tmp = TempDir::new().unwrap();
        let skills_root = tmp.path().join("skills");
        let skill_dir = skills_root.join("nested").join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: nested\n---\n## Steps\n1. One\n",
        )
        .unwrap();
        let found = find_skill_dir_by_slug(&skills_root, "my-skill").unwrap();
        assert_eq!(found, skill_dir);
    }

    #[test]
    fn walk_unique_dedupes_same_pattern() {
        let tmp = TempDir::new().unwrap();
        let skills_root = tmp.path().join("skills");
        for name in ["a", "b"] {
            let dir = skills_root.join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                "---\nname: same-skill\ndescription: dup\n---\n## Steps\n1. One\n",
            )
            .unwrap();
        }
        let opts = SkillPatternOptions::from_change_kind(SkillChangeKind::Agent);
        let mut seen = std::collections::HashSet::new();
        let mut patterns = Vec::new();
        walk_unique_skill_dirs(&skills_root, |d| {
            if let Some(p) = build_work_package_skill(d, &skills_root, &opts) {
                if seen.insert(p.pattern_id.clone()) {
                    patterns.push(p);
                }
            }
        });
        assert_eq!(patterns.len(), 1);
    }
}
