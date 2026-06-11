//! Skill installation infrastructure — pure data-processing functions
//! extracted from mod.rs to keep slash command dispatch separate.
//!
//! This module has NO dependency on `App`, `CommandResult`, or slash-command
//! dispatch. It is used by `skills.rs` (CLI skills subcommand) and by unit
//! tests in the parent module.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use bytes::Bytes;
use hermes_core::AgentError;
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(crate) const DEFAULT_SKILL_TAPS: &[&str] = &[
    "https://github.com/NousResearch/hermes-agent::skills",
    "https://github.com/NousResearch/hermes-agent::optional-skills",
    "https://github.com/openai/skills::skills",
    "https://github.com/anthropics/skills::skills",
    "https://github.com/VoltAgent/awesome-agent-skills::skills",
    "https://github.com/mattpocock/skills::skills",
    "https://github.com/github/awesome-copilot::skills",
    "https://github.com/garrytan/gstack::",
    "https://github.com/MiniMax-AI/cli::skill",
];

const GITHUB_API_BASE: &str = "https://api.github.com";
const OFFICIAL_SKILLS_REPO: &str = "nousresearch/hermes-agent";
const HERMES_SKILLS_INDEX_URL: &str =
    "https://hermes-agent.nousresearch.com/docs/api/skills-index.json";
const SKILLS_SH_SEARCH_URL: &str = "https://skills.sh/api/search";
const CLAWHUB_API_BASE: &str = "https://clawhub.ai/api/v1";
const SKILLS_HUB_STATE_DIR: &str = hermes_skills::HUB_STATE_DIR;
const SKILLS_HUB_AUDIT_FILE: &str = "audit.log";
pub(crate) const SENTRUX_MCP_SERVER_NAME: &str = "sentrux";
pub(crate) const SENTRUX_MCP_COMMAND: &str = "sentrux";
pub(crate) const SENTRUX_MCP_ARG: &str = "--mcp";
const SKILL_BOOTSTRAP_ALLOWED_EXECUTABLES: &[&str] = &[
    "bash", "sh", "python", "python3", "pip", "pip3", "pipx", "uv", "uvx", "node", "npm", "npx",
    "pnpm", "yarn", "bun", "cargo", "rustup", "go", "make", "cmake", "git", "brew", "apt",
    "apt-get", "dnf", "yum", "pacman", "zypper", "apk",
];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillTapSpec {
    pub(crate) repo: String,
    pub(crate) path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedSkillSource {
    pub(crate) repo: String,
    pub(crate) branch: String,
    pub(crate) skill_dir: String,
}

#[derive(Debug, Clone)]
pub(crate) enum RegistryInstallSource {
    GitHub(ResolvedSkillSource),
    LobeHub {
        slug: String,
    },
    ClawHub {
        slug: String,
        version: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct RegistrySkillRecord {
    pub(crate) identifier: String,
    pub(crate) description: String,
    pub(crate) source: String,
    pub(crate) score: i32,
    pub(crate) install_source: RegistryInstallSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallFallbackSource {
    SkillsSh,
    Tap,
}

pub(crate) type SkillHubInstalledEntry = hermes_skills::SkillHubInstalledEntry;
pub(crate) type SkillsHubLockFile = hermes_skills::SkillsHubLock;

#[derive(Debug, Clone)]
pub(crate) struct SkillInstallProvenance {
    pub(crate) source: String,
    pub(crate) identifier: String,
    pub(crate) trust_level: String,
    pub(crate) metadata: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SkillBootstrapPlan {
    pub(crate) commands: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedBootstrapCommand {
    pub(crate) display: String,
    pub(crate) executable: String,
    pub(crate) args: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HermesSkillsIndexResponse {
    #[serde(default)]
    skills: Vec<HermesSkillsIndexEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct HermesSkillsIndexEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    resolved_github_id: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SkillsShSearchResponse {
    #[serde(default)]
    skills: Vec<SkillsShSearchEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct SkillsShSearchEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    #[serde(rename = "skillId")]
    skill_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    source: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct LobeHubMeta {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct LobeHubAgentResponse {
    #[serde(default)]
    author: String,
    #[serde(default)]
    homepage: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    meta: LobeHubMeta,
    #[serde(default)]
    config: LobeHubConfig,
}

#[derive(Debug, Default, Deserialize)]
struct LobeHubConfig {
    #[serde(default)]
    #[serde(rename = "systemRole")]
    system_role: String,
}

#[derive(Debug, Deserialize)]
struct ClawHubSkillDetailResponse {
    #[serde(default)]
    #[serde(rename = "latestVersion")]
    latest_version: ClawHubLatestVersion,
}

#[derive(Debug, Default, Deserialize)]
struct ClawHubLatestVersion {
    #[serde(default)]
    version: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRepoInfo {
    default_branch: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubTreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct GitHubTreeResponse {
    tree: Vec<GitHubTreeEntry>,
}

// ---------------------------------------------------------------------------
// Parsing / resolution functions
// ---------------------------------------------------------------------------

pub(crate) fn parse_skill_tap_spec(raw: &str) -> Option<SkillTapSpec> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (base, override_path) = if let Some((lhs, rhs)) = trimmed.split_once("::") {
        (lhs.trim(), Some(rhs.trim()))
    } else {
        (trimmed, None)
    };

    let (repo, mut path) = if let Some(rest) = base
        .strip_prefix("https://github.com/")
        .or_else(|| base.strip_prefix("http://github.com/"))
    {
        let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return None;
        }
        let path = if segments.len() >= 5 && segments[2] == "tree" {
            segments[4..].join("/")
        } else {
            "skills".to_string()
        };
        (format!("{}/{}", segments[0], segments[1]), path)
    } else {
        let segments: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return None;
        }
        let path = if segments.len() > 2 {
            segments[2..].join("/")
        } else {
            "skills".to_string()
        };
        (format!("{}/{}", segments[0], segments[1]), path)
    };

    if let Some(override_path) = override_path {
        path = override_path.to_string();
    }

    Some(SkillTapSpec {
        repo,
        path: path.trim_matches('/').to_string(),
    })
}

pub(crate) fn parse_skill_name_and_version(spec: &str) -> (String, Option<String>) {
    let trimmed = spec.trim();
    if let Some((name, version)) = trimmed.rsplit_once('@') {
        if !name.is_empty() && !version.is_empty() && !name.starts_with("https://") {
            return (name.to_string(), Some(version.to_string()));
        }
    }
    (trimmed.to_string(), None)
}

pub(crate) fn looks_like_github_repo_slug(token: &str) -> bool {
    let parts: Vec<&str> = token.split('/').filter(|s| !s.is_empty()).collect();
    parts.len() == 2
}

pub(crate) fn parse_explicit_github_skill(spec: &str) -> Option<(String, Option<String>, String)> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Registry-prefixed identifiers (official/..., skills.sh/..., etc.)
    // must not be treated as direct GitHub owner/repo/path slugs.
    if parse_registry_prefixed_skill(trimmed).is_some() {
        return None;
    }

    if let Some(rest) = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
    {
        let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return None;
        }
        let repo = format!("{}/{}", segments[0], segments[1]);
        if segments.len() >= 5 && segments[2] == "tree" {
            let branch = segments[3].to_string();
            let path = segments[4..].join("/");
            if path.is_empty() {
                return None;
            }
            return Some((repo, Some(branch), path));
        }
        if segments.len() > 2 {
            let path = segments[2..].join("/");
            if path.is_empty() {
                return None;
            }
            return Some((repo, None, path));
        }
        return None;
    }

    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() >= 3 {
        let repo = format!("{}/{}", segments[0], segments[1]);
        let path = segments[2..].join("/");
        if path.is_empty() {
            return None;
        }
        return Some((repo, None, path));
    }

    None
}

pub(crate) fn sanitize_skill_install_name(source: &str) -> String {
    let raw = source
        .trim()
        .split('/')
        .next_back()
        .unwrap_or(source)
        .trim();
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            out.push(ch);
        } else if out.ends_with('_') {
            continue;
        } else {
            out.push('_');
        }
    }
    let normalized = out.trim_matches('_').to_string();
    if normalized.is_empty() {
        "skill".to_string()
    } else {
        normalized
    }
}

pub(crate) fn ensure_safe_relative_path(path: &str) -> Result<(), AgentError> {
    if path.is_empty() {
        return Err(AgentError::Config("Empty path in skill bundle.".into()));
    }
    if path.starts_with('/') || path.contains('\\') {
        return Err(AgentError::Config(format!(
            "Unsafe path in skill bundle: {}",
            path
        )));
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(AgentError::Config(format!(
                "Unsafe path segment in skill bundle: {}",
                path
            )));
        }
    }
    Ok(())
}

pub(crate) fn parse_registry_prefixed_skill(spec: &str) -> Option<(String, String)> {
    let (prefix, rest) = spec.split_once('/')?;
    let normalized = prefix.trim().to_ascii_lowercase();
    let source = match normalized.as_str() {
        "official" => "official",
        "github" => "github",
        "skills.sh" | "skills-sh" => "skills.sh",
        "lobehub" => "lobehub",
        "clawhub" => "clawhub",
        "claude-marketplace" => "claude-marketplace",
        _ => return None,
    };
    let key = rest.trim();
    if key.is_empty() {
        return None;
    }
    Some((source.to_string(), key.to_string()))
}

pub(crate) fn score_registry_match(entry: &HermesSkillsIndexEntry, query: &str) -> i32 {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return 0;
    }

    let name = entry.name.to_ascii_lowercase();
    let id = entry.identifier.to_ascii_lowercase();
    let desc = entry.description.to_ascii_lowercase();
    let tags = entry
        .tags
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");

    if id == q || name == q {
        return 1000;
    }
    if id.starts_with(&q) || name.starts_with(&q) {
        return 900;
    }
    if id.contains(&q) || name.contains(&q) {
        return 700;
    }
    if tags.contains(&q) {
        return 550;
    }
    if desc.contains(&q) {
        return 450;
    }
    0
}

pub(crate) fn skill_source_priority(source: &str) -> usize {
    match source.trim().to_ascii_lowercase().as_str() {
        "official" => 0,
        "skills.sh" | "skills-sh" => 1,
        "well-known" => 2,
        "url" => 3,
        "github" => 4,
        "clawhub" => 5,
        "claude-marketplace" => 6,
        "lobehub" => 7,
        _ => 99,
    }
}

pub(crate) fn sort_registry_skill_records(records: &mut [RegistrySkillRecord]) {
    records.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| skill_source_priority(&a.source).cmp(&skill_source_priority(&b.source)))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.identifier.cmp(&b.identifier))
    });
}

pub(crate) async fn fetch_hermes_skills_index(
    client: &reqwest::Client,
) -> Result<Vec<HermesSkillsIndexEntry>, AgentError> {
    let resp = client
        .get(HERMES_SKILLS_INDEX_URL)
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("Skills index request failed: {}", e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "Skills index lookup failed ({}): {}",
            status, body
        )));
    }
    let payload = resp
        .json::<HermesSkillsIndexResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid skills index response: {}", e)))?;
    Ok(payload.skills)
}

pub(crate) fn resolved_source_from_index(
    entry: &HermesSkillsIndexEntry,
) -> Option<RegistryInstallSource> {
    let source = entry.source.to_ascii_lowercase();
    if source == "lobehub" {
        let slug = entry
            .identifier
            .strip_prefix("lobehub/")
            .unwrap_or(entry.identifier.as_str())
            .trim()
            .to_string();
        if slug.is_empty() {
            return None;
        }
        return Some(RegistryInstallSource::LobeHub { slug });
    }
    if source == "clawhub" {
        let slug = entry.identifier.trim().to_string();
        if slug.is_empty() {
            return None;
        }
        return Some(RegistryInstallSource::ClawHub {
            slug,
            version: None,
        });
    }
    if source == "official" {
        let path = entry.path.trim().trim_matches('/');
        if path.is_empty() {
            return None;
        }
        return Some(RegistryInstallSource::GitHub(ResolvedSkillSource {
            repo: OFFICIAL_SKILLS_REPO.to_string(),
            branch: "main".to_string(),
            skill_dir: path.to_string(),
        }));
    }

    if let Some(resolved) = entry.resolved_github_id.as_deref() {
        if let Some((repo, _, skill_dir)) = parse_explicit_github_skill(resolved) {
            return Some(RegistryInstallSource::GitHub(ResolvedSkillSource {
                repo,
                branch: "main".to_string(),
                skill_dir,
            }));
        }
    }

    if !entry.repo.trim().is_empty() {
        let dir = if !entry.path.trim().is_empty() {
            entry.path.trim_matches('/').to_string()
        } else {
            // claude-marketplace entries often point at repo root collections.
            "skills".to_string()
        };
        return Some(RegistryInstallSource::GitHub(ResolvedSkillSource {
            repo: entry.repo.trim().to_string(),
            branch: "main".to_string(),
            skill_dir: dir,
        }));
    }

    None
}

pub(crate) async fn search_multi_registry(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<Vec<RegistrySkillRecord>, AgentError> {
    let entries = fetch_hermes_skills_index(client).await?;
    let mut matches: Vec<RegistrySkillRecord> = Vec::new();
    for entry in entries {
        let score = score_registry_match(&entry, query);
        if score <= 0 {
            continue;
        }
        let Some(install_source) = resolved_source_from_index(&entry) else {
            continue;
        };
        matches.push(RegistrySkillRecord {
            identifier: entry.identifier.clone(),
            description: entry.description.clone(),
            source: entry.source.clone(),
            score,
            install_source,
        });
    }

    sort_registry_skill_records(&mut matches);
    if matches.len() > limit {
        matches.truncate(limit);
    }
    Ok(matches)
}

pub(crate) async fn resolve_skill_via_registry_index(
    client: &reqwest::Client,
    requested: &str,
    source_hint: Option<&str>,
) -> Result<RegistrySkillRecord, AgentError> {
    let entries = fetch_hermes_skills_index(client).await?;
    let requested_l = requested.trim().to_ascii_lowercase();
    let source_hint = source_hint.map(|s| s.to_ascii_lowercase());

    let mut exact: Vec<RegistrySkillRecord> = Vec::new();
    let mut fuzzy: Vec<RegistrySkillRecord> = Vec::new();
    for entry in entries {
        if let Some(ref hint) = source_hint {
            if entry.source.to_ascii_lowercase() != *hint {
                continue;
            }
        }
        let Some(install_source) = resolved_source_from_index(&entry) else {
            continue;
        };
        let source_l = entry.source.to_ascii_lowercase();
        let identifier_l = entry.identifier.to_ascii_lowercase();
        let name_l = entry.name.to_ascii_lowercase();
        let source_scoped = format!("{}/{}", source_l, name_l);
        let source_scoped_id = format!("{}/{}", source_l, identifier_l);
        let rec = RegistrySkillRecord {
            identifier: entry.identifier.clone(),
            description: entry.description.clone(),
            source: entry.source.clone(),
            score: score_registry_match(&entry, requested),
            install_source,
        };
        if requested_l == identifier_l
            || requested_l == name_l
            || requested_l == source_scoped
            || requested_l == source_scoped_id
        {
            exact.push(rec);
        } else if identifier_l.contains(&requested_l) || name_l.contains(&requested_l) {
            fuzzy.push(rec);
        }
    }

    sort_registry_skill_records(&mut exact);
    sort_registry_skill_records(&mut fuzzy);

    if let Some(first) = exact.into_iter().next() {
        return Ok(first);
    }
    if let Some(first) = fuzzy.into_iter().next() {
        return Ok(first);
    }
    Err(AgentError::Config(format!(
        "Skill '{}' was not found in multi-registry index.",
        requested
    )))
}

pub(crate) fn build_lobehub_skill_markdown(payload: &LobeHubAgentResponse, slug: &str) -> String {
    let title = if payload.meta.title.trim().is_empty() {
        slug.to_string()
    } else {
        payload.meta.title.trim().to_string()
    };
    let description = if payload.meta.description.trim().is_empty() {
        payload.summary.trim().to_string()
    } else {
        payload.meta.description.trim().to_string()
    };
    let role = payload.config.system_role.trim();

    let mut md = String::new();
    md.push_str("---\n");
    md.push_str(&format!("name: {}\n", slug));
    if !description.is_empty() {
        md.push_str(&format!(
            "description: {}\n",
            description.replace('\n', " ")
        ));
    }
    md.push_str("category: lobehub\n");
    md.push_str("---\n\n");
    md.push_str(&format!("# {}\n\n", title));
    if !description.is_empty() {
        md.push_str(&format!("{}\n\n", description));
    }
    md.push_str("## Source\n");
    md.push_str(&format!("- Registry: lobehub\n- Identifier: {}\n", slug));
    if !payload.author.trim().is_empty() {
        md.push_str(&format!("- Author: {}\n", payload.author.trim()));
    }
    if !payload.homepage.trim().is_empty() {
        md.push_str(&format!("- Homepage: {}\n", payload.homepage.trim()));
    }
    md.push_str("\n## Instructions\n");
    if role.is_empty() {
        md.push_str("No system role provided by source registry.\n");
    } else {
        md.push_str(role);
        md.push('\n');
    }
    md
}

pub(crate) fn default_trust_level_for_source(source: &str) -> &'static str {
    match source {
        "official" => "builtin",
        "skills.sh" | "hermes-index" | "claude-marketplace" | "github" | "tap" => "trusted",
        "lobehub" | "clawhub" => "community",
        _ => "community",
    }
}

// ---------------------------------------------------------------------------
// Hub lock functions
// ---------------------------------------------------------------------------

pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub(crate) fn skills_hub_state_dir(skills_dir: &Path) -> PathBuf {
    skills_dir.join(SKILLS_HUB_STATE_DIR)
}

pub(crate) fn skills_hub_lock_path(skills_dir: &Path) -> PathBuf {
    hermes_skills::hub_lock_path(skills_dir)
}

pub(crate) fn skills_hub_audit_path(skills_dir: &Path) -> PathBuf {
    skills_hub_state_dir(skills_dir).join(SKILLS_HUB_AUDIT_FILE)
}

pub(crate) fn read_skills_hub_lock(skills_dir: &Path) -> SkillsHubLockFile {
    hermes_skills::read_hub_lock(skills_dir)
}

pub(crate) fn write_skills_hub_lock(
    skills_dir: &Path,
    lock: &SkillsHubLockFile,
) -> Result<(), AgentError> {
    let state_dir = skills_hub_state_dir(skills_dir);
    std::fs::create_dir_all(&state_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create skills hub state dir '{}': {}",
            state_dir.display(),
            e
        ))
    })?;
    let path = skills_hub_lock_path(skills_dir);
    let body = serde_json::to_string_pretty(lock)
        .map_err(|e| AgentError::Config(format!("Failed to serialize skills hub lock: {}", e)))?;
    std::fs::write(&path, body).map_err(|e| {
        AgentError::Io(format!(
            "Failed to write skills hub lock '{}': {}",
            path.display(),
            e
        ))
    })
}

pub(crate) fn append_skills_hub_audit(
    skills_dir: &Path,
    action: &str,
    entry: &SkillHubInstalledEntry,
) -> Result<(), AgentError> {
    let state_dir = skills_hub_state_dir(skills_dir);
    std::fs::create_dir_all(&state_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create skills hub state dir '{}': {}",
            state_dir.display(),
            e
        ))
    })?;
    let path = skills_hub_audit_path(skills_dir);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| {
            AgentError::Io(format!(
                "Failed to open skills hub audit log '{}': {}",
                path.display(),
                e
            ))
        })?;
    let line = serde_json::json!({
        "timestamp": now_rfc3339(),
        "action": action,
        "name": entry.name,
        "source": entry.source,
        "identifier": entry.identifier,
        "trust_level": entry.trust_level,
        "scan_verdict": entry.scan_verdict,
        "content_hash": entry.content_hash,
    });
    use std::io::Write as _;
    writeln!(file, "{}", line)
        .map_err(|e| AgentError::Io(format!("Failed to append skills hub audit log: {}", e)))
}

pub(crate) fn hash_skill_bundle(files: &[(String, Bytes)]) -> String {
    let mut sorted: Vec<_> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (rel_path, bytes) in sorted {
        h.update(rel_path.as_bytes());
        h.update([0]);
        h.update(bytes.as_ref());
        h.update([0xFF]);
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

pub(crate) fn collect_skill_files_recursive(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), AgentError> {
    for entry in std::fs::read_dir(dir)
        .map_err(|e| AgentError::Io(format!("Failed to read dir '{}': {}", dir.display(), e)))?
    {
        let entry = entry.map_err(|e| {
            AgentError::Io(format!(
                "Failed to read dir entry '{}': {}",
                dir.display(),
                e
            ))
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| {
            AgentError::Io(format!(
                "Failed to get file type for '{}': {}",
                path.display(),
                e
            ))
        })?;
        if file_type.is_dir() {
            collect_skill_files_recursive(root, &path, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|e| AgentError::Io(format!("Failed to compute relative path: {}", e)))?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path)
            .map_err(|e| AgentError::Io(format!("Failed to read '{}': {}", path.display(), e)))?;
        out.push((rel, bytes));
    }
    Ok(())
}

pub(crate) fn hash_installed_skill_dir(skill_dir: &Path) -> Result<String, AgentError> {
    if !skill_dir.exists() {
        return Err(AgentError::Config(format!(
            "Installed skill path does not exist: {}",
            skill_dir.display()
        )));
    }
    let mut files = Vec::new();
    collect_skill_files_recursive(skill_dir, skill_dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (rel_path, bytes) in files {
        h.update(rel_path.as_bytes());
        h.update([0]);
        h.update(&bytes);
        h.update([0xFF]);
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("sha256:{hex}"))
}

pub(crate) fn record_skill_install_in_hub_lock(
    skills_dir: &Path,
    installed_name: &str,
    install_path: &Path,
    files: &[(String, Bytes)],
    provenance: &SkillInstallProvenance,
) -> Result<(), AgentError> {
    let mut lock = read_skills_hub_lock(skills_dir);
    let now = now_rfc3339();
    let install_path_rel = install_path
        .strip_prefix(skills_dir)
        .unwrap_or(install_path)
        .to_string_lossy()
        .replace('\\', "/");
    let content_hash = hash_installed_skill_dir(install_path)?;
    let files_rel: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let entry = SkillHubInstalledEntry {
        name: installed_name.to_string(),
        source: provenance.source.clone(),
        identifier: provenance.identifier.clone(),
        trust_level: provenance.trust_level.clone(),
        scan_verdict: "clean".to_string(),
        content_hash,
        install_path: install_path_rel,
        files: files_rel,
        metadata: provenance.metadata.clone(),
        installed_at: now.clone(),
        updated_at: now,
    };
    lock.installed.retain(|item| item.name != installed_name);
    lock.installed.push(entry.clone());
    lock.installed.sort_by(|a, b| a.name.cmp(&b.name));
    write_skills_hub_lock(skills_dir, &lock)?;
    append_skills_hub_audit(skills_dir, "INSTALL", &entry)?;
    Ok(())
}

pub(crate) fn record_skill_uninstall_in_hub_lock(
    skills_dir: &Path,
    skill_name: &str,
) -> Result<Option<SkillHubInstalledEntry>, AgentError> {
    let mut lock = read_skills_hub_lock(skills_dir);
    let mut removed: Option<SkillHubInstalledEntry> = None;
    lock.installed.retain(|entry| {
        if entry.name == skill_name {
            removed = Some(entry.clone());
            false
        } else {
            true
        }
    });
    write_skills_hub_lock(skills_dir, &lock)?;
    if let Some(ref removed_entry) = removed {
        append_skills_hub_audit(skills_dir, "UNINSTALL", removed_entry)?;
    }
    Ok(removed)
}

pub(crate) fn skills_install_force(extra: Option<&str>) -> bool {
    if extra
        .map(|e| e.split_whitespace().any(|t| t == "--force"))
        .unwrap_or(false)
    {
        return true;
    }
    std::env::var("HERMES_SKILLS_INSTALL_FORCE")
        .ok()
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

pub(crate) fn skill_guard_enforce_bundle(
    install_name: &str,
    source: &str,
    files: &[(String, Bytes)],
    force: bool,
) -> Result<(), AgentError> {
    let file_vec: Vec<(String, Vec<u8>)> =
        files.iter().map(|(p, b)| (p.clone(), b.to_vec())).collect();
    hermes_skills::SkillGuard::enforce_install_bundle(install_name, source, &file_vec, force)
        .map_err(|e| AgentError::Config(e.to_string()))
}

// ---------------------------------------------------------------------------
// Source resolution — GitHub helpers
// ---------------------------------------------------------------------------

pub(crate) fn github_request(
    client: &reqwest::Client,
    url: &str,
    accept: &str,
) -> reqwest::RequestBuilder {
    let mut req = client
        .get(url)
        .header("Accept", accept)
        .header("User-Agent", "hermes-agent-ultra");
    if let Ok(token) = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .map(|v| v.trim().to_string())
    {
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }
    }
    req
}

pub(crate) async fn github_default_branch(
    client: &reqwest::Client,
    repo: &str,
) -> Result<String, AgentError> {
    let url = format!("{}/repos/{}", GITHUB_API_BASE, repo);
    let resp = github_request(client, &url, "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("GitHub request failed for {}: {}", repo, e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "GitHub repo lookup failed for {} ({}): {}",
            repo, status, body
        )));
    }
    let payload = resp
        .json::<GitHubRepoInfo>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid GitHub repo response: {}", e)))?;
    Ok(payload.default_branch)
}

pub(crate) async fn github_repo_tree(
    client: &reqwest::Client,
    repo: &str,
    branch: &str,
) -> Result<Vec<GitHubTreeEntry>, AgentError> {
    let encoded_branch = urlencoding::encode(branch);
    let url = format!(
        "{}/repos/{}/git/trees/{}?recursive=1",
        GITHUB_API_BASE, repo, encoded_branch
    );
    let resp = github_request(client, &url, "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("GitHub tree request failed: {}", e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "GitHub tree lookup failed for {repo}@{branch} ({status}): {body}"
        )));
    }
    let payload = resp
        .json::<GitHubTreeResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid GitHub tree response: {}", e)))?;
    Ok(payload.tree)
}

// ---------------------------------------------------------------------------
// Source resolution — taps
// ---------------------------------------------------------------------------

pub(crate) async fn resolve_skill_via_taps(
    client: &reqwest::Client,
    taps: &[String],
    requested_skill: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let mut suggestions: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for tap in taps {
        let Some(spec) = parse_skill_tap_spec(tap) else {
            continue;
        };
        let branch = match github_default_branch(client, &spec.repo).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tree = match github_repo_tree(client, &spec.repo, &branch).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path_prefix = if spec.path.is_empty() {
            String::new()
        } else {
            format!("{}/", spec.path.trim_matches('/'))
        };
        for entry in tree {
            if entry.kind != "blob" || !entry.path.ends_with("/SKILL.md") {
                continue;
            }
            if !path_prefix.is_empty() && !entry.path.starts_with(&path_prefix) {
                continue;
            }
            let skill_dir = entry.path.trim_end_matches("/SKILL.md");
            let skill_name = skill_dir
                .split('/')
                .next_back()
                .unwrap_or(skill_dir)
                .to_string();
            if skill_name.eq_ignore_ascii_case(requested_skill) {
                return Ok(ResolvedSkillSource {
                    repo: spec.repo.clone(),
                    branch,
                    skill_dir: skill_dir.to_string(),
                });
            }
            if skill_name
                .to_ascii_lowercase()
                .contains(&requested_skill.to_ascii_lowercase())
            {
                suggestions.insert(skill_name);
            }
        }
    }

    let suggestion_text = if suggestions.is_empty() {
        "none".to_string()
    } else {
        suggestions
            .into_iter()
            .take(8)
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(AgentError::Config(format!(
        "Skill '{}' not found in configured taps. Suggestions: {}",
        requested_skill, suggestion_text
    )))
}

pub(crate) async fn resolve_skill_in_repo(
    client: &reqwest::Client,
    repo: &str,
    requested_skill: &str,
    preferred_prefix: Option<&str>,
) -> Result<ResolvedSkillSource, AgentError> {
    let branch = github_default_branch(client, repo).await?;
    let tree = github_repo_tree(client, repo, &branch).await?;

    let preferred_prefix = preferred_prefix
        .map(|v| v.trim_matches('/').to_string())
        .unwrap_or_default();
    let mut exact_candidates: Vec<String> = Vec::new();
    let mut fuzzy_candidates: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    for entry in tree {
        if entry.kind != "blob" || !entry.path.ends_with("/SKILL.md") {
            continue;
        }
        let skill_dir = entry.path.trim_end_matches("/SKILL.md").to_string();
        let skill_name = skill_dir
            .split('/')
            .next_back()
            .unwrap_or(skill_dir.as_str())
            .to_string();
        if skill_name.eq_ignore_ascii_case(requested_skill) {
            exact_candidates.push(skill_dir.clone());
        } else if skill_name
            .to_ascii_lowercase()
            .contains(&requested_skill.to_ascii_lowercase())
        {
            fuzzy_candidates.insert(skill_name);
        }
    }

    if exact_candidates.is_empty() {
        let suggestion_text = if fuzzy_candidates.is_empty() {
            "none".to_string()
        } else {
            fuzzy_candidates
                .into_iter()
                .take(8)
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(AgentError::Config(format!(
            "Skill '{}' not found in repo {}. Suggestions: {}",
            requested_skill, repo, suggestion_text
        )));
    }

    exact_candidates.sort_by_key(|candidate| {
        let preferred = if preferred_prefix.is_empty() {
            1usize
        } else if candidate.starts_with(&format!("{}/", preferred_prefix)) {
            0usize
        } else {
            1usize
        };
        (preferred, candidate.len(), candidate.clone())
    });
    let skill_dir = exact_candidates
        .into_iter()
        .next()
        .ok_or_else(|| AgentError::Config("No matching skill path found.".into()))?;

    Ok(ResolvedSkillSource {
        repo: repo.to_string(),
        branch,
        skill_dir,
    })
}

pub(crate) async fn search_skills_via_taps(
    client: &reqwest::Client,
    taps: &[String],
    query: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, AgentError> {
    let query_l = query.to_ascii_lowercase();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut out: Vec<(String, String)> = Vec::new();

    for tap in taps {
        let Some(spec) = parse_skill_tap_spec(tap) else {
            continue;
        };
        let branch = match github_default_branch(client, &spec.repo).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tree = match github_repo_tree(client, &spec.repo, &branch).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path_prefix = if spec.path.is_empty() {
            String::new()
        } else {
            format!("{}/", spec.path.trim_matches('/'))
        };
        for entry in tree {
            if entry.kind != "blob" || !entry.path.ends_with("/SKILL.md") {
                continue;
            }
            if !path_prefix.is_empty() && !entry.path.starts_with(&path_prefix) {
                continue;
            }
            let skill_dir = entry.path.trim_end_matches("/SKILL.md");
            let skill_name = skill_dir.split('/').next_back().unwrap_or(skill_dir);
            if !skill_name.to_ascii_lowercase().contains(&query_l) {
                continue;
            }
            let key = format!("{}/{}", spec.repo, skill_dir);
            if seen.insert(key.clone()) {
                out.push((skill_name.to_string(), key));
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }

    Ok(out)
}

pub(crate) async fn fetch_skill_files_from_github(
    client: &reqwest::Client,
    source: &ResolvedSkillSource,
) -> Result<Vec<(String, Bytes)>, AgentError> {
    let tree = github_repo_tree(client, &source.repo, &source.branch).await?;
    let prefix = format!("{}/", source.skill_dir.trim_matches('/'));
    let mut files = Vec::new();

    for entry in tree {
        if entry.kind != "blob" || !entry.path.starts_with(&prefix) {
            continue;
        }
        let rel_path = entry.path[prefix.len()..].to_string();
        ensure_safe_relative_path(&rel_path)?;
        let raw_path = entry
            .path
            .split('/')
            .map(|segment| urlencoding::encode(segment).to_string())
            .collect::<Vec<_>>()
            .join("/");
        let raw_url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}",
            source.repo, source.branch, raw_path
        );
        let bytes = match client
            .get(&raw_url)
            .header("User-Agent", "hermes-agent-ultra")
            .timeout(std::time::Duration::from_secs(25))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp
                .bytes()
                .await
                .map_err(|e| AgentError::Config(format!("Invalid file payload: {}", e)))?,
            _ => {
                let encoded_path = entry
                    .path
                    .split('/')
                    .map(urlencoding::encode)
                    .collect::<Vec<_>>()
                    .join("/");
                let api_url = format!(
                    "{}/repos/{}/contents/{}?ref={}",
                    GITHUB_API_BASE,
                    source.repo,
                    encoded_path,
                    urlencoding::encode(&source.branch)
                );
                let resp = github_request(client, &api_url, "application/vnd.github.v3.raw")
                    .timeout(std::time::Duration::from_secs(25))
                    .send()
                    .await
                    .map_err(|e| {
                        AgentError::Config(format!("GitHub file download failed: {}", e))
                    })?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(AgentError::Config(format!(
                        "Failed to download {} from {} ({}): {}",
                        rel_path, source.repo, status, body
                    )));
                }
                resp.bytes()
                    .await
                    .map_err(|e| AgentError::Config(format!("Invalid file payload: {}", e)))?
            }
        };
        files.push((rel_path, bytes));
    }

    if !files.iter().any(|(path, _)| path == "SKILL.md") {
        return Err(AgentError::Config(format!(
            "Resolved source {}/{} is missing SKILL.md",
            source.repo, source.skill_dir
        )));
    }
    if files.is_empty() {
        return Err(AgentError::Config(format!(
            "No files found at {}/{}",
            source.repo, source.skill_dir
        )));
    }
    Ok(files)
}

// ---------------------------------------------------------------------------
// LobeHub
// ---------------------------------------------------------------------------

pub(crate) async fn fetch_lobehub_skill_files(
    client: &reqwest::Client,
    slug: &str,
) -> Result<Vec<(String, Bytes)>, AgentError> {
    let url = format!("https://chat-agents.lobehub.com/{}.json", slug);
    let resp = client
        .get(&url)
        .header("Accept", "application/json,text/plain,*/*")
        .header("User-Agent", "Mozilla/5.0 hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("LobeHub request failed: {}", e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "LobeHub lookup failed for '{}' ({}): {}",
            slug, status, body
        )));
    }
    let payload = resp
        .json::<LobeHubAgentResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid LobeHub payload: {}", e)))?;
    let md = build_lobehub_skill_markdown(&payload, slug);
    Ok(vec![("SKILL.md".to_string(), Bytes::from(md))])
}

// ---------------------------------------------------------------------------
// ClawHub
// ---------------------------------------------------------------------------

pub(crate) fn detect_archive_format(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 4
        && bytes[0] == 0x50
        && bytes[1] == 0x4B
        && bytes[2] == 0x03
        && bytes[3] == 0x04
    {
        return "zip";
    }
    if bytes.len() >= 2 && bytes[0] == 0x1F && bytes[1] == 0x8B {
        return "tar.gz";
    }
    "unknown"
}

pub(crate) fn extract_clawhub_archive(bytes: &[u8]) -> Result<Vec<(String, Bytes)>, AgentError> {
    match detect_archive_format(bytes) {
        "zip" => {
            let cursor = std::io::Cursor::new(bytes);
            let mut zip = zip::ZipArchive::new(cursor).map_err(|e| {
                AgentError::Config(format!("Failed to parse ClawHub zip payload: {}", e))
            })?;
            let mut out = Vec::new();
            for i in 0..zip.len() {
                let mut file = zip.by_index(i).map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub zip entry: {}", e))
                })?;
                if file.is_dir() {
                    continue;
                }
                let raw_name = file.name().replace('\\', "/");
                let segments: Vec<&str> = raw_name.split('/').filter(|s| !s.is_empty()).collect();
                let normalized = if segments.is_empty() {
                    file.name().to_string()
                } else if segments.len() == 1 {
                    segments[0].to_string()
                } else {
                    // Drop top-level archive folder if present.
                    segments[1..].join("/")
                };
                ensure_safe_relative_path(&normalized)?;
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut file, &mut buf).map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub file payload: {}", e))
                })?;
                out.push((normalized, Bytes::from(buf)));
            }
            Ok(out)
        }
        "tar.gz" => {
            let decoder = flate2::read::GzDecoder::new(bytes);
            let mut archive = tar::Archive::new(decoder);
            let mut out = Vec::new();
            let entries = archive.entries().map_err(|e| {
                AgentError::Config(format!("Failed to parse ClawHub tar payload: {}", e))
            })?;
            for entry in entries {
                let mut entry = entry.map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub tar entry: {}", e))
                })?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let path = entry
                    .path()
                    .map_err(|e| AgentError::Config(format!("Invalid tar entry path: {}", e)))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let normalized = path.split('/').skip(1).collect::<Vec<_>>().join("/");
                if normalized.is_empty() {
                    continue;
                }
                ensure_safe_relative_path(&normalized)?;
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut buf).map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub tar payload: {}", e))
                })?;
                out.push((normalized, Bytes::from(buf)));
            }
            Ok(out)
        }
        _ => Err(AgentError::Config(
            "Unsupported ClawHub archive format (expected zip or tar.gz).".to_string(),
        )),
    }
}

pub(crate) async fn fetch_clawhub_skill_files(
    client: &reqwest::Client,
    slug: &str,
    version_hint: Option<&str>,
) -> Result<Vec<(String, Bytes)>, AgentError> {
    let detail_url = format!("{}/skills/{}", CLAWHUB_API_BASE, slug);
    let detail = client
        .get(&detail_url)
        .header("Accept", "application/json")
        .header("User-Agent", "Mozilla/5.0 hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("ClawHub detail request failed: {}", e)))?;
    if !detail.status().is_success() {
        let status = detail.status();
        let body = detail.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "ClawHub detail lookup failed for '{}' ({}): {}",
            slug, status, body
        )));
    }
    let payload = detail
        .json::<ClawHubSkillDetailResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid ClawHub detail payload: {}", e)))?;
    let version = version_hint
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.to_string())
        .or_else(|| {
            let v = payload.latest_version.version.trim();
            if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            }
        })
        .ok_or_else(|| {
            AgentError::Config(format!("No ClawHub version available for '{}'.", slug))
        })?;

    let download_url = format!(
        "{}/download?slug={}&version={}",
        CLAWHUB_API_BASE,
        urlencoding::encode(slug),
        urlencoding::encode(&version)
    );
    let mut last_err = String::new();
    for attempt in 1..=4 {
        let resp = client
            .get(&download_url)
            .header("Accept", "*/*")
            .header("User-Agent", "Mozilla/5.0 hermes-agent-ultra")
            .timeout(std::time::Duration::from_secs(40))
            .send()
            .await
            .map_err(|e| AgentError::Config(format!("ClawHub download request failed: {}", e)))?;
        if resp.status().is_success() {
            let bytes = resp.bytes().await.map_err(|e| {
                AgentError::Config(format!("Invalid ClawHub download payload: {}", e))
            })?;
            return extract_clawhub_archive(&bytes);
        }
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait_secs = attempt * 2;
            tokio::time::sleep(std::time::Duration::from_secs(wait_secs as u64)).await;
            last_err = "rate limited (429)".to_string();
            continue;
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "ClawHub download failed for '{}@{}' ({}): {}",
            slug, version, status, body
        )));
    }
    Err(AgentError::Config(format!(
        "ClawHub download for '{}@{}' failed after retries: {}",
        slug, version, last_err
    )))
}

// ---------------------------------------------------------------------------
// Claude Marketplace
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ClaudeMarketplaceManifest {
    #[serde(default)]
    plugins: Vec<ClaudeMarketplacePlugin>,
}

#[derive(Debug, Clone, Deserialize)]
struct ClaudeMarketplacePlugin {
    #[serde(default)]
    name: String,
    #[serde(default)]
    skills: Vec<String>,
}

async fn fetch_claude_marketplace_manifest(
    client: &reqwest::Client,
) -> Result<ClaudeMarketplaceManifest, AgentError> {
    let url = format!(
        "{}/repos/anthropics/skills/contents/.claude-plugin/marketplace.json",
        GITHUB_API_BASE
    );
    let resp = github_request(client, &url, "application/vnd.github.v3.raw")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("Claude marketplace request failed: {}", e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "Claude marketplace lookup failed ({}): {}",
            status, body
        )));
    }
    resp.json::<ClaudeMarketplaceManifest>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid Claude marketplace payload: {}", e)))
}

pub(crate) async fn resolve_claude_marketplace_skill(
    client: &reqwest::Client,
    requested: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let manifest = fetch_claude_marketplace_manifest(client).await?;
    let req = requested.trim().trim_matches('/').to_ascii_lowercase();
    let mut candidate_paths: Vec<String> = Vec::new();
    for plugin in manifest.plugins {
        let plugin_name = plugin.name.to_ascii_lowercase();
        for skill_path in plugin.skills {
            let normalized = skill_path
                .trim()
                .trim_start_matches("./")
                .trim_start_matches('/')
                .to_string();
            if normalized.is_empty() {
                continue;
            }
            let basename = normalized
                .split('/')
                .next_back()
                .unwrap_or(normalized.as_str())
                .to_ascii_lowercase();
            if req == basename
                || req == normalized.to_ascii_lowercase()
                || req == format!("{}/{}", plugin_name, basename)
                || req == format!("{}/{}", plugin_name, normalized.to_ascii_lowercase())
            {
                return Ok(ResolvedSkillSource {
                    repo: "anthropics/skills".to_string(),
                    branch: "main".to_string(),
                    skill_dir: normalized,
                });
            }
            if basename.contains(&req) || normalized.to_ascii_lowercase().contains(&req) {
                candidate_paths.push(normalized);
            }
        }
    }
    candidate_paths.sort();
    candidate_paths.dedup();
    Err(AgentError::Config(format!(
        "Claude marketplace skill '{}' not found. Suggestions: {}",
        requested,
        if candidate_paths.is_empty() {
            "none".to_string()
        } else {
            candidate_paths
                .into_iter()
                .take(8)
                .collect::<Vec<_>>()
                .join(", ")
        }
    )))
}

// ---------------------------------------------------------------------------
// Official skill source resolution
// ---------------------------------------------------------------------------

pub(crate) async fn resolve_official_skill_source(
    client: &reqwest::Client,
    requested: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let req = requested.trim().trim_matches('/');
    if req.is_empty() {
        return Err(AgentError::Config(
            "Missing official skill identifier (e.g., official/security/1password).".to_string(),
        ));
    }

    let normalized = canonicalize_official_skill_dir(req.trim_start_matches("official/"));
    if normalized.is_empty() {
        return Err(AgentError::Config(
            "Missing official skill identifier (e.g., official/security/1password).".to_string(),
        ));
    }

    let branch = github_default_branch(client, OFFICIAL_SKILLS_REPO).await?;
    let tree = github_repo_tree(client, OFFICIAL_SKILLS_REPO, &branch).await?;
    let has_skill_dir = |dir: &str| -> bool {
        let target = format!("{}/SKILL.md", dir.trim_matches('/'));
        tree.iter()
            .any(|entry| entry.kind == "blob" && entry.path == target)
    };

    let mut candidate_queries = vec![
        req.to_string(),
        normalized.clone(),
        format!("official/{}", normalized),
    ];
    let basename = normalized
        .split('/')
        .next_back()
        .unwrap_or(normalized.as_str())
        .to_string();
    if !basename.is_empty() {
        candidate_queries.push(basename);
    }
    candidate_queries.sort();
    candidate_queries.dedup();

    for query in candidate_queries {
        if let Ok(record) = resolve_skill_via_registry_index(client, &query, Some("official")).await
        {
            if let RegistryInstallSource::GitHub(source) = record.install_source {
                let mut candidates = official_skill_path_candidates(&source.skill_dir);
                for c in official_skill_path_candidates(&normalized) {
                    if !candidates.iter().any(|existing| existing == &c) {
                        candidates.push(c);
                    }
                }
                for candidate in candidates {
                    if has_skill_dir(&candidate) {
                        return Ok(ResolvedSkillSource {
                            repo: OFFICIAL_SKILLS_REPO.to_string(),
                            branch: branch.clone(),
                            skill_dir: candidate,
                        });
                    }
                }
            }
        }
    }

    for candidate in official_skill_path_candidates(&normalized) {
        if has_skill_dir(&candidate) {
            return Ok(ResolvedSkillSource {
                repo: OFFICIAL_SKILLS_REPO.to_string(),
                branch: branch.clone(),
                skill_dir: candidate,
            });
        }
    }

    Err(AgentError::Config(format!(
        "Official skill '{}' not found in upstream skills or optional-skills catalogs.",
        requested
    )))
}

pub(crate) fn canonicalize_official_skill_dir(path: &str) -> String {
    path.trim().trim_matches('/').to_string()
}

pub(crate) fn official_skill_path_candidates(path_like: &str) -> Vec<String> {
    let normalized = canonicalize_official_skill_dir(path_like);
    if normalized.is_empty() {
        return Vec::new();
    }
    if normalized.starts_with("skills/") || normalized.starts_with("optional-skills/") {
        return vec![normalized];
    }
    vec![
        format!("skills/{}", normalized),
        format!("optional-skills/{}", normalized),
    ]
}

// ---------------------------------------------------------------------------
// Skills.sh resolution
// ---------------------------------------------------------------------------

pub(crate) async fn resolve_skills_sh_source(
    client: &reqwest::Client,
    requested: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let req = requested.trim().trim_matches('/');
    if req.is_empty() {
        return Err(AgentError::Config(
            "Missing skills.sh skill identifier.".to_string(),
        ));
    }
    if let Some((repo, _, skill_dir)) = parse_explicit_github_skill(req) {
        let branch = github_default_branch(client, &repo).await?;
        return Ok(ResolvedSkillSource {
            repo,
            branch,
            skill_dir,
        });
    }

    if let Ok(resolved) = resolve_skill_via_registry_index(client, req, Some("skills.sh")).await {
        if let RegistryInstallSource::GitHub(source) = resolved.install_source {
            let branch = github_default_branch(client, &source.repo).await?;
            return Ok(ResolvedSkillSource { branch, ..source });
        }
    }

    let search_resp = client
        .get(SKILLS_SH_SEARCH_URL)
        .query(&[("q", req), ("limit", "20")])
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("skills.sh search request failed: {}", e)))?;
    if !search_resp.status().is_success() {
        let status = search_resp.status();
        let body = search_resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "skills.sh search failed ({}): {}",
            status, body
        )));
    }
    let payload = search_resp
        .json::<SkillsShSearchResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid skills.sh payload: {}", e)))?;
    let req_l = req.to_ascii_lowercase();
    for hit in payload.skills {
        let source = hit.source.trim();
        if source.is_empty() {
            continue;
        }
        let skill_id = if hit.skill_id.trim().is_empty() {
            hit.name.trim().to_string()
        } else {
            hit.skill_id.trim().to_string()
        };
        let repo = source.to_string();
        let branch = github_default_branch(client, &repo).await?;
        if let Ok(found) = resolve_skill_in_repo(client, &repo, &skill_id, Some("skills")).await {
            return Ok(found);
        }
        if let Ok(found) = resolve_skill_in_repo(client, &repo, &req_l, Some("skills")).await {
            return Ok(found);
        }
        if let Some((repo2, _, dir)) = parse_explicit_github_skill(&hit.id) {
            return Ok(ResolvedSkillSource {
                repo: repo2,
                branch,
                skill_dir: dir,
            });
        }
    }

    Err(AgentError::Config(format!(
        "Unable to resolve skills.sh skill '{}'.",
        requested
    )))
}

pub(crate) async fn search_skills_sh_registry(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, AgentError> {
    let capped_limit = limit.clamp(1, 50).to_string();
    let search_resp = client
        .get(SKILLS_SH_SEARCH_URL)
        .query(&[("q", query), ("limit", capped_limit.as_str())])
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("skills.sh search request failed: {}", e)))?;
    if !search_resp.status().is_success() {
        let status = search_resp.status();
        let body = search_resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "skills.sh search failed ({}): {}",
            status, body
        )));
    }
    let payload = search_resp
        .json::<SkillsShSearchResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid skills.sh payload: {}", e)))?;

    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for hit in payload.skills {
        let id = hit.id.trim();
        if id.is_empty() {
            continue;
        }
        let identifier = format!("skills.sh/{}", id);
        if !seen.insert(identifier.clone()) {
            continue;
        }
        let display_name = if hit.name.trim().is_empty() {
            id.to_string()
        } else {
            hit.name.trim().to_string()
        };
        out.push((display_name, identifier));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Fallback router
// ---------------------------------------------------------------------------

pub(crate) async fn resolve_install_via_fallback_router(
    client: &reqwest::Client,
    skill_name: &str,
    taps: &[String],
) -> Result<(ResolvedSkillSource, InstallFallbackSource), AgentError> {
    if let Ok(resolved) = resolve_skills_sh_source(client, skill_name).await {
        return Ok((resolved, InstallFallbackSource::SkillsSh));
    }
    let resolved = resolve_skill_via_taps(client, taps, skill_name).await?;
    Ok((resolved, InstallFallbackSource::Tap))
}

// ---------------------------------------------------------------------------
// Identifier parsers
// ---------------------------------------------------------------------------

pub(crate) fn parse_repo_skill_identifier(identifier: &str) -> Option<(String, String)> {
    let trimmed = identifier.trim().trim_start_matches("github/");
    let pieces: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    if pieces.len() < 3 {
        return None;
    }
    let repo = format!("{}/{}", pieces[0], pieces[1]);
    let skill_dir = pieces[2..].join("/");
    if skill_dir.is_empty() {
        None
    } else {
        Some((repo, skill_dir))
    }
}

pub(crate) fn canonicalize_skills_sh_identifier(identifier: &str) -> String {
    identifier
        .trim()
        .trim_start_matches("skills.sh/")
        .trim_start_matches("skills-sh/")
        .to_string()
}

// ---------------------------------------------------------------------------
// Install / Exec
// ---------------------------------------------------------------------------

pub(crate) async fn fetch_bundle_for_lock_entry(
    client: &reqwest::Client,
    entry: &SkillHubInstalledEntry,
    taps: &[String],
) -> Result<Vec<(String, Bytes)>, AgentError> {
    match entry.source.as_str() {
        "official" => {
            let resolved = resolve_official_skill_source(client, &entry.identifier).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "skills.sh" | "skills-sh" => {
            let id = canonicalize_skills_sh_identifier(&entry.identifier);
            let resolved = resolve_skills_sh_source(client, &id).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "lobehub" => fetch_lobehub_skill_files(client, &entry.identifier).await,
        "clawhub" => fetch_clawhub_skill_files(client, &entry.identifier, None).await,
        "claude-marketplace" => {
            let resolved = resolve_claude_marketplace_skill(client, &entry.identifier).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "tap" => {
            if let Some((repo, skill_dir)) = parse_repo_skill_identifier(&entry.identifier) {
                let branch = github_default_branch(client, &repo).await?;
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            let resolved = resolve_skill_via_taps(client, taps, &entry.name).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "github" => {
            if let Some((repo, maybe_branch, skill_dir)) =
                parse_explicit_github_skill(&entry.identifier)
            {
                let branch = if let Some(branch) = maybe_branch {
                    branch
                } else {
                    github_default_branch(client, &repo).await?
                };
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            if let Some((repo, skill_dir)) = parse_repo_skill_identifier(&entry.identifier) {
                let branch = github_default_branch(client, &repo).await?;
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            let resolved =
                resolve_skill_in_repo(client, &entry.identifier, &entry.name, None).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        other => {
            if let Ok(hit) =
                resolve_skill_via_registry_index(client, &entry.identifier, Some(other)).await
            {
                return match hit.install_source {
                    RegistryInstallSource::GitHub(source) => {
                        let branch = github_default_branch(client, &source.repo).await?;
                        fetch_skill_files_from_github(
                            client,
                            &ResolvedSkillSource { branch, ..source },
                        )
                        .await
                    }
                    RegistryInstallSource::LobeHub { slug } => {
                        fetch_lobehub_skill_files(client, &slug).await
                    }
                    RegistryInstallSource::ClawHub { slug, version } => {
                        fetch_clawhub_skill_files(client, &slug, version.as_deref()).await
                    }
                };
            }
            Err(AgentError::Config(format!(
                "Unknown hub source '{}' for installed skill '{}'",
                entry.source, entry.name
            )))
        }
    }
}

pub(crate) fn install_skill_files(
    skills_dir: &std::path::Path,
    install_name: &str,
    files: &[(String, Bytes)],
    source: &str,
    force: bool,
) -> Result<std::path::PathBuf, AgentError> {
    skill_guard_enforce_bundle(install_name, source, files, force)?;

    std::fs::create_dir_all(skills_dir)
        .map_err(|e| AgentError::Io(format!("Failed to create skills dir: {}", e)))?;

    let target = skills_dir.join(install_name);
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .map_err(|e| AgentError::Io(format!("Failed to remove existing skill dir: {}", e)))?;
    }
    std::fs::create_dir_all(&target)
        .map_err(|e| AgentError::Io(format!("Failed to create skill dir: {}", e)))?;

    for (rel_path, bytes) in files {
        ensure_safe_relative_path(rel_path)?;
        let output = target.join(rel_path);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("Failed to create parent dirs: {}", e)))?;
        }
        std::fs::write(&output, bytes)
            .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", output.display(), e)))?;
    }

    let skill_md = target.join("SKILL.md");
    if !skill_md.exists() {
        return Err(AgentError::Config(format!(
            "Installed skill is missing SKILL.md at {}",
            skill_md.display()
        )));
    }

    Ok(target)
}

// ---------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------

pub(crate) fn skill_auto_bootstrap_enabled() -> bool {
    !std::env::var("HERMES_SKILL_AUTO_BOOTSTRAP")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
}

pub(crate) fn skill_bootstrap_force_confirmed() -> bool {
    std::env::var("HERMES_SKILL_BOOTSTRAP_ASSUME_YES")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        || std::env::var("HERMES_SKILL_BOOTSTRAP_FORCE")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
}

pub(crate) fn prompt_bootstrap_yes_no(prompt: &str, default_yes: bool) -> bool {
    use std::io::Write as _;
    print!("{}", prompt);
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return default_yes;
    }
    let answer = buf.trim().to_ascii_lowercase();
    if answer.is_empty() {
        return default_yes;
    }
    matches!(answer.as_str(), "y" | "yes")
}

pub(crate) fn push_bootstrap_command_if_present(commands: &mut Vec<String>, raw: Option<&str>) {
    if let Some(raw) = raw {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            commands.push(trimmed.to_string());
        }
    }
}

pub(crate) fn collect_bootstrap_commands_from_value(
    value: &serde_json::Value,
    out: &mut Vec<String>,
) {
    match value {
        serde_json::Value::String(s) => push_bootstrap_command_if_present(out, Some(s)),
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    push_bootstrap_command_if_present(out, Some(s));
                }
            }
        }
        serde_json::Value::Object(map) => {
            push_bootstrap_command_if_present(out, map.get("command").and_then(|v| v.as_str()));
            if let Some(commands) = map.get("commands") {
                collect_bootstrap_commands_from_value(commands, out);
            }
            if let Some(script) = map.get("script").and_then(|v| v.as_str()) {
                let script = script.trim();
                if !script.is_empty() {
                    if script.ends_with(".py") {
                        out.push(format!("python3 {}", script));
                    } else {
                        out.push(format!("bash {}", script));
                    }
                }
            }
            if let Some(scripts) = map.get("scripts").and_then(|v| v.as_array()) {
                for script in scripts {
                    if let Some(script) = script.as_str() {
                        let script = script.trim();
                        if script.is_empty() {
                            continue;
                        }
                        if script.ends_with(".py") {
                            out.push(format!("python3 {}", script));
                        } else {
                            out.push(format!("bash {}", script));
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn parse_skill_bootstrap_plan(
    files: &[(String, Bytes)],
) -> Result<Option<SkillBootstrapPlan>, AgentError> {
    let skill_md = files
        .iter()
        .find_map(|(path, bytes)| {
            if path == "SKILL.md" {
                Some(bytes)
            } else {
                None
            }
        })
        .ok_or_else(|| AgentError::Config("Installed skill payload is missing SKILL.md".into()))?;

    let content = std::str::from_utf8(skill_md)
        .map_err(|e| AgentError::Config(format!("Installed SKILL.md is not valid UTF-8: {}", e)))?;
    let (frontmatter, _body) = hermes_tools::tools::skill_utils::parse_frontmatter(content);

    let mut commands = Vec::new();
    for key in [
        "bootstrap",
        "setup",
        "install",
        "bootstrap_command",
        "setup_command",
        "install_command",
        "bootstrap_commands",
        "setup_commands",
        "install_commands",
    ] {
        if let Some(value) = frontmatter.get(key) {
            collect_bootstrap_commands_from_value(value, &mut commands);
        }
    }

    let mut dedup = HashSet::new();
    let normalized: Vec<String> = commands
        .into_iter()
        .filter_map(|cmd| {
            let trimmed = cmd.trim().to_string();
            if trimmed.is_empty() || !dedup.insert(trimmed.clone()) {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect();

    if normalized.is_empty() {
        Ok(None)
    } else {
        Ok(Some(SkillBootstrapPlan {
            commands: normalized,
        }))
    }
}

pub(crate) fn is_allowed_bootstrap_executable(executable: &str) -> bool {
    let normalized = executable
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(executable)
        .trim()
        .to_ascii_lowercase();
    SKILL_BOOTSTRAP_ALLOWED_EXECUTABLES
        .iter()
        .any(|allowed| *allowed == normalized)
}

pub(crate) fn parse_bootstrap_command(raw: &str) -> Result<ParsedBootstrapCommand, AgentError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AgentError::Config(
            "Bootstrap command cannot be empty".to_string(),
        ));
    }
    if trimmed.len() > 2048 {
        return Err(AgentError::Config(
            "Bootstrap command is too long (>2048 bytes)".to_string(),
        ));
    }

    // Deliberately block shell control operators and substitutions.
    let forbidden = Regex::new(r"[`\n\r;]|&&|\|\||\||>>?|<<?|\$\(").expect("valid regex");
    if forbidden.is_match(trimmed) {
        return Err(AgentError::Config(format!(
            "Blocked bootstrap command (contains forbidden shell operators): {}",
            trimmed
        )));
    }

    let mut tokens = shlex::split(trimmed).ok_or_else(|| {
        AgentError::Config(format!(
            "Unable to parse bootstrap command safely: {}",
            trimmed
        ))
    })?;
    if tokens.is_empty() {
        return Err(AgentError::Config(
            "Bootstrap command parsed to no executable".to_string(),
        ));
    }

    let executable = tokens.remove(0);
    if executable.contains('/') || executable.contains('\\') {
        let path = Path::new(&executable);
        if path.is_absolute() {
            return Err(AgentError::Config(format!(
                "Bootstrap executable must be relative (got absolute path): {}",
                executable
            )));
        }
        ensure_safe_relative_path(&executable)?;
        if executable.ends_with(".sh") {
            let mut args = vec![executable];
            args.extend(tokens);
            return Ok(ParsedBootstrapCommand {
                display: trimmed.to_string(),
                executable: "bash".to_string(),
                args,
            });
        }
        if executable.ends_with(".py") {
            let mut args = vec![executable];
            args.extend(tokens);
            return Ok(ParsedBootstrapCommand {
                display: trimmed.to_string(),
                executable: "python3".to_string(),
                args,
            });
        }
    } else if !is_allowed_bootstrap_executable(&executable) {
        return Err(AgentError::Config(format!(
            "Bootstrap executable '{}' is not in the allowlist",
            executable
        )));
    }

    Ok(ParsedBootstrapCommand {
        display: trimmed.to_string(),
        executable,
        args: tokens,
    })
}

pub(crate) async fn execute_bootstrap_command(
    skill_dir: &Path,
    command: &ParsedBootstrapCommand,
) -> Result<(), AgentError> {
    let exec_path = if command.executable.contains('/') || command.executable.contains('\\') {
        skill_dir.join(&command.executable)
    } else {
        PathBuf::from(&command.executable)
    };

    let mut process = tokio::process::Command::new(&exec_path);
    process
        .args(&command.args)
        .current_dir(skill_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = process.output().await.map_err(|e| {
        AgentError::Io(format!(
            "Failed to execute bootstrap command '{}': {}",
            command.display, e
        ))
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        if !stdout.is_empty() {
            println!(
                "    stdout: {}",
                stdout.lines().take(3).collect::<Vec<_>>().join(" | ")
            );
        }
        Ok(())
    } else {
        Err(AgentError::Config(format!(
            "Bootstrap command failed (exit={}): {}\n{}\n{}",
            output.status,
            command.display,
            if stdout.is_empty() { "" } else { "stdout:" },
            if stdout.is_empty() {
                stderr
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{}\nstderr:\n{}", stdout, stderr)
            }
        )))
    }
}

pub(crate) async fn maybe_run_skill_bootstrap(
    install_name: &str,
    skill_dir: &Path,
    files: &[(String, Bytes)],
) -> Result<(), AgentError> {
    if !skill_auto_bootstrap_enabled() {
        println!("Skill bootstrap skipped: HERMES_SKILL_AUTO_BOOTSTRAP=0.");
        return Ok(());
    }

    let Some(plan) = parse_skill_bootstrap_plan(files)? else {
        return Ok(());
    };

    let mut runnable: Vec<(ParsedBootstrapCommand, hermes_tools::ApprovalDecision)> = Vec::new();
    let mut blocked: Vec<(String, String)> = Vec::new();
    for raw in plan.commands {
        match parse_bootstrap_command(&raw) {
            Ok(parsed) => {
                let decision = hermes_tools::check_approval(&parsed.display);
                if matches!(decision, hermes_tools::ApprovalDecision::Denied) {
                    blocked.push((
                        parsed.display,
                        "blocked by command approval policy".to_string(),
                    ));
                } else {
                    runnable.push((parsed, decision));
                }
            }
            Err(err) => blocked.push((raw, err.to_string())),
        }
    }

    if runnable.is_empty() && blocked.is_empty() {
        return Ok(());
    }

    println!(
        "Detected bootstrap plan for '{}': {} runnable command(s), {} blocked.",
        install_name,
        runnable.len(),
        blocked.len()
    );
    for (cmd, reason) in &blocked {
        println!("  - blocked: `{}` ({})", cmd, reason);
    }
    if runnable.is_empty() {
        return Ok(());
    }

    let has_confirm = runnable.iter().any(|(_, decision)| {
        matches!(
            decision,
            hermes_tools::ApprovalDecision::RequiresConfirmation
        )
    });
    let force_yes = skill_bootstrap_force_confirmed();
    if has_confirm && !force_yes {
        let proceed = prompt_bootstrap_yes_no(
            "Run bootstrap commands that require installer confirmation now? [Y/n]: ",
            true,
        );
        if !proceed {
            println!("Skipped bootstrap execution.");
            return Ok(());
        }
    }

    for (command, decision) in runnable {
        if matches!(
            decision,
            hermes_tools::ApprovalDecision::RequiresConfirmation
        ) && !force_yes
        {
            println!("  • running (confirmed): {}", command.display);
        } else if matches!(decision, hermes_tools::ApprovalDecision::Approved) {
            println!("  • running: {}", command.display);
        } else if !force_yes {
            println!("  • skipped: {} (confirmation required)", command.display);
            continue;
        } else {
            println!("  • running (forced): {}", command.display);
        }
        execute_bootstrap_command(skill_dir, &command).await?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tap management
// ---------------------------------------------------------------------------

pub(crate) fn normalize_tap_path_for_storage(path: &str) -> String {
    let normalized = path.trim_matches('/');
    if normalized.is_empty() {
        String::new()
    } else {
        format!("{}/", normalized)
    }
}

pub(crate) fn tap_object_to_string(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    if let Some(url) = obj
        .get("url")
        .and_then(|u| u.as_str())
        .or_else(|| obj.get("tap").and_then(|u| u.as_str()))
    {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let repo = obj.get("repo").and_then(|v| v.as_str())?;
    let repo = repo.trim().trim_matches('/');
    if repo.is_empty() {
        return None;
    }
    let path = obj
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("skills/")
        .trim()
        .trim_matches('/');
    if path.is_empty() {
        Some(format!("https://github.com/{}::", repo))
    } else {
        Some(format!("https://github.com/{}::{}", repo, path))
    }
}

pub(crate) fn tap_string_to_object(tap: &str) -> serde_json::Value {
    if let Some(spec) = parse_skill_tap_spec(tap) {
        let mut obj = serde_json::Map::new();
        obj.insert("repo".to_string(), serde_json::Value::String(spec.repo));
        obj.insert(
            "path".to_string(),
            serde_json::Value::String(normalize_tap_path_for_storage(&spec.path)),
        );
        obj.insert(
            "url".to_string(),
            serde_json::Value::String(tap.to_string()),
        );
        serde_json::Value::Object(obj)
    } else {
        serde_json::json!({ "url": tap })
    }
}

pub(crate) fn read_skill_taps(path: &std::path::Path) -> Vec<String> {
    if !path.exists() {
        return Vec::new();
    }
    let content = std::fs::read_to_string(path).unwrap_or_else(|_| "[]".to_string());
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&content);
    let Ok(value) = parsed else {
        return Vec::new();
    };
    match value {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        serde_json::Value::Object(map) => {
            let taps = map.get("taps").cloned().unwrap_or(serde_json::Value::Null);
            match taps {
                serde_json::Value::Array(arr) => arr
                    .into_iter()
                    .filter_map(|item| match item {
                        serde_json::Value::String(s) => Some(s),
                        serde_json::Value::Object(obj) => tap_object_to_string(&obj),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

pub(crate) fn subscription_entry_to_source(entry: &serde_json::Value) -> Option<String> {
    match entry {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        serde_json::Value::Object(obj) => {
            let source = obj
                .get("source")
                .and_then(|v| v.as_str())
                .or_else(|| obj.get("tap").and_then(|v| v.as_str()))
                .or_else(|| obj.get("url").and_then(|v| v.as_str()))?;
            let trimmed = source.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        _ => None,
    }
}

pub(crate) fn read_skill_subscriptions(path: &std::path::Path) -> Vec<String> {
    if !path.exists() {
        return Vec::new();
    }
    let content = std::fs::read_to_string(path).unwrap_or_else(|_| "[]".to_string());
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&content);
    let Ok(value) = parsed else {
        return Vec::new();
    };
    match value {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(subscription_entry_to_source)
            .collect(),
        serde_json::Value::Object(map) => {
            let subscriptions = map
                .get("subscriptions")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            match subscriptions {
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(subscription_entry_to_source)
                    .collect(),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

pub(crate) fn write_skill_taps(path: &std::path::Path, taps: &[String]) -> Result<(), AgentError> {
    let serialized_taps: Vec<serde_json::Value> =
        taps.iter().map(|tap| tap_string_to_object(tap)).collect();
    let payload = serde_json::json!({
        "taps": serialized_taps
    });
    let json =
        serde_json::to_string_pretty(&payload).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, format!("{}\n", json)).map_err(|e| AgentError::Io(e.to_string()))?;
    Ok(())
}

pub(crate) fn merged_skill_taps(custom_taps: &[String]) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();
    for tap in DEFAULT_SKILL_TAPS {
        merged.push((*tap).to_string());
    }
    for tap in custom_taps {
        if !merged.iter().any(|existing| existing == tap) {
            merged.push(tap.clone());
        }
    }
    merged
}

pub(crate) fn subscription_source_to_tap(source: &str) -> Option<String> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://github.com/") || lower.starts_with("http://github.com/") {
        return parse_skill_tap_spec(trimmed).map(|_| trimmed.to_string());
    }
    if lower.contains("://") {
        return None;
    }
    if let Some((prefix, _)) = trimmed.split_once('/') {
        let p = prefix.trim().to_ascii_lowercase();
        if matches!(
            p.as_str(),
            "official" | "skills.sh" | "lobehub" | "clawhub" | "claude-marketplace" | "github"
        ) {
            return None;
        }
    }
    parse_skill_tap_spec(trimmed).map(|_| trimmed.to_string())
}

pub(crate) fn effective_skill_taps(
    taps_file: &std::path::Path,
    subscriptions_file: &std::path::Path,
) -> Vec<String> {
    let custom_taps = read_skill_taps(taps_file);
    let mut merged = merged_skill_taps(&custom_taps);
    for sub in read_skill_subscriptions(subscriptions_file) {
        // Subscriptions may include non-tap registries; only include values that
        // can be interpreted as GitHub tap specs.
        let Some(tap) = subscription_source_to_tap(&sub) else {
            continue;
        };
        if !merged.iter().any(|existing| existing == &tap) {
            merged.push(tap);
        }
    }
    merged
}
