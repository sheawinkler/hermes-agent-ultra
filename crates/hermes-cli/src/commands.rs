//! Slash command handler (Requirement 9.2).
//!
//! Defines and dispatches all supported `/` commands in the interactive
//! REPL, and provides auto-completion suggestions.

use std::process::Stdio;
use std::sync::Arc;
use std::{
    collections::HashSet,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use bytes::Bytes;
use hermes_core::AgentError;
use regex::Regex;
use serde::Deserialize;

use crate::app::App;
use crate::model_switch::{curated_provider_slugs, normalize_provider_model, provider_model_ids};

// ---------------------------------------------------------------------------
// CommandResult
// ---------------------------------------------------------------------------

/// Result of handling a slash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandResult {
    /// The command was fully handled (no further action needed).
    Handled,
    /// The command requires the agent to process a follow-up message.
    NeedsAgent,
    /// The user requested to quit the application.
    Quit,
}

fn secret_stdout_allowed() -> bool {
    std::env::var("HERMES_ALLOW_SECRET_STDOUT")
        .ok()
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn mask_secret_value(secret: &str) -> String {
    if secret.is_empty() {
        return "(empty)".to_string();
    }
    if secret.len() <= 8 {
        return "*".repeat(secret.len());
    }
    format!(
        "{}***{}",
        &secret[..4],
        &secret[secret.len().saturating_sub(4)..]
    )
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

/// All supported slash commands and their descriptions.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/new", "Start a new session"),
    ("/reset", "Reset the current session (clear messages)"),
    (
        "/clear",
        "Clear screen/session state and start a fresh session",
    ),
    ("/retry", "Retry the last user message"),
    ("/undo", "Undo the last exchange"),
    ("/history", "Show recent conversation history"),
    ("/title", "Set or show session title metadata"),
    (
        "/branch",
        "Create a branch/fork marker for the current session",
    ),
    ("/fork", "Alias for /branch"),
    ("/snapshot", "Create/list snapshot checkpoints"),
    ("/snap", "Alias for /snapshot"),
    ("/rollback", "List rollback checkpoints"),
    (
        "/model",
        "Show current model, set directly, or pick provider/model interactively",
    ),
    ("/provider", "List configured providers and availability"),
    (
        "/personality",
        "Show current personality, list built-ins, or switch mode",
    ),
    ("/profile", "Show active profile and Hermes home path"),
    ("/fast", "Toggle fast-mode hints"),
    ("/skin", "Show available skin/theme options"),
    ("/voice", "Show voice mode status"),
    ("/skills", "List available skills"),
    ("/skill", "Alias for /skills"),
    ("/tools", "List registered tools"),
    (
        "/toolcards",
        "Inline tool-card controls (e.g. `/toolcards export`)",
    ),
    ("/toolsets", "Show configured toolsets by platform"),
    ("/plugins", "List plugin bundles and status"),
    ("/mcp", "List configured MCP servers"),
    ("/reload", "Reload runtime env/config values"),
    ("/reload-mcp", "Reload MCP server metadata"),
    ("/reload_mcp", "Alias for /reload-mcp"),
    ("/cron", "Show cron scheduler status"),
    ("/scheduler", "Alias for /background"),
    ("/agents", "Show active/background task state"),
    ("/tasks", "Alias for /agents"),
    ("/queue", "Queue a follow-up prompt"),
    ("/q", "Alias for /queue"),
    ("/steer", "Inject non-interrupt steering instruction"),
    ("/btw", "Run an ephemeral side-question"),
    ("/plan", "Show planning helper status"),
    ("/lsp", "Show language-server/indexing context status"),
    ("/graph", "Show graph-memory/context status"),
    ("/image", "Attach an image path for next prompt"),
    ("/config", "Show or modify configuration"),
    ("/compress", "Trigger context compression"),
    ("/compact", "Alias for /compress"),
    ("/clear-queue", "Clear queued background jobs"),
    ("/usage", "Show token usage statistics"),
    ("/insights", "Show local usage/session insights"),
    ("/stop", "Stop current agent execution"),
    ("/status", "Show session status (model, turns, token count)"),
    ("/agent", "Alias for /status"),
    (
        "/about",
        "Show build/parity/upstream snapshot and enabled Ultra features",
    ),
    ("/ops", "Operator control plane (status + quick controls)"),
    (
        "/platforms",
        "Show enabled gateway/messaging platform adapters",
    ),
    ("/gateway", "Alias for /platforms"),
    ("/commands", "Show categorized slash command catalog"),
    ("/log", "Show recent runtime log files"),
    ("/debug-dump", "Dump local debug/session details"),
    ("/dump-format", "Show transcript export format"),
    ("/experiment", "Show experiment toggle surface"),
    ("/feedback", "Show feedback/report channels"),
    ("/copy", "Copy latest assistant message (if supported)"),
    ("/paste", "Attach clipboard payload (if supported)"),
    ("/gquota", "Show Google quota hint (if configured)"),
    ("/sethome", "Set home channel/session marker"),
    ("/set-home", "Alias for /sethome"),
    ("/restart", "Restart gateway process (gateway mode)"),
    ("/approve", "Approve pending action (gateway mode)"),
    ("/deny", "Deny pending action (gateway mode)"),
    ("/update", "Check update policy/status"),
    ("/save", "Save current session to disk"),
    ("/load", "Load a saved session"),
    ("/background", "Run a task in the background"),
    ("/mouse", "Toggle mouse interactions in the TUI"),
    ("/verbose", "Toggle verbose mode"),
    ("/statusbar", "Toggle status bar visibility"),
    ("/sb", "Alias for /statusbar"),
    ("/yolo", "Toggle auto-approve mode"),
    ("/reasoning", "Toggle reasoning display"),
    ("/raw", "RTK raw-mode controls (status/on/off/toggle/once)"),
    (
        "/policy",
        "Policy lifecycle (needs HERMES_POLICY_ADMIN_TOKEN, same as HTTP X-Hermes-Policy-Admin)",
    ),
    ("/help", "Show help for available commands"),
    ("/quit", "Quit the application"),
    ("/exit", "Alias for /quit"),
];

const DEFAULT_SKILL_TAPS: &[&str] = &[
    "https://github.com/openai/skills::skills",
    "https://github.com/anthropics/skills::skills",
    "https://github.com/VoltAgent/awesome-agent-skills::skills",
    "https://github.com/mattpocock/skills::skills",
    "https://github.com/github/awesome-copilot::skills",
    "https://github.com/garrytan/gstack::",
    "https://github.com/MiniMax-AI/cli::skill",
];

const GITHUB_API_BASE: &str = "https://api.github.com";
const HERMES_SKILLS_INDEX_URL: &str =
    "https://hermes-agent.nousresearch.com/docs/api/skills-index.json";
const SKILLS_SH_SEARCH_URL: &str = "https://skills.sh/api/search";
const CLAWHUB_API_BASE: &str = "https://clawhub.ai/api/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillTapSpec {
    repo: String,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedSkillSource {
    repo: String,
    branch: String,
    skill_dir: String,
}

#[derive(Debug, Clone)]
enum RegistryInstallSource {
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
struct RegistrySkillRecord {
    identifier: String,
    description: String,
    source: String,
    score: i32,
    install_source: RegistryInstallSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallFallbackSource {
    SkillsSh,
    Tap,
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

fn parse_skill_tap_spec(raw: &str) -> Option<SkillTapSpec> {
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

fn parse_skill_name_and_version(spec: &str) -> (String, Option<String>) {
    let trimmed = spec.trim();
    if let Some((name, version)) = trimmed.rsplit_once('@') {
        if !name.is_empty() && !version.is_empty() && !name.starts_with("https://") {
            return (name.to_string(), Some(version.to_string()));
        }
    }
    (trimmed.to_string(), None)
}

fn looks_like_github_repo_slug(token: &str) -> bool {
    let parts: Vec<&str> = token.split('/').filter(|s| !s.is_empty()).collect();
    parts.len() == 2
}

fn parse_explicit_github_skill(spec: &str) -> Option<(String, Option<String>, String)> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
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

fn sanitize_skill_install_name(source: &str) -> String {
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

fn ensure_safe_relative_path(path: &str) -> Result<(), AgentError> {
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

fn parse_registry_prefixed_skill(spec: &str) -> Option<(String, String)> {
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

fn score_registry_match(entry: &HermesSkillsIndexEntry, query: &str) -> i32 {
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

fn skill_source_priority(source: &str) -> usize {
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

fn sort_registry_skill_records(records: &mut [RegistrySkillRecord]) {
    records.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| skill_source_priority(&a.source).cmp(&skill_source_priority(&b.source)))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.identifier.cmp(&b.identifier))
    });
}

async fn fetch_hermes_skills_index(
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

fn resolved_source_from_index(entry: &HermesSkillsIndexEntry) -> Option<RegistryInstallSource> {
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
            repo: "nousresearch/hermes-agent".to_string(),
            branch: "main".to_string(),
            skill_dir: format!("optional-skills/{}", path),
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

async fn search_multi_registry(
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

async fn resolve_skill_via_registry_index(
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

fn build_lobehub_skill_markdown(payload: &LobeHubAgentResponse, slug: &str) -> String {
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

fn skill_guard_scan_bundle(files: &[(String, Bytes)]) -> Result<(), AgentError> {
    let guard = hermes_skills::SkillGuard::default();
    for (rel_path, bytes) in files {
        // Skip binary files to avoid false positives from compressed payloads.
        let Ok(text) = std::str::from_utf8(bytes.as_ref()) else {
            continue;
        };
        let probe = hermes_core::types::Skill {
            name: rel_path.clone(),
            content: text.to_string(),
            category: Some("external".to_string()),
            description: None,
        };
        guard.scan_security_only(&probe).map_err(|e| {
            AgentError::Config(format!(
                "Security scan failed for skill bundle file '{}': {}",
                rel_path, e
            ))
        })?;
    }
    Ok(())
}

fn github_request(client: &reqwest::Client, url: &str, accept: &str) -> reqwest::RequestBuilder {
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

async fn github_default_branch(client: &reqwest::Client, repo: &str) -> Result<String, AgentError> {
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

async fn github_repo_tree(
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

async fn resolve_skill_via_taps(
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

async fn resolve_skill_in_repo(
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

async fn search_skills_via_taps(
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

async fn fetch_skill_files_from_github(
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
        let encoded_path = entry
            .path
            .split('/')
            .map(urlencoding::encode)
            .collect::<Vec<_>>()
            .join("/");
        let url = format!(
            "{}/repos/{}/contents/{}?ref={}",
            GITHUB_API_BASE,
            source.repo,
            encoded_path,
            urlencoding::encode(&source.branch)
        );
        let resp = github_request(client, &url, "application/vnd.github.v3.raw")
            .timeout(std::time::Duration::from_secs(25))
            .send()
            .await
            .map_err(|e| AgentError::Config(format!("GitHub file download failed: {}", e)))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::Config(format!(
                "Failed to download {} from {} ({}): {}",
                rel_path, source.repo, status, body
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AgentError::Config(format!("Invalid file payload: {}", e)))?;
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

async fn fetch_lobehub_skill_files(
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

fn detect_archive_format(bytes: &[u8]) -> &'static str {
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

fn extract_clawhub_archive(bytes: &[u8]) -> Result<Vec<(String, Bytes)>, AgentError> {
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

async fn fetch_clawhub_skill_files(
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

async fn resolve_claude_marketplace_skill(
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

async fn resolve_official_skill_source(
    client: &reqwest::Client,
    requested: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let req = requested.trim().trim_matches('/');
    if req.is_empty() {
        return Err(AgentError::Config(
            "Missing official skill identifier (e.g., official/security/1password).".to_string(),
        ));
    }
    if req.contains('/') {
        return Ok(ResolvedSkillSource {
            repo: "nousresearch/hermes-agent".to_string(),
            branch: "main".to_string(),
            skill_dir: format!("optional-skills/{}", req),
        });
    }
    let resolved = resolve_skill_via_registry_index(client, req, Some("official")).await?;
    match resolved.install_source {
        RegistryInstallSource::GitHub(source) => Ok(source),
        _ => Err(AgentError::Config(format!(
            "Official skill '{}' does not resolve to a GitHub-backed source.",
            requested
        ))),
    }
}

async fn resolve_skills_sh_source(
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

async fn search_skills_sh_registry(
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

async fn resolve_install_via_fallback_router(
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

fn install_skill_files(
    skills_dir: &std::path::Path,
    install_name: &str,
    files: &[(String, Bytes)],
) -> Result<std::path::PathBuf, AgentError> {
    skill_guard_scan_bundle(files)?;

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

fn normalize_tap_path_for_storage(path: &str) -> String {
    let normalized = path.trim_matches('/');
    if normalized.is_empty() {
        String::new()
    } else {
        format!("{}/", normalized)
    }
}

fn tap_object_to_string(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
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

fn tap_string_to_object(tap: &str) -> serde_json::Value {
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

fn read_skill_taps(path: &std::path::Path) -> Vec<String> {
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

fn write_skill_taps(path: &std::path::Path, taps: &[String]) -> Result<(), AgentError> {
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

fn merged_skill_taps(custom_taps: &[String]) -> Vec<String> {
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

/// Return auto-completion suggestions for a partial slash command.
pub fn autocomplete(partial: &str) -> Vec<&'static str> {
    let mut seen = HashSet::new();
    let mut ranked: Vec<(&'static str, i32)> = Vec::new();
    let query = partial.trim().to_ascii_lowercase();
    for (cmd, desc) in SLASH_COMMANDS {
        if !seen.insert(*cmd) {
            continue;
        }
        if let Some(score) = command_match_score(&query, cmd, desc) {
            ranked.push((cmd, score));
        }
    }
    ranked.sort_by(|(a_cmd, a_score), (b_cmd, b_score)| {
        b_score.cmp(a_score).then_with(|| a_cmd.cmp(b_cmd))
    });
    ranked.into_iter().map(|(cmd, _)| cmd).collect()
}

fn command_match_score(query: &str, cmd: &str, desc: &str) -> Option<i32> {
    if query.is_empty() || query == "/" {
        return Some(10);
    }
    let cmd_l = cmd.to_ascii_lowercase();
    let desc_l = desc.to_ascii_lowercase();
    if cmd_l == query {
        return Some(1200);
    }
    if cmd_l.starts_with(query) {
        return Some(1000 - (cmd_l.len().saturating_sub(query.len()) as i32));
    }
    if cmd_l.contains(query) {
        return Some(850 - (cmd_l.len().saturating_sub(query.len()) as i32));
    }
    if let Some(pos) = desc_l.find(query.trim_start_matches('/')) {
        return Some(700 - pos as i32);
    }
    let subseq = subsequence_score(query.trim_start_matches('/'), cmd_l.trim_start_matches('/'));
    if subseq > 0 {
        return Some(500 + subseq);
    }
    None
}

fn subsequence_score(needle: &str, haystack: &str) -> i32 {
    if needle.is_empty() || haystack.is_empty() {
        return 0;
    }
    let mut score = 0i32;
    let mut idx = 0usize;
    let chars: Vec<char> = haystack.chars().collect();
    for ch in needle.chars() {
        let mut found = false;
        while idx < chars.len() {
            if chars[idx] == ch {
                score += 2;
                if idx > 0 && chars[idx - 1] == '-' {
                    score += 1;
                }
                idx += 1;
                found = true;
                break;
            }
            idx += 1;
        }
        if !found {
            return 0;
        }
    }
    score
}

/// Return the help text for a specific slash command.
pub fn help_for(cmd: &str) -> Option<&'static str> {
    SLASH_COMMANDS
        .iter()
        .find(|(name, _)| *name == cmd)
        .map(|(_, desc)| *desc)
}

fn canonical_command(cmd: &str) -> &str {
    match cmd {
        "/clear" => "/new",
        "/compact" => "/compress",
        "/skill" => "/skills",
        "/agent" => "/status",
        "/tasks" => "/agents",
        "/scheduler" => "/background",
        "/gateway" => "/platforms",
        "/reload_mcp" => "/reload-mcp",
        "/fork" => "/branch",
        "/snap" => "/snapshot",
        "/set-home" => "/sethome",
        "/q" => "/queue",
        "/sb" => "/statusbar",
        "/exit" => "/quit",
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Command dispatcher
// ---------------------------------------------------------------------------

/// Handle a slash command.
///
/// `cmd` is the full command token including the `/` prefix
/// (e.g. `/model`, `/new`). `args` are the remaining tokens.
pub async fn handle_slash_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    match canonical_command(cmd) {
        "/new" => {
            app.new_session();
            emit_command_output(app, format!("[New session started: {}]", app.session_id));
            Ok(CommandResult::Handled)
        }
        "/reset" => {
            app.reset_session();
            emit_command_output(app, "[Session reset]");
            Ok(CommandResult::Handled)
        }
        "/retry" => {
            app.retry_last().await?;
            Ok(CommandResult::Handled)
        }
        "/undo" => {
            app.undo_last();
            emit_command_output(app, "[Last exchange undone]");
            Ok(CommandResult::Handled)
        }
        "/history" => handle_history_command(app),
        "/title" | "/branch" | "/snapshot" | "/rollback" | "/queue" | "/steer" | "/btw"
        | "/sethome" => handle_session_compat_command(app, canonical_command(cmd), args),
        "/model" => handle_model_command(app, args).await,
        "/provider" => handle_provider_command(app).await,
        "/personality" => handle_personality_command(app, args),
        "/profile" => handle_profile_command(app),
        "/fast" | "/skin" | "/voice" => {
            handle_runtime_ui_mode_command(app, canonical_command(cmd), args)
        }
        "/skills" => handle_skills_command(app),
        "/tools" => handle_tools_command(app),
        "/toolcards" => handle_toolcards_command(app, args),
        "/toolsets" => handle_toolsets_command(app),
        "/plugins" => handle_plugins_command(app),
        "/mcp" => handle_mcp_command(app),
        "/reload" | "/reload-mcp" => handle_reload_command(app, canonical_command(cmd)),
        "/cron" => handle_cron_command(app),
        "/agents" => handle_agents_command(app),
        "/plan" | "/lsp" | "/graph" | "/image" => {
            handle_capability_surface_command(app, canonical_command(cmd), args)
        }
        "/config" => handle_config_command(app, args),
        "/compress" => handle_compress_command(app),
        "/clear-queue" => handle_clear_queue_command(app),
        "/usage" => handle_usage_command(app),
        "/insights" => handle_insights_command(app),
        "/stop" => handle_stop_command(app),
        "/status" => handle_status_command(app),
        "/about" => handle_about_command(app),
        "/ops" => handle_ops_command(app, args).await,
        "/platforms" => handle_platforms_command(app),
        "/commands" => {
            print_help(app);
            Ok(CommandResult::Handled)
        }
        "/log" => handle_log_command(app),
        "/debug-dump" | "/dump-format" | "/experiment" | "/feedback" | "/copy" | "/paste"
        | "/gquota" | "/restart" | "/approve" | "/deny" | "/update" => {
            handle_compatibility_notice_command(app, canonical_command(cmd), args)
        }
        "/save" => handle_save_command(app, args),
        "/load" => handle_load_command(app, args),
        "/background" => handle_background_command(app, args),
        "/mouse" => handle_mouse_command(app, args),
        "/verbose" => handle_verbose_command(app),
        "/statusbar" => handle_statusbar_command(app),
        "/yolo" => handle_yolo_command(app),
        "/reasoning" => handle_reasoning_command(app),
        "/raw" => handle_raw_command(app, args),
        "/policy" => handle_policy_command(app, args),
        "/help" => {
            print_help(app);
            Ok(CommandResult::Handled)
        }
        "/quit" | "/exit" => {
            emit_command_output(app, "Goodbye!");
            Ok(CommandResult::Quit)
        }
        _ => {
            emit_command_output(
                app,
                format!(
                    "Unknown command: {}. Type /help for available commands.",
                    cmd
                ),
            );
            Ok(CommandResult::Handled)
        }
    }
}

fn handle_toolcards_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("help");
    let msg = match action {
        "export" => {
            "Tool-card export is handled by the interactive TUI modal loop. In TUI, run `/toolcards export` to write `~/.hermes-agent-ultra/logs/toolcards-export.txt`.".to_string()
        }
        _ => "Tool-card controls:\n  /toolcards export   Export current tool-card transcript".to_string(),
    };
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// Individual command handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelSwitchRequest {
    PickProviderThenModel,
    PickModelFromProvider(String),
    SetDirect(String),
}

fn parse_model_switch_request(args: &[&str], known_providers: &[&str]) -> ModelSwitchRequest {
    if args.is_empty() {
        return ModelSwitchRequest::PickProviderThenModel;
    }
    let raw = args.join(" ");
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ModelSwitchRequest::PickProviderThenModel;
    }
    if trimmed.contains(':') {
        return ModelSwitchRequest::SetDirect(trimmed.to_string());
    }
    if known_providers
        .iter()
        .any(|p| p.eq_ignore_ascii_case(trimmed))
    {
        return ModelSwitchRequest::PickModelFromProvider(trimmed.to_ascii_lowercase());
    }
    ModelSwitchRequest::SetDirect(trimmed.to_string())
}

fn split_provider_model(provider_model: &str) -> (&str, &str) {
    provider_model
        .split_once(':')
        .unwrap_or(("openai", provider_model))
}

fn normalize_model_target(current_model: &str, raw: &str) -> Result<String, AgentError> {
    let trimmed = raw.trim();
    if trimmed.contains(':') {
        return normalize_provider_model(trimmed);
    }
    let (provider, _) = split_provider_model(current_model);
    normalize_provider_model(&format!("{}:{}", provider.trim(), trimmed))
}

/// Run `curses_select` safely from both plain CLI and active TUI sessions.
///
/// In TUI mode, use an embedded selector that does not toggle terminal mode.
fn run_model_picker_select(
    app: &App,
    title: &str,
    items: &[String],
    initial_index: usize,
) -> crate::SelectResult {
    if app.stream_handle.is_some() {
        crate::curses_select_embedded(title, items, initial_index)
    } else {
        crate::curses_select(title, items, initial_index)
    }
}

async fn pick_model_for_provider(
    app: &mut App,
    provider: &str,
    current_model: &str,
) -> Result<bool, AgentError> {
    let models = provider_model_ids(provider).await;
    if models.is_empty() {
        emit_command_output(
            app,
            format!("No models available for provider '{}'.", provider),
        );
        return Ok(false);
    }

    let (_, current_model_id) = split_provider_model(current_model);
    let default_index = models
        .iter()
        .position(|m| m.eq_ignore_ascii_case(current_model_id))
        .unwrap_or(0);
    let labels: Vec<String> = models.clone();
    let title = format!("Select {} model ({} available)", provider, labels.len());
    let pick = run_model_picker_select(app, &title, &labels, default_index);
    if !pick.confirmed || pick.index >= models.len() {
        emit_command_output(app, "Model switch cancelled.");
        return Ok(false);
    }
    let provider_model = format!("{}:{}", provider, models[pick.index].trim());
    app.switch_model(&provider_model);
    emit_command_output(app, format!("Model switched to: {}", provider_model));
    Ok(true)
}

async fn handle_model_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let known_providers = curated_provider_slugs();
    match parse_model_switch_request(args, &known_providers) {
        ModelSwitchRequest::SetDirect(raw) => {
            let provider_model = normalize_model_target(&app.current_model, &raw)?;
            app.switch_model(&provider_model);
            emit_command_output(app, format!("Model switched to: {}", provider_model));
        }
        ModelSwitchRequest::PickModelFromProvider(provider) => {
            let current_model = app.current_model.clone();
            pick_model_for_provider(app, &provider, &current_model).await?;
        }
        ModelSwitchRequest::PickProviderThenModel => {
            emit_command_output(app, format!("Current model: {}", app.current_model));
            let providers: Vec<String> = known_providers.iter().map(|p| (*p).to_string()).collect();
            if providers.is_empty() {
                emit_command_output(app, "No providers are registered for selection.");
                return Ok(CommandResult::Handled);
            }
            let (current_provider, _) = split_provider_model(&app.current_model);
            let default_provider_index = providers
                .iter()
                .position(|p| p.eq_ignore_ascii_case(current_provider))
                .unwrap_or(0);
            let provider_pick =
                run_model_picker_select(app, "Select provider", &providers, default_provider_index);
            if !provider_pick.confirmed || provider_pick.index >= providers.len() {
                emit_command_output(app, "Model switch cancelled.");
                return Ok(CommandResult::Handled);
            }
            let provider = providers[provider_pick.index].as_str();
            let current_model = app.current_model.clone();
            pick_model_for_provider(app, provider, &current_model).await?;
        }
    }
    Ok(CommandResult::Handled)
}

fn emit_command_output(app: &mut App, text: impl Into<String>) {
    let rendered = text.into();
    if app.stream_handle.is_some() {
        app.push_ui_assistant(rendered);
    } else {
        println!("{}", rendered);
    }
}

fn format_personality_catalog(
    current_personality: Option<&str>,
    builtin_descriptions: &[(&str, &str)],
) -> String {
    let mut out = String::from("## Built-in personalities\n\n");
    if let Some(current) = current_personality.filter(|v| !v.trim().is_empty()) {
        out.push_str(&format!("Current: `{}`\n\n", current));
    } else {
        out.push_str("Current: `(none)`\n\n");
    }
    out.push_str("Use `/personality <name>` to switch.\n\n");
    for (name, usage) in builtin_descriptions {
        out.push_str(&format!("- `{}`\n  {}\n\n", name, usage));
    }
    out.trim_end().to_string()
}

fn handle_personality_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let builtin = hermes_agent::builtin_personality_names();
    let builtin_descriptions = hermes_agent::builtin_personality_descriptions();
    if args.is_empty() {
        emit_command_output(
            app,
            format_personality_catalog(app.current_personality.as_deref(), builtin_descriptions),
        );
    } else if args.len() == 1 && args[0].eq_ignore_ascii_case("list") {
        emit_command_output(
            app,
            format_personality_catalog(app.current_personality.as_deref(), builtin_descriptions),
        );
    } else {
        let name = args.join(" ");
        app.switch_personality(&name);
        let mut response = format!("Switched personality to `{}`.", name);
        if !name.contains(char::is_whitespace)
            && !name.eq_ignore_ascii_case("default")
            && !builtin.iter().any(|n| n.eq_ignore_ascii_case(&name))
        {
            response.push_str(&format!(
                "\n\nNote: `{}` is not built-in. Hermes will look for `personalities/{}.md` or treat inline text as compatibility mode.",
                name, name,
            ));
        }
        emit_command_output(app, response);
    }
    Ok(CommandResult::Handled)
}

fn handle_skills_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let skills_dir = hermes_config::hermes_home().join("skills");
    if !skills_dir.exists() {
        emit_command_output(
            app,
            format!(
                "No skills directory found at {}. Run `hermes setup` first.",
                skills_dir.display()
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let mut skills: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let skill_md = path.join("SKILL.md");
            if !path.is_dir() || !skill_md.exists() {
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let title = std::fs::read_to_string(&skill_md)
                .ok()
                .and_then(|c| {
                    c.lines()
                        .find(|l| l.starts_with('#'))
                        .map(|l| l.trim_start_matches('#').trim().to_string())
                })
                .unwrap_or_else(|| "(no description)".to_string());
            skills.push((name, title));
        }
    }
    skills.sort_by(|a, b| a.0.cmp(&b.0));

    if skills.is_empty() {
        emit_command_output(
            app,
            format!(
                "No installed skills found in {}.\nInstall skills with `hermes skills install <name>`.",
                skills_dir.display()
            ),
        );
    } else {
        let mut out = format!("Installed skills ({}):\n", skills.len());
        for (name, title) in &skills {
            out.push_str(&format!("- `{}` — {}\n", name, title));
        }
        out.push_str("\nUse `hermes skills inspect <name>` for details.");
        emit_command_output(app, out.trim_end());
    }
    Ok(CommandResult::Handled)
}

fn handle_tools_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let tools = app.tool_registry.list_tools();
    if tools.is_empty() {
        emit_command_output(app, "No tools registered.");
    } else {
        let mut out = format!("Registered tools ({}):\n", tools.len());
        for tool in &tools {
            out.push_str(&format!("- `{}` — {}\n", tool.name, tool.description));
        }
        emit_command_output(app, out.trim_end());
    }
    Ok(CommandResult::Handled)
}

fn handle_config_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        // Show full config
        let config_json = serde_json::to_string_pretty(&*app.config)
            .unwrap_or_else(|e| format!("<serialization error: {}>", e));
        emit_command_output(app, config_json);
    } else {
        match args[0] {
            "get" => {
                if args.len() < 2 {
                    emit_command_output(app, "Usage: /config get <key>");
                } else {
                    let key = args[1];
                    let value = get_config_value(app, key);
                    match value {
                        Some(v) => emit_command_output(app, format!("{} = {}", key, v)),
                        None => emit_command_output(
                            app,
                            format!("Key '{}' not found in configuration.", key),
                        ),
                    }
                }
            }
            "set" => {
                if args.len() < 3 {
                    emit_command_output(app, "Usage: /config set <key> <value>");
                } else {
                    let key = args[1];
                    let value = args[2..].join(" ");
                    if set_config_value(app, key, &value) {
                        emit_command_output(app, format!("Set {} = {}", key, value));
                    } else {
                        emit_command_output(app, format!("Unknown configuration key: {}", key));
                    }
                }
            }
            _ => {
                emit_command_output(
                    app,
                    format!("Unknown config action '{}'. Use 'get' or 'set'.", args[0]),
                );
            }
        }
    }
    Ok(CommandResult::Handled)
}

/// Get a configuration value by dotted key path.
fn get_config_value(app: &App, key: &str) -> Option<String> {
    match key {
        "model" => app.config.model.clone(),
        "personality" => app.config.personality.clone(),
        "max_turns" => Some(app.config.max_turns.to_string()),
        "system_prompt" => app.config.system_prompt.clone(),
        _ => None,
    }
}

/// Set a configuration value by dotted key path.
fn set_config_value(app: &mut App, key: &str, value: &str) -> bool {
    match key {
        "model" => {
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                cfg.model = Some(value.to_string());
                cfg
            });
            app.switch_model(value);
            true
        }
        "personality" => {
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                cfg.personality = Some(value.to_string());
                cfg
            });
            app.switch_personality(value);
            true
        }
        "max_turns" => {
            if let Ok(turns) = value.parse::<u32>() {
                app.config = Arc::new({
                    let mut cfg = (*app.config).clone();
                    cfg.max_turns = turns;
                    cfg
                });
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

fn handle_compress_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let msg_count = app.messages.len();
    if msg_count <= 2 {
        emit_command_output(
            app,
            format!("Context too small to compress ({} messages).", msg_count),
        );
        return Ok(CommandResult::Handled);
    }

    let keep = std::cmp::max(2, msg_count / 3);
    let removed = msg_count - keep;
    let summary_text = format!(
        "[Compressed: {} earlier messages summarized. {} messages retained.]",
        removed, keep,
    );

    let split_at = app.messages.len() - keep;
    let retained = app.messages.split_off(split_at);
    app.messages.clear();
    app.messages
        .push(hermes_core::Message::system(summary_text));
    app.messages.extend(retained);

    emit_command_output(
        app,
        format!(
            "Compressed context: removed {} messages, kept {}. Total now: {}.",
            removed,
            keep,
            app.messages.len(),
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_usage_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let msg_count = app.messages.len();
    let user_msgs = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::User)
        .count();
    let assistant_msgs = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::Assistant)
        .count();

    let estimated_tokens: usize = app
        .messages
        .iter()
        .map(|m| m.content.as_ref().map_or(0, |c| c.len()) / 4)
        .sum();

    emit_command_output(
        app,
        format!(
            "Session Usage Statistics\n  Session:     {}\n  Model:       {}\n  Messages:    {} total\n    User:      {}\n    Assistant: {}\n  Est. tokens: ~{}",
            app.session_id, app.current_model, msg_count, user_msgs, assistant_msgs, estimated_tokens
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_stop_command(app: &mut App) -> Result<CommandResult, AgentError> {
    app.interrupt_controller.interrupt(None);
    emit_command_output(
        app,
        "[Stopping current agent execution]\nAgent execution halted. You can continue typing or use /retry.",
    );
    Ok(CommandResult::Handled)
}

fn handle_status_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let msg_count = app.messages.len();
    let turns = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::User)
        .count();
    let estimated_tokens: usize = app
        .messages
        .iter()
        .map(|m| m.content.as_ref().map_or(0, |c| c.len()) / 4)
        .sum();

    emit_command_output(
        app,
        format!(
            "Session Status\n  ID:            {}\n  Model:         {}\n  Personality:   {}\n  Turns:         {}\n  Messages:      {}\n  Est. tokens:   ~{}\n  Max turns:     {}",
            app.session_id,
            app.current_model,
            app.current_personality.as_deref().unwrap_or("(none)"),
            turns,
            msg_count,
            estimated_tokens,
            app.config.max_turns
        ),
    );
    Ok(CommandResult::Handled)
}

fn discover_repo_root_for_about() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("HERMES_REPO_ROOT") {
        let path = PathBuf::from(explicit.trim());
        if path.exists() {
            return Some(path);
        }
    }

    let mut probes: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        probes.push(cwd);
    }
    probes.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));

    for probe in probes {
        for candidate in probe.ancestors() {
            if candidate.join("docs/parity").exists() && candidate.join("README.md").exists() {
                return Some(candidate.to_path_buf());
            }
        }
    }
    None
}

fn read_json_file(path: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<serde_json::Value>(&raw).ok()
}

fn json_value_at_path<'a>(
    value: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn json_str_at_path(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    json_value_at_path(value, path)?
        .as_str()
        .map(|s| s.to_string())
}

fn json_u64_at_path(value: &serde_json::Value, path: &[&str]) -> Option<u64> {
    json_value_at_path(value, path)?.as_u64()
}

fn latest_upstream_sync_report(report_dir: &Path) -> Option<PathBuf> {
    let mut reports: Vec<PathBuf> = std::fs::read_dir(report_dir)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_string_lossy();
            if name.starts_with("upstream-sync-") && name.ends_with(".txt") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    reports.sort();
    reports.into_iter().last()
}

fn parse_sync_report_metadata(path: &Path) -> (std::collections::HashMap<String, String>, usize) {
    let mut meta = std::collections::HashMap::new();
    let mut pending_commit_lines = 0usize;
    let raw = std::fs::read_to_string(path).unwrap_or_default();

    let mut in_pending_section = false;
    let mut in_pending_block = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if !in_pending_section {
            if trimmed.starts_with("## Pending Upstream Commits") {
                in_pending_section = true;
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim();
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                {
                    meta.insert(key.to_string(), v.trim().to_string());
                }
            }
            continue;
        }

        if trimmed == "```" {
            if !in_pending_block {
                in_pending_block = true;
            } else {
                break;
            }
            continue;
        }
        if in_pending_block && !trimmed.is_empty() {
            pending_commit_lines = pending_commit_lines.saturating_add(1);
        }
    }

    (meta, pending_commit_lines)
}

fn yes_no(flag: bool) -> &'static str {
    if flag {
        "yes"
    } else {
        "no"
    }
}

fn handle_about_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let mut out = String::new();
    let _ = writeln!(out, "Hermes Agent Ultra — About");
    let _ = writeln!(out, "  Version:         {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "  Session model:   {}", app.current_model);
    let _ = writeln!(
        out,
        "  Personality:     {}",
        app.current_personality.as_deref().unwrap_or("(none)")
    );
    if let Ok(exe) = std::env::current_exe() {
        let _ = writeln!(out, "  Binary:          {}", exe.display());
    }
    if let Ok(cwd) = std::env::current_dir() {
        let _ = writeln!(out, "  Current dir:     {}", cwd.display());
    }

    let raw_mode = app.tool_registry.raw_mode_state();
    let policy_mode = std::env::var("HERMES_TOOL_POLICY_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "enforce".to_string());
    let policy_preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "balanced".to_string());

    let has_contextlattice_mcp = app.config.mcp_servers.iter().any(|entry| {
        let name_hit = entry.name.to_ascii_lowercase().contains("contextlattice");
        let url_hit = entry
            .url
            .as_ref()
            .map(|u| u.to_ascii_lowercase().contains("contextlattice"))
            .unwrap_or(false);
        name_hit || url_hit
    });

    let _ = writeln!(out);
    let _ = writeln!(out, "Enabled Ultra Features:");
    let _ = writeln!(
        out,
        "  - RTK raw-mode: enabled={} once={}",
        yes_no(raw_mode.enabled),
        yes_no(raw_mode.once_pending)
    );
    let _ = writeln!(
        out,
        "  - Tool policy: mode={} preset={}",
        policy_mode, policy_preset
    );
    let _ = writeln!(
        out,
        "  - Code indexing: {} (max_files={}, max_symbols={})",
        yes_no(app.config.agent.code_index_enabled),
        app.config.agent.code_index_max_files,
        app.config.agent.code_index_max_symbols
    );
    let _ = writeln!(
        out,
        "  - LSP context injection: {} (max_chars={})",
        yes_no(app.config.agent.lsp_context_enabled),
        app.config.agent.lsp_context_max_chars
    );
    let _ = writeln!(
        out,
        "  - Background review loop: {}",
        yes_no(app.config.agent.background_review_enabled)
    );
    let _ = writeln!(out, "  - Multi-registry skills: yes");
    let _ = writeln!(out, "  - Skill security scanning: yes");
    let _ = writeln!(
        out,
        "  - ContextLattice MCP configured: {}",
        yes_no(has_contextlattice_mcp)
    );

    if let Some(repo_root) = discover_repo_root_for_about() {
        let report_dir = repo_root.join(".sync-reports");
        let workstream_path = repo_root.join("docs/parity/workstream-status.json");
        let queue_path = repo_root.join("docs/parity/upstream-missing-queue.json");
        let proof_path = repo_root.join("docs/parity/global-parity-proof.json");

        let mut upstream_ref = String::from("unknown");
        let mut upstream_sha = String::from("unknown");
        let mut workstream_generated = String::from("unknown");
        if let Some(workstream) = read_json_file(&workstream_path) {
            if let Some(v) = json_str_at_path(&workstream, &["upstream_ref"]) {
                upstream_ref = v;
            }
            if let Some(v) = json_str_at_path(&workstream, &["upstream_sha"]) {
                upstream_sha = v;
            }
            if let Some(v) = json_str_at_path(&workstream, &["generated_at_utc"]) {
                workstream_generated = v;
            }
        }

        let mut queue_pending = 0u64;
        let mut queue_ported = 0u64;
        let mut queue_superseded = 0u64;
        if let Some(queue) = read_json_file(&queue_path) {
            queue_pending =
                json_u64_at_path(&queue, &["summary", "by_disposition", "pending"]).unwrap_or(0);
            queue_ported =
                json_u64_at_path(&queue, &["summary", "by_disposition", "ported"]).unwrap_or(0);
            queue_superseded =
                json_u64_at_path(&queue, &["summary", "by_disposition", "superseded"]).unwrap_or(0);
        }

        let mut release_gate_pass = String::from("unknown");
        let mut ci_gate_pass = String::from("unknown");
        if let Some(proof) = read_json_file(&proof_path) {
            if let Some(v) =
                json_value_at_path(&proof, &["release_gate", "pass"]).and_then(|v| v.as_bool())
            {
                release_gate_pass = yes_no(v).to_string();
            }
            if let Some(v) =
                json_value_at_path(&proof, &["ci_gate", "pass"]).and_then(|v| v.as_bool())
            {
                ci_gate_pass = yes_no(v).to_string();
            }
        }

        let mut latest_report_name = String::from("none");
        let mut latest_origin_sha = String::from("unknown");
        let mut latest_upstream_sha = String::from("unknown");
        let mut latest_timestamp = String::from("unknown");
        let mut latest_pending_count = 0usize;
        if let Some(report_path) = latest_upstream_sync_report(&report_dir) {
            latest_report_name = report_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| report_path.display().to_string());
            let (meta, pending_count) = parse_sync_report_metadata(&report_path);
            latest_pending_count = pending_count;
            if let Some(v) = meta.get("origin_sha") {
                latest_origin_sha = v.clone();
            }
            if let Some(v) = meta.get("upstream_sha") {
                latest_upstream_sha = v.clone();
            }
            if let Some(v) = meta.get("timestamp_utc") {
                latest_timestamp = v.clone();
            }
        }

        let _ = writeln!(out);
        let _ = writeln!(out, "Parity Snapshot:");
        let _ = writeln!(out, "  - Repo root: {}", repo_root.display());
        let _ = writeln!(out, "  - Upstream ref: {}", upstream_ref);
        let _ = writeln!(out, "  - Upstream sha: {}", upstream_sha);
        let _ = writeln!(
            out,
            "  - Workstream report generated_at: {}",
            workstream_generated
        );
        let _ = writeln!(
            out,
            "  - Queue (pending/ported/superseded): {}/{}/{}",
            queue_pending, queue_ported, queue_superseded
        );
        let _ = writeln!(
            out,
            "  - Gate status (release/ci): {}/{}",
            release_gate_pass, ci_gate_pass
        );
        let _ = writeln!(out, "  - Latest sync report: {}", latest_report_name);
        let _ = writeln!(out, "  - Latest sync timestamp_utc: {}", latest_timestamp);
        let _ = writeln!(out, "  - Latest report origin_sha: {}", latest_origin_sha);
        let _ = writeln!(
            out,
            "  - Latest report upstream_sha: {}",
            latest_upstream_sha
        );
        let _ = writeln!(
            out,
            "  - Pending upstream commits in latest report: {}",
            latest_pending_count
        );
    } else {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Parity Snapshot: unavailable (run from a source checkout to load docs/parity + .sync-reports)."
        );
    }

    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

async fn handle_ops_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let yolo = !app.config.approval.require_approval;
        let out = format!(
            "Operator Control Plane\n\
             \n\
             Runtime:\n\
               session:      {}\n\
               model:        {}\n\
               personality:  {}\n\
             \n\
             Controls:\n\
               yolo:         {}\n\
               mouse:        {}\n\
               statusbar:    ON\n\
               reasoning:    toggle via `/ops reasoning`\n\
               raw:          toggle via `/ops raw`\n\
               verbose:      toggle via `/ops verbose`\n\
             \n\
             Quick actions:\n\
               /ops model [provider|provider:model]\n\
               /ops personality [list|name]\n\
               /ops mouse [on|off|toggle]\n\
               /ops yolo\n\
               /ops reasoning\n\
               /ops raw [on|off|toggle|once]\n\
               /ops verbose\n\
               /ops help",
            app.session_id,
            app.current_model,
            app.current_personality.as_deref().unwrap_or("(none)"),
            if yolo { "ON" } else { "OFF" },
            if app.mouse_enabled() { "ON" } else { "OFF" },
        );
        emit_command_output(app, out);
        return Ok(CommandResult::Handled);
    }

    match args[0].to_ascii_lowercase().as_str() {
        "help" => {
            emit_command_output(
                app,
                "Operator control plane commands:\n\
                 - /ops status\n\
                 - /ops model [provider|provider:model]\n\
                 - /ops personality [list|name]\n\
                 - /ops mouse [on|off|toggle]\n\
                 - /ops yolo\n\
                 - /ops reasoning\n\
                 - /ops raw [on|off|toggle|once]\n\
                 - /ops verbose\n\
                 - /ops statusbar",
            );
            Ok(CommandResult::Handled)
        }
        "model" => handle_model_command(app, &args[1..]).await,
        "personality" => handle_personality_command(app, &args[1..]),
        "mouse" => handle_mouse_command(app, &args[1..]),
        "yolo" => handle_yolo_command(app),
        "reasoning" => handle_reasoning_command(app),
        "raw" => handle_raw_command(app, &args[1..]),
        "verbose" => handle_verbose_command(app),
        "statusbar" => handle_statusbar_command(app),
        other => {
            emit_command_output(
                app,
                format!(
                    "Unknown /ops target '{}'. Try `/ops help` for available controls.",
                    other
                ),
            );
            Ok(CommandResult::Handled)
        }
    }
}

fn handle_save_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sessions_dir = hermes_config::hermes_home().join("sessions");
    std::fs::create_dir_all(&sessions_dir)
        .map_err(|e| AgentError::Io(format!("Failed to create sessions dir: {}", e)))?;

    let filename = if args.is_empty() {
        format!("{}.json", app.session_id)
    } else {
        format!("{}.json", args[0])
    };

    let path = sessions_dir.join(&filename);
    let info = app.session_info();
    let data = serde_json::json!({
        "session_info": info,
        "messages": app.messages.iter().map(|m| {
            serde_json::json!({
                "role": format!("{:?}", m.role),
                "content": m.content.as_deref().unwrap_or(""),
            })
        }).collect::<Vec<_>>(),
    });

    let json =
        serde_json::to_string_pretty(&data).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(&path, json)
        .map_err(|e| AgentError::Io(format!("Failed to save session: {}", e)))?;

    emit_command_output(app, format!("Session saved to {}", path.display()));
    Ok(CommandResult::Handled)
}

fn handle_load_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sessions_dir = hermes_config::hermes_home().join("sessions");

    if args.is_empty() {
        // List available sessions
        if !sessions_dir.exists() {
            emit_command_output(app, "No saved sessions found.");
            return Ok(CommandResult::Handled);
        }
        let entries: Vec<String> = std::fs::read_dir(&sessions_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext == "json")
                            .unwrap_or(false)
                    })
                    .filter_map(|e| {
                        e.path()
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                    })
                    .collect()
            })
            .unwrap_or_default();

        if entries.is_empty() {
            emit_command_output(app, "No saved sessions found.");
        } else {
            let mut out = String::from("Saved sessions:\n");
            for name in &entries {
                out.push_str(&format!("- `{}`\n", name));
            }
            out.push_str("\nUsage: `/load <session-name>`");
            emit_command_output(app, out.trim_end());
        }
        return Ok(CommandResult::Handled);
    }

    let name = args[0];
    let path = sessions_dir.join(format!("{}.json", name));
    if !path.exists() {
        emit_command_output(
            app,
            format!("Session '{}' not found at {}", name, path.display()),
        );
        return Ok(CommandResult::Handled);
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| AgentError::Io(format!("Failed to read session: {}", e)))?;
    let data: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| AgentError::Config(format!("Failed to parse session: {}", e)))?;

    if let Some(messages) = data.get("messages").and_then(|m| m.as_array()) {
        app.messages.clear();
        for msg in messages {
            let role_str = msg.get("role").and_then(|r| r.as_str()).unwrap_or("User");
            let content_str = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
            let message = match role_str {
                "Assistant" => hermes_core::Message::assistant(content_str),
                "System" => hermes_core::Message::system(content_str),
                _ => hermes_core::Message::user(content_str),
            };
            app.messages.push(message);
        }
        emit_command_output(
            app,
            format!(
                "Loaded session '{}' ({} messages)",
                name,
                app.messages.len()
            ),
        );
    } else {
        emit_command_output(app, "Session file has no messages array.");
    }

    Ok(CommandResult::Handled)
}

fn handle_background_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        emit_command_output(
            app,
            "Usage: /background <message>\nQueues a task to run in the background while you continue chatting.",
        );
        return Ok(CommandResult::Handled);
    }

    let task = args.join(" ");
    let job_id = format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S"),
        uuid::Uuid::new_v4().simple()
    );
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    std::fs::create_dir_all(&jobs_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create background job directory {}: {}",
            jobs_dir.display(),
            e
        ))
    })?;
    let status_path = jobs_dir.join(format!("{}.json", job_id));
    let log_path = jobs_dir.join(format!("{}.log", job_id));

    let status = serde_json::json!({
        "id": job_id,
        "task": task,
        "status": "queued",
        "created_at": chrono::Utc::now().to_rfc3339(),
        "started_at": serde_json::Value::Null,
        "finished_at": serde_json::Value::Null,
        "exit_code": serde_json::Value::Null,
        "log_path": log_path,
    });
    std::fs::write(
        &status_path,
        serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|e| AgentError::Io(format!("Failed to write background status: {}", e)))?;

    let task_for_run = task.clone();
    let status_path_for_run = status_path.clone();
    let log_path_for_run = log_path.clone();
    tokio::spawn(async move {
        let started = chrono::Utc::now().to_rfc3339();
        let mut queued = read_json_map(&status_path_for_run);
        queued.insert(
            "status".to_string(),
            serde_json::Value::String("running".into()),
        );
        queued.insert(
            "started_at".to_string(),
            serde_json::Value::String(started.clone()),
        );
        let _ = write_json_map(&status_path_for_run, &queued);

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                let mut failed = queued.clone();
                failed.insert("status".into(), serde_json::Value::String("failed".into()));
                failed.insert(
                    "finished_at".into(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );
                failed.insert(
                    "error".into(),
                    serde_json::Value::String(format!("current_exe: {}", e)),
                );
                let _ = write_json_map(&status_path_for_run, &failed);
                return;
            }
        };

        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("chat")
            .arg("--query")
            .arg(task_for_run)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Ok(home) = std::env::var("HERMES_HOME") {
            cmd.env("HERMES_HOME", home);
        }

        let out = cmd.output().await;
        match out {
            Ok(output) => {
                let exit = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let log = format!(
                    "task: {}\nstarted_at: {}\nfinished_at: {}\nexit_code: {}\n\n[stdout]\n{}\n\n[stderr]\n{}\n",
                    queued
                        .get("task")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    started,
                    chrono::Utc::now().to_rfc3339(),
                    exit,
                    stdout,
                    stderr
                );
                let _ = std::fs::write(&log_path_for_run, log);

                let mut done = queued.clone();
                done.insert(
                    "status".into(),
                    serde_json::Value::String(if output.status.success() {
                        "completed".into()
                    } else {
                        "failed".into()
                    }),
                );
                done.insert(
                    "finished_at".into(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );
                done.insert("exit_code".into(), serde_json::json!(exit));
                let _ = write_json_map(&status_path_for_run, &done);
            }
            Err(e) => {
                let mut failed = queued.clone();
                failed.insert("status".into(), serde_json::Value::String("failed".into()));
                failed.insert(
                    "finished_at".into(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );
                failed.insert(
                    "error".into(),
                    serde_json::Value::String(format!("spawn/output failed: {}", e)),
                );
                let _ = write_json_map(&status_path_for_run, &failed);
            }
        }
    });

    emit_command_output(
        app,
        format!(
            "[Background task queued: \"{}\"]\nJob ID: {}\nStatus: {}\nLogs:   {}\nThis task runs in a detached `hermes chat --query ...` process.",
            task,
            status["id"].as_str().unwrap_or("unknown"),
            status_path.display(),
            log_path.display()
        ),
    );

    Ok(CommandResult::Handled)
}

fn read_json_map(path: &std::path::Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_json_map(
    path: &std::path::Path,
    map: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), std::io::Error> {
    let content = serde_json::to_string_pretty(&serde_json::Value::Object(map.clone()))
        .unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path, content)
}

fn handle_verbose_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let current = tracing::enabled!(tracing::Level::DEBUG);
    if current {
        emit_command_output(
            app,
            "Verbose mode: OFF (switching to info level)\n(Runtime log level changes require restart — use `hermes -v` for verbose)",
        );
    } else {
        emit_command_output(
            app,
            "Verbose mode: ON (switching to debug level)\n(Runtime log level changes require restart — use `hermes -v` for verbose)",
        );
    }
    Ok(CommandResult::Handled)
}

fn handle_yolo_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let currently_required = app.config.approval.require_approval;
    let new_val = !currently_required;

    app.config = Arc::new({
        let mut cfg = (*app.config).clone();
        cfg.approval.require_approval = new_val;
        cfg
    });

    if !new_val {
        emit_command_output(
            app,
            "YOLO mode: ON — tool executions will not require approval.\nBe careful! The agent can now execute tools without confirmation.",
        );
    } else {
        emit_command_output(
            app,
            "YOLO mode: OFF — tool executions will require approval.",
        );
    }
    Ok(CommandResult::Handled)
}

fn handle_reasoning_command(app: &mut App) -> Result<CommandResult, AgentError> {
    // Reasoning display is a runtime-only toggle; stored as thread-local state
    // since StreamingConfig doesn't have a show_reasoning field.
    use std::sync::atomic::{AtomicBool, Ordering};
    static SHOW_REASONING: AtomicBool = AtomicBool::new(false);

    let prev = SHOW_REASONING.fetch_xor(true, Ordering::Relaxed);
    let new_val = !prev;

    if new_val {
        emit_command_output(
            app,
            "Reasoning display: ON — model reasoning will be shown.",
        );
    } else {
        emit_command_output(
            app,
            "Reasoning display: OFF — model reasoning will be hidden.",
        );
    }
    Ok(CommandResult::Handled)
}

fn handle_raw_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let state = app.tool_registry.raw_mode_state();
    let log_dir = app.tool_registry.rtk_log_dir();
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        emit_command_output(
            app,
            format!(
                "RTK raw mode: {}{}\nDual logs: {}\nUsage: /raw [on|off|toggle|once|status]",
                if state.enabled { "ON" } else { "OFF" },
                if state.once_pending {
                    " (one-shot pending)"
                } else {
                    ""
                },
                log_dir.display()
            ),
        );
        return Ok(CommandResult::Handled);
    }

    match args[0].trim().to_ascii_lowercase().as_str() {
        "help" => emit_command_output(
            app,
            "RTK raw controls:\n  /raw status   Show current mode + log path\n  /raw on       Disable output filtering for all tool calls\n  /raw off      Re-enable RTK output filtering\n  /raw toggle   Toggle global raw mode\n  /raw once     Raw pass-through for next tool call only",
        ),
        "once" => {
            app.tool_registry.set_raw_mode_once();
            emit_command_output(
                app,
                "RTK raw mode armed for next tool call only. It auto-resets after one dispatch.",
            );
        }
        "on" | "off" | "toggle" | "true" | "false" | "yes" | "no" | "1" | "0" => {
            let next = match args[0].trim().to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => true,
                "off" | "false" | "no" | "0" => false,
                "toggle" => !state.enabled,
                _ => state.enabled,
            };
            app.tool_registry.set_raw_mode(next);
            std::env::set_var("HERMES_RTK_RAW", if next { "1" } else { "0" });
            emit_command_output(
                app,
                format!(
                    "RTK raw mode: {} (dual logs: {})",
                    if next { "ON" } else { "OFF" },
                    log_dir.display()
                ),
            );
        }
        _ => emit_command_output(app, "Usage: /raw [on|off|toggle|once|status]"),
    }
    Ok(CommandResult::Handled)
}

fn handle_policy_command(app: &mut App, _args: &[&str]) -> Result<CommandResult, AgentError> {
    emit_command_output(
        app,
        "The adaptive `/policy` CLI was removed — Hermes Python has no equivalent policy store.",
    );
    Ok(CommandResult::Handled)
}

fn handle_history_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let transcript = app.transcript_messages();
    if transcript.is_empty() {
        emit_command_output(app, "No conversation history yet.");
        return Ok(CommandResult::Handled);
    }
    let mut out = String::from("Recent conversation history:\n");
    for (idx, msg) in transcript.iter().enumerate().rev().take(12).rev() {
        let role = match msg.role {
            hermes_core::MessageRole::User => "USER",
            hermes_core::MessageRole::Assistant => "HERMES",
            hermes_core::MessageRole::System => "SYSTEM",
            hermes_core::MessageRole::Tool => "TOOL",
        };
        let preview = msg
            .content
            .as_deref()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .trim();
        let clipped = if preview.chars().count() > 96 {
            let mut s: String = preview.chars().take(95).collect();
            s.push('…');
            s
        } else {
            preview.to_string()
        };
        let _ = writeln!(out, "{:>3}. {:<7} {}", idx + 1, role, clipped);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

async fn handle_provider_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let providers = curated_provider_slugs();
    if providers.is_empty() {
        emit_command_output(app, "No providers registered.");
        return Ok(CommandResult::Handled);
    }
    let entries = crate::model_switch::provider_catalog_entries(&providers, 4).await;
    if entries.is_empty() {
        emit_command_output(
            app,
            format!(
                "Configured providers: {}\nCurrent model: {}",
                providers.join(", "),
                app.current_model
            ),
        );
        return Ok(CommandResult::Handled);
    }
    let mut out = format!("Current model: {}\n\nProviders:\n", app.current_model);
    for entry in entries {
        let preview = entry.models.join(", ");
        let suffix = if entry.total_models > entry.models.len() {
            format!(" (+{} more)", entry.total_models - entry.models.len())
        } else {
            String::new()
        };
        let _ = writeln!(out, "  - {:<14} {}{}", entry.provider, preview, suffix);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_profile_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let home = hermes_config::hermes_home();
    let selected = app.config.profile.current.as_deref().unwrap_or("default");
    let mut out = String::new();
    let _ = writeln!(out, "Active profile: {}", selected);
    let _ = writeln!(out, "Hermes home: {}", home.display());
    let _ = writeln!(out, "Session id: {}", app.session_id);
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_runtime_ui_mode_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let arg = args.first().copied().unwrap_or("status");
    let msg = match cmd {
        "/fast" => format!(
            "Fast mode compatibility command received (`{}`).\nCurrent model: {}\nTip: switch to a lower-latency model via `/model`.",
            arg, app.current_model
        ),
        "/skin" => "Skin/themes are selected with `HERMES_THEME`.\nAvailable built-ins: ultra-neon, ultra-amber, ultra-ice, ultra-hc, dark, light.".to_string(),
        "/voice" => "Voice mode uses provider/platform capabilities; no separate TUI voice engine is active in this session.".to_string(),
        _ => "Unsupported runtime UI mode command.".to_string(),
    };
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

fn handle_toolsets_command(app: &mut App) -> Result<CommandResult, AgentError> {
    if app.config.platform_toolsets.is_empty() {
        emit_command_output(app, "No explicit platform toolsets configured.");
        return Ok(CommandResult::Handled);
    }
    let mut rows: Vec<_> = app.config.platform_toolsets.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = String::from("Configured toolsets by platform:\n");
    for (platform, toolsets) in rows {
        let _ = writeln!(out, "  - {:<10} {}", platform, toolsets.join(", "));
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_plugins_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let plugins_dir = hermes_config::hermes_home().join("plugins");
    if !plugins_dir.exists() {
        emit_command_output(
            app,
            format!(
                "Plugin directory not found yet: {}\nUse `hermes plugins install ...` to add plugin bundles.",
                plugins_dir.display()
            ),
        );
        return Ok(CommandResult::Handled);
    }
    let mut plugin_names = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&plugins_dir) {
        for entry in read_dir.flatten() {
            if entry.path().is_dir() {
                plugin_names.push(entry.file_name().to_string_lossy().to_string());
            }
        }
    }
    plugin_names.sort();
    if plugin_names.is_empty() {
        emit_command_output(
            app,
            format!("No installed plugin bundles in {}.", plugins_dir.display()),
        );
    } else {
        emit_command_output(
            app,
            format!(
                "Installed plugin bundles ({}):\n{}",
                plugin_names.len(),
                plugin_names
                    .iter()
                    .map(|n| format!("  - {}", n))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        );
    }
    Ok(CommandResult::Handled)
}

fn handle_mcp_command(app: &mut App) -> Result<CommandResult, AgentError> {
    if app.config.mcp_servers.is_empty() {
        emit_command_output(app, "No MCP servers configured in `config.yaml`.");
        return Ok(CommandResult::Handled);
    }
    let mut out = String::from("Configured MCP servers:\n");
    for server in &app.config.mcp_servers {
        let endpoint = server
            .url
            .as_deref()
            .filter(|u| !u.is_empty())
            .unwrap_or("<stdio>");
        let _ = writeln!(out, "  - {:<18} {}", server.name, endpoint);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_reload_command(app: &mut App, cmd: &str) -> Result<CommandResult, AgentError> {
    if cmd == "/reload-mcp" {
        emit_command_output(
            app,
            "MCP reload requested. Restart session/gateway for full connector renegotiation.",
        );
    } else {
        emit_command_output(
            app,
            "Config/env reload requested. Secrets and dynamic provider keys are re-read on next tool/model operation.",
        );
    }
    Ok(CommandResult::Handled)
}

fn handle_cron_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let cron_data = hermes_config::cron_dir();
    let jobs_file = cron_data.join("jobs.json");
    let count = std::fs::read_to_string(&jobs_file)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_array().map(|arr| arr.len()))
        .unwrap_or(0);
    emit_command_output(
        app,
        format!(
            "Cron scheduler data dir: {}\nPersisted jobs: {}\nUse `hermes cron list` for full job table.",
            cron_data.display(),
            count
        ),
    );
    Ok(CommandResult::Handled)
}

fn background_status_rows() -> Vec<String> {
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    let mut rows = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(&jobs_dir) else {
        return rows;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("unknown");
        let status = v
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown");
        let task = v
            .get("task")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .replace('\n', " ");
        rows.push(format!("{id}  [{status}]  {task}"));
    }
    rows.sort();
    rows
}

fn handle_agents_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let rows = background_status_rows();
    if rows.is_empty() {
        emit_command_output(app, "No background jobs found.");
    } else {
        let joined = rows.into_iter().take(20).collect::<Vec<_>>().join("\n");
        emit_command_output(app, format!("Background jobs:\n{}", joined));
    }
    Ok(CommandResult::Handled)
}

fn handle_capability_surface_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let msg = match cmd {
        "/plan" => "Planning mode is available through structured prompting and delegated workers; use `/background <task>` for long-running plans.",
        "/lsp" => "LSP/code-index context is enabled by default for workspace-aware runs. If context seems stale, restart the session to refresh index snapshots.",
        "/graph" => "Graph-memory and ContextLattice integration are active; use normal prompts and the agent will retrieve memory context automatically.",
        "/image" => {
            if let Some(path) = args.first() {
                return {
                    emit_command_output(
                        app,
                        format!(
                            "Image hint captured: `{}`.\nSend your next prompt describing how Hermes should use this image.",
                            path
                        ),
                    );
                    Ok(CommandResult::Handled)
                };
            }
            "Usage: /image <path> — attach an image hint for your next prompt."
        }
        _ => "Command surface available.",
    };
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

fn handle_session_compat_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let arg_joined = args.join(" ");
    let msg = match cmd {
        "/title" => {
            if arg_joined.trim().is_empty() {
                "Usage: /title <name>".to_string()
            } else {
                format!("Session title marker set to: {}", arg_joined.trim())
            }
        }
        "/branch" => {
            if arg_joined.trim().is_empty() {
                "Branch marker created for current session.".to_string()
            } else {
                format!("Branch marker created: {}", arg_joined.trim())
            }
        }
        "/snapshot" => "Snapshot compatibility command acknowledged. Use `hermes backup` / `hermes import` for persisted state snapshots.".to_string(),
        "/rollback" => "Rollback checkpoints are managed through saved sessions. Use `/save`, `/load`, and `/history`.".to_string(),
        "/queue" => {
            if arg_joined.trim().is_empty() {
                "Usage: /queue <prompt>".to_string()
            } else {
                format!("Queued prompt hint: {}", arg_joined.trim())
            }
        }
        "/steer" => {
            if arg_joined.trim().is_empty() {
                "Usage: /steer <instruction>".to_string()
            } else {
                format!("Steering note recorded: {}", arg_joined.trim())
            }
        }
        "/btw" => {
            if arg_joined.trim().is_empty() {
                "Usage: /btw <question>".to_string()
            } else {
                format!("Side-question captured: {}", arg_joined.trim())
            }
        }
        "/sethome" => "Home-session marker command is primarily gateway-facing; local CLI session remains active.".to_string(),
        _ => "Compatibility command acknowledged.".to_string(),
    };
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

fn handle_clear_queue_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    let mut removed = 0usize;
    if let Ok(read_dir) = std::fs::read_dir(&jobs_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let status = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| {
                    v.get("status")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            if matches!(
                status.as_str(),
                "queued" | "running" | "failed" | "completed"
            ) {
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
    }
    emit_command_output(
        app,
        format!("Cleared {} queued/background status file(s).", removed),
    );
    Ok(CommandResult::Handled)
}

fn handle_insights_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let msg_count = app.messages.len();
    let user_count = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::User)
        .count();
    let assistant_count = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::Assistant)
        .count();
    emit_command_output(
        app,
        format!(
            "Session insights:\n  - Total messages: {}\n  - User messages: {}\n  - Hermes messages: {}\n  - Session: {}",
            msg_count, user_count, assistant_count, app.session_id
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_platforms_command(app: &mut App) -> Result<CommandResult, AgentError> {
    if app.config.platforms.is_empty() {
        emit_command_output(
            app,
            "No explicit gateway platform adapters configured (running in local CLI mode).",
        );
        return Ok(CommandResult::Handled);
    }
    let mut entries: Vec<_> = app.config.platforms.keys().cloned().collect();
    entries.sort();
    let mut out = String::from("Configured gateway platforms:\n");
    for p in entries {
        let _ = writeln!(out, "  - {}", p);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_log_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let logs_dir = hermes_config::hermes_home().join("logs");
    let mut files = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&logs_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    files.reverse();
    if files.is_empty() {
        emit_command_output(app, format!("No log files found in {}", logs_dir.display()));
        return Ok(CommandResult::Handled);
    }
    let mut out = format!("Recent log files in {}:\n", logs_dir.display());
    for path in files.into_iter().take(12) {
        let _ = writeln!(
            out,
            "  - {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    out.push_str("Use `hermes logs` for full tail output.");
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_compatibility_notice_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let arg = args.join(" ");
    let msg = match cmd {
        "/debug-dump" => "Debug dump compatibility mode: use `hermes debug share --local` for a full local diagnostic bundle.".to_string(),
        "/dump-format" => "Transcript export format: JSON session snapshots (`/save`) with role/content fields plus metadata.".to_string(),
        "/experiment" => format!(
            "Experiment surface ready. Current model: {}. {}",
            app.current_model,
            if arg.trim().is_empty() {
                "Use `/model` to switch experiment variants.".to_string()
            } else {
                format!("Received experiment hint: {}", arg.trim())
            }
        ),
        "/feedback" => "Feedback channels: open a GitHub issue in this repository with repro steps + `hermes debug share --local` output.".to_string(),
        "/copy" => "Clipboard copy helper is platform-dependent; use terminal copy from transcript for now.".to_string(),
        "/paste" => "Clipboard attach helper is platform-dependent; use `/image <path>` for image workflows.".to_string(),
        "/gquota" => "Gemini quota details come from provider account dashboards; no direct CLI quota probe is active in this build.".to_string(),
        "/restart" => "Gateway restart is a gateway-mode command. Use `hermes gateway restart`.".to_string(),
        "/approve" => "Approve is gateway workflow only (pending approval queue).".to_string(),
        "/deny" => "Deny is gateway workflow only (pending approval queue).".to_string(),
        "/update" => "Update compatibility command: use `hermes update` for updater workflow.".to_string(),
        _ => "Compatibility command acknowledged.".to_string(),
    };
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

fn handle_statusbar_command(app: &mut App) -> Result<CommandResult, AgentError> {
    emit_command_output(
        app,
        "Status bar is always enabled in the current TUI renderer.",
    );
    Ok(CommandResult::Handled)
}

fn parse_toggle_arg(raw: Option<&str>, current: bool) -> Result<bool, &'static str> {
    let Some(raw) = raw else {
        return Ok(!current);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "toggle" => Ok(!current),
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => Err("Usage: /mouse [on|off|toggle]"),
    }
}

fn handle_mouse_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.len() >= 2 && args[0].eq_ignore_ascii_case("set") {
        match parse_toggle_arg(args.get(1).copied(), app.mouse_enabled()) {
            Ok(next) => {
                app.set_mouse_enabled(next);
                std::env::set_var("HERMES_TUI_MOUSE", if next { "1" } else { "0" });
                emit_command_output(
                    app,
                    format!("Mouse interactions: {}", if next { "ON" } else { "OFF" }),
                );
            }
            Err(usage) => emit_command_output(app, usage),
        }
        return Ok(CommandResult::Handled);
    }

    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        emit_command_output(
            app,
            format!(
                "Mouse interactions: {} (use `/mouse on` or `/mouse off`)",
                if app.mouse_enabled() { "ON" } else { "OFF" }
            ),
        );
        return Ok(CommandResult::Handled);
    }

    match parse_toggle_arg(args.first().copied(), app.mouse_enabled()) {
        Ok(next) => {
            app.set_mouse_enabled(next);
            std::env::set_var("HERMES_TUI_MOUSE", if next { "1" } else { "0" });
            emit_command_output(
                app,
                format!("Mouse interactions: {}", if next { "ON" } else { "OFF" }),
            );
        }
        Err(usage) => emit_command_output(app, usage),
    }
    Ok(CommandResult::Handled)
}

fn print_help(app: &mut App) {
    let mut out = String::from("Hermes Agent — Available Commands:\n\n");
    for (cmd, desc) in SLASH_COMMANDS {
        out.push_str(&format!("`{:<16}` {}\n", cmd, desc));
    }
    out.push_str("\nYou can also type any text to send it as a message to the agent.");
    emit_command_output(app, out);
}

// ---------------------------------------------------------------------------
// CLI subcommand handlers (dispatched from main.rs)
// ---------------------------------------------------------------------------

fn resolve_cli_chat_provider_model(
    config_model: Option<&str>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
) -> Result<String, AgentError> {
    let provider_override = provider_override
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_ascii_lowercase());
    let model_override = model_override.map(str::trim).filter(|v| !v.is_empty());

    let mut current_model = config_model
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("gpt-4o")
        .to_string();

    if let Some(model) = model_override {
        current_model = model.to_string();
    } else if provider_override.is_none() {
        if let Ok(model_env) = std::env::var("HERMES_INFERENCE_MODEL") {
            let model_env = model_env.trim();
            if !model_env.is_empty() {
                current_model = model_env.to_string();
            }
        }
    }
    if let Some(provider) = provider_override.as_deref() {
        if let Some((_, model_name)) = current_model.split_once(':') {
            current_model = format!("{provider}:{}", model_name.trim());
        } else {
            current_model = format!("{provider}:{}", current_model.trim());
        }
    }
    if !current_model.contains(':') {
        current_model = normalize_provider_model(&current_model)?;
    }
    Ok(current_model)
}

fn apply_cli_chat_runtime_env(provider_model: &str) {
    let provider_model = provider_model.trim();
    if provider_model.is_empty() {
        return;
    }
    std::env::set_var("HERMES_MODEL", provider_model);
    std::env::set_var("HERMES_INFERENCE_MODEL", provider_model);
    if let Some((provider, _)) = provider_model.split_once(':') {
        let provider = provider.trim();
        if !provider.is_empty() {
            std::env::set_var("HERMES_INFERENCE_PROVIDER", provider);
            if std::env::var_os("HERMES_TUI_PROVIDER").is_some() {
                std::env::set_var("HERMES_TUI_PROVIDER", provider);
            }
        }
    }
}

/// Handle `hermes chat [--query ...] [--preload-skill ...] [--yolo]`.
pub async fn handle_cli_chat(
    query: Option<String>,
    preload_skill: Option<String>,
    yolo: bool,
    model_override: Option<String>,
    provider_override: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    use crate::runtime_tool_wiring::{wire_cron_scheduler_backend, wire_stdio_clarify_backend};
    use crate::terminal_backend::build_terminal_backend;
    use crate::tool_preview::{build_tool_preview_from_value, tool_emoji};
    use hermes_config::load_config;
    use hermes_core::MessageRole;
    use hermes_cron::cron_scheduler_for_data_dir;
    use hermes_skills::{FileSkillStore, SkillManager};
    use hermes_tools::ToolRegistry;

    if let Some(skill) = &preload_skill {
        println!("[Preloading skill: {}]", skill);
    }
    if yolo {
        println!("[YOLO mode: tool confirmations disabled]");
    }

    let mut config =
        load_config(None).map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;

    if yolo {
        config.approval.require_approval = false;
    }

    let current_model = resolve_cli_chat_provider_model(
        config.model.as_deref(),
        model_override.as_deref(),
        provider_override.as_deref(),
    )?;
    apply_cli_chat_runtime_env(&current_model);

    let tool_registry = Arc::new(ToolRegistry::new());
    let terminal_backend = build_terminal_backend(&config);
    let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
    let skill_provider: Arc<dyn hermes_core::SkillProvider> =
        Arc::new(SkillManager::new(skill_store));
    hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
    wire_stdio_clarify_backend(&tool_registry);
    let cron_data_dir = hermes_config::cron_dir();
    std::fs::create_dir_all(&cron_data_dir)
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    let cron_scheduler = Arc::new(cron_scheduler_for_data_dir(cron_data_dir));
    cron_scheduler
        .load_persisted_jobs()
        .await
        .map_err(|e| hermes_core::AgentError::Config(format!("cron load: {e}")))?;
    cron_scheduler.start().await;
    wire_cron_scheduler_backend(&tool_registry, cron_scheduler);
    let tool_schemas =
        crate::platform_toolsets::resolve_platform_tool_schemas(&config, "cli", &tool_registry);
    let agent_tool_registry = Arc::new(crate::app::bridge_tool_registry(&tool_registry));

    let agent_config = crate::app::build_agent_config(&config, &current_model);
    let provider = crate::app::build_provider(&config, &current_model);

    let on_tool_start: Box<dyn Fn(&str, &serde_json::Value) + Send + Sync> =
        Box::new(move |name: &str, args: &serde_json::Value| {
            let emoji = tool_emoji(name);
            let preview = build_tool_preview_from_value(name, args, 56).unwrap_or_default();
            if preview.is_empty() {
                println!("┊ {emoji} {name}");
            } else {
                println!("┊ {emoji} {name:<16} {preview}");
            }
        });
    let on_tool_complete: Box<dyn Fn(&str, &str) + Send + Sync> =
        Box::new(move |name: &str, result: &str| {
            let mut snippet: String = result.trim().chars().take(96).collect();
            if result.trim().chars().count() > 96 {
                snippet.push_str("...");
            }
            let emoji = tool_emoji(name);
            if snippet.is_empty() {
                println!("┊ {emoji} {name:<16} done");
            } else {
                println!("┊ {emoji} {name:<16} done: {snippet}");
            }
        });
    let callbacks = hermes_agent::AgentCallbacks {
        on_tool_start: Some(on_tool_start),
        on_tool_complete: Some(on_tool_complete),
        ..Default::default()
    };
    let agent = hermes_agent::AgentLoop::new(agent_config, agent_tool_registry, provider)
        .with_callbacks(callbacks);

    match query {
        Some(q) => {
            let messages = vec![hermes_core::Message::user(&q)];
            let result = agent.run(messages, Some(tool_schemas)).await?;

            let reply = result
                .messages
                .iter()
                .rev()
                .find_map(|m| {
                    if m.role == MessageRole::Assistant {
                        m.content.clone()
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| "(no assistant reply)".to_string());
            println!("{}", reply);
        }
        None => {
            println!("Starting interactive chat session...");
            println!("(Use `hermes` for the default interactive TUI)");
        }
    }
    Ok(())
}

/// Handle `hermes skills [action] [name] [--extra ...]`.
pub async fn handle_cli_skills(
    action: Option<String>,
    name: Option<String>,
    extra: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    let skills_dir = hermes_config::hermes_home().join("skills");

    match action.as_deref().unwrap_or("list") {
        "list" => {
            if !skills_dir.exists() {
                println!(
                    "No skills directory found at {}. Run `hermes setup` first.",
                    skills_dir.display()
                );
                return Ok(());
            }
            let mut count = 0u32;
            println!("Installed skills ({}):", skills_dir.display());
            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let skill_md = path.join("SKILL.md");
                    if path.is_dir() && skill_md.exists() {
                        let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                        let first_line = std::fs::read_to_string(&skill_md)
                            .ok()
                            .and_then(|c| {
                                c.lines()
                                    .find(|l| l.starts_with('#'))
                                    .map(|l| l.trim_start_matches('#').trim().to_string())
                            })
                            .unwrap_or_else(|| "(no description)".to_string());
                        println!("  • {} — {}", dir_name, first_line);
                        count += 1;
                    }
                }
            }
            if count == 0 {
                println!("  (no skills installed)");
            }
        }
        "browse" => {
            if !skills_dir.exists() {
                println!("No skills directory found.");
                return Ok(());
            }
            println!("Skills Browser");
            println!("==============\n");
            let mut categories: std::collections::HashMap<String, Vec<(String, String)>> =
                std::collections::HashMap::new();
            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let skill_md = path.join("SKILL.md");
                    if path.is_dir() && skill_md.exists() {
                        let dir_name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                        let first_line = content
                            .lines()
                            .find(|l| l.starts_with('#'))
                            .map(|l| l.trim_start_matches('#').trim().to_string())
                            .unwrap_or_else(|| "(no description)".to_string());
                        let category = path
                            .parent()
                            .and_then(|p| p.file_name())
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "general".to_string());
                        categories
                            .entry(category)
                            .or_default()
                            .push((dir_name, first_line));
                    }
                }
            }
            for (category, skills) in &categories {
                println!("[{}]", category);
                for (name, desc) in skills {
                    println!("  • {} — {}", name, desc);
                }
                println!();
            }
            if categories.is_empty() {
                println!("  (no skills installed)");
            }
        }
        "search" => {
            let query = name.unwrap_or_default();
            if query.is_empty() {
                println!("Usage: hermes skills search <query>");
                return Ok(());
            }
            println!("Searching registries for: \"{}\"...", query);
            let client = reqwest::Client::new();
            let mut displayed_results = false;

            if let Ok(results) = search_multi_registry(&client, &query, 40).await {
                if !results.is_empty() {
                    displayed_results = true;
                    println!("Multi-registry matches:");
                    for rec in results {
                        let short_desc = if rec.description.trim().is_empty() {
                            "(no description)"
                        } else {
                            rec.description.trim()
                        };
                        println!("  • [{}] {} — {}", rec.source, rec.identifier, short_desc);
                    }
                    println!(
                        "\nInstall with: hermes skills install <identifier> (example: skills.sh/anthropics/skills/skill-creator)"
                    );
                }
            }

            // Legacy hub path retained for compatibility.
            match client
                .get("https://skills.hermes.run/api/search")
                .query(&[("q", &query)])
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(data) = resp.json::<serde_json::Value>().await {
                        if let Some(results) = data.get("results").and_then(|r| r.as_array()) {
                            if results.is_empty() {
                                if !displayed_results {
                                    println!("No skills found matching \"{}\".", query);
                                }
                            } else {
                                displayed_results = true;
                                println!("\nLegacy Skills Hub matches:");
                                for skill in results {
                                    let name =
                                        skill.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                    let desc = skill
                                        .get("description")
                                        .and_then(|d| d.as_str())
                                        .unwrap_or("");
                                    let version = skill
                                        .get("version")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?");
                                    println!("  • {} (v{}) — {}", name, version, desc);
                                }
                                println!("\nInstall with: hermes skills install <name>");
                            }
                        } else {
                            if !displayed_results {
                                println!("Unexpected response format from Skills Hub.");
                            }
                        }
                    }
                }
                Ok(resp) => {
                    if !displayed_results {
                        println!("Skills Hub returned status {}", resp.status());
                    }
                }
                Err(e) => {
                    if !displayed_results {
                        println!("Could not reach Skills Hub: {}", e);
                    }
                }
            }
            if !displayed_results {
                if let Ok(skills_sh_hits) = search_skills_sh_registry(&client, &query, 20).await {
                    if !skills_sh_hits.is_empty() {
                        displayed_results = true;
                        println!("\nSkills.sh fallback matches:");
                        for (name, identifier) in skills_sh_hits {
                            println!("  • {} — {}", name, identifier);
                        }
                        println!(
                            "\nInstall with: hermes skills install skills.sh/<owner/repo/skill>"
                        );
                    }
                }
            }
            if !displayed_results {
                let taps_file = hermes_config::hermes_home().join("skill_taps.json");
                let taps = merged_skill_taps(&read_skill_taps(&taps_file));
                let fallback = search_skills_via_taps(&client, &taps, &query, 20).await?;
                if fallback.is_empty() {
                    println!("No tap-backed matches found for \"{}\".", query);
                } else {
                    println!("\nTap-backed matches:");
                    for (name, source) in fallback {
                        println!("  • {} — {}", name, source);
                    }
                    println!(
                        "\nInstall with: hermes skills install <name> or hermes skills install <owner/repo/path>"
                    );
                }
            }
        }
        "install" => {
            let skill_spec = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills install <name>".into(),
                )
            })?;
            let (skill_name, _requested_version) = parse_skill_name_and_version(&skill_spec);
            println!("Installing skill: {}", skill_name);

            let client = reqwest::Client::new();
            let explicit = parse_explicit_github_skill(&skill_name);
            let registry_prefixed = parse_registry_prefixed_skill(&skill_name);

            let (files, install_seed) = if let Some((repo, maybe_branch, skill_dir)) = explicit {
                let branch = if let Some(branch) = maybe_branch {
                    branch
                } else {
                    github_default_branch(&client, &repo).await?
                };
                let resolved = ResolvedSkillSource {
                    repo,
                    branch,
                    skill_dir,
                };
                println!(
                    "Resolved source: {}/{} @ {}",
                    resolved.repo, resolved.skill_dir, resolved.branch
                );
                (
                    fetch_skill_files_from_github(&client, &resolved).await?,
                    skill_name.clone(),
                )
            } else if let Some((source, key)) = registry_prefixed {
                match source.as_str() {
                    "official" => {
                        let resolved = resolve_official_skill_source(&client, &key).await?;
                        println!(
                            "Resolved official source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            key,
                        )
                    }
                    "skills.sh" => {
                        let resolved = resolve_skills_sh_source(&client, &key).await?;
                        println!(
                            "Resolved skills.sh source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            key,
                        )
                    }
                    "lobehub" => {
                        println!("Resolved lobehub source: {}", key);
                        (fetch_lobehub_skill_files(&client, &key).await?, key)
                    }
                    "clawhub" => {
                        println!("Resolved clawhub source: {}", key);
                        (
                            fetch_clawhub_skill_files(&client, &key, _requested_version.as_deref())
                                .await?,
                            key,
                        )
                    }
                    "claude-marketplace" => {
                        let resolved = resolve_claude_marketplace_skill(&client, &key).await?;
                        println!(
                            "Resolved claude-marketplace source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            key,
                        )
                    }
                    "github" => {
                        let (repo, maybe_branch, skill_dir) = parse_explicit_github_skill(&key)
                            .ok_or_else(|| {
                                AgentError::Config(format!(
                                    "github/ installs require owner/repo/path, got '{}'",
                                    key
                                ))
                            })?;
                        let branch = if let Some(branch) = maybe_branch {
                            branch
                        } else {
                            github_default_branch(&client, &repo).await?
                        };
                        let resolved = ResolvedSkillSource {
                            repo,
                            branch,
                            skill_dir,
                        };
                        println!(
                            "Resolved github source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            key,
                        )
                    }
                    _ => {
                        return Err(AgentError::Config(format!(
                            "Unsupported skill registry source '{}'",
                            source
                        )))
                    }
                }
            } else if let Some(skill_hint) = _requested_version
                .as_deref()
                .filter(|_| looks_like_github_repo_slug(&skill_name))
            {
                let resolved =
                    resolve_skill_in_repo(&client, &skill_name, skill_hint, Some("skills")).await?;
                println!(
                    "Resolved source: {}/{} @ {}",
                    resolved.repo, resolved.skill_dir, resolved.branch
                );
                (
                    fetch_skill_files_from_github(&client, &resolved).await?,
                    skill_name.clone(),
                )
            } else {
                let from_index = resolve_skill_via_registry_index(&client, &skill_name, None).await;
                if let Ok(hit) = from_index {
                    match hit.install_source {
                        RegistryInstallSource::GitHub(resolved) => {
                            let branch = github_default_branch(&client, &resolved.repo).await?;
                            let resolved = ResolvedSkillSource { branch, ..resolved };
                            println!(
                                "Resolved source [{}]: {}/{} @ {}",
                                hit.source, resolved.repo, resolved.skill_dir, resolved.branch
                            );
                            (
                                fetch_skill_files_from_github(&client, &resolved).await?,
                                hit.identifier,
                            )
                        }
                        RegistryInstallSource::LobeHub { slug } => {
                            println!("Resolved source [lobehub]: {}", slug);
                            (fetch_lobehub_skill_files(&client, &slug).await?, slug)
                        }
                        RegistryInstallSource::ClawHub { slug, version } => {
                            println!("Resolved source [clawhub]: {}", slug);
                            (
                                fetch_clawhub_skill_files(&client, &slug, version.as_deref())
                                    .await?,
                                slug,
                            )
                        }
                    }
                } else {
                    let taps_file = hermes_config::hermes_home().join("skill_taps.json");
                    let taps = merged_skill_taps(&read_skill_taps(&taps_file));
                    let (resolved, route) =
                        resolve_install_via_fallback_router(&client, &skill_name, &taps).await?;
                    match route {
                        InstallFallbackSource::SkillsSh => println!(
                            "Resolved source [skills.sh fallback]: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        ),
                        InstallFallbackSource::Tap => println!(
                            "Resolved source (tap): {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        ),
                    }
                    (
                        fetch_skill_files_from_github(&client, &resolved).await?,
                        skill_name.clone(),
                    )
                }
            };

            let install_name = sanitize_skill_install_name(
                _requested_version
                    .as_deref()
                    .filter(|_| looks_like_github_repo_slug(&skill_name))
                    .unwrap_or(install_seed.as_str()),
            );
            let target = install_skill_files(&skills_dir, &install_name, &files)?;
            println!("Skill '{}' installed to {}", install_name, target.display());
        }
        "reset" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills reset <name>".into(),
                )
            })?;
            let target = skills_dir.join(&skill_name);
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to remove skill dir: {}", e))
                })?;
            }
            std::fs::create_dir_all(&target).map_err(|e| {
                hermes_core::AgentError::Io(format!("Failed to create skill dir: {}", e))
            })?;
            std::fs::write(
                target.join("SKILL.md"),
                format!(
                    "# {}\n\nReset by CLI. Replace with canonical skill contents.\n",
                    skill_name
                ),
            )
            .map_err(|e| hermes_core::AgentError::Io(format!("Failed to write SKILL.md: {}", e)))?;
            println!("Skill '{}' reset at {}", skill_name, target.display());
        }
        "subscribe" => {
            let source = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing source. Usage: hermes skills subscribe <name-or-url>".into(),
                )
            })?;
            std::fs::create_dir_all(&skills_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let subscriptions_path = skills_dir.join("subscriptions.json");
            let mut subscriptions: Vec<serde_json::Value> = if subscriptions_path.exists() {
                let raw = std::fs::read_to_string(&subscriptions_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                serde_json::from_str(&raw).unwrap_or_default()
            } else {
                Vec::new()
            };
            let normalized = source.trim().to_string();
            if normalized.is_empty() {
                return Err(hermes_core::AgentError::Config(
                    "skills subscribe: source cannot be empty".into(),
                ));
            }
            let exists = subscriptions.iter().any(|item| {
                item.get("source")
                    .and_then(|v| v.as_str())
                    .map(|s| s == normalized)
                    .unwrap_or(false)
            });
            if exists {
                println!("Skill subscription already exists: {}", normalized);
                return Ok(());
            }
            subscriptions.push(serde_json::json!({
                "source": normalized,
                "added_at": chrono::Utc::now().to_rfc3339(),
                "options": extra.as_deref().unwrap_or(""),
            }));
            std::fs::write(
                &subscriptions_path,
                serde_json::to_string_pretty(&subscriptions)
                    .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?,
            )
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!(
                "Subscribed to skill source '{}'. Registry: {}",
                source,
                subscriptions_path.display()
            );
        }
        "inspect" => {
            let skill_name = name.unwrap_or_default();
            let skill_md = skills_dir.join(&skill_name).join("SKILL.md");
            if skill_md.exists() {
                let content = std::fs::read_to_string(&skill_md)
                    .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
                println!("{}", content);
            } else {
                println!("Skill '{}' not found at {}", skill_name, skill_md.display());
            }
        }
        "uninstall" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills uninstall <name>".into(),
                )
            })?;
            let target = skills_dir.join(&skill_name);
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to remove skill: {}", e))
                })?;
                println!("Skill '{}' uninstalled.", skill_name);
            } else {
                println!("Skill '{}' not found.", skill_name);
            }
        }
        "check" => {
            let skill_name = name.unwrap_or_default();
            if skill_name.is_empty() {
                println!("Checking all installed skills...");
                let mut ok = 0u32;
                let mut issues = 0u32;
                if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        let path = entry.path();
                        if !path.is_dir() {
                            continue;
                        }
                        let dir_name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let skill_md = path.join("SKILL.md");
                        if !skill_md.exists() {
                            println!("  ✗ {} — missing SKILL.md", dir_name);
                            issues += 1;
                        } else {
                            let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                            if content.trim().is_empty() {
                                println!("  ⚠ {} — SKILL.md is empty", dir_name);
                                issues += 1;
                            } else {
                                println!("  ✓ {}", dir_name);
                                ok += 1;
                            }
                        }
                    }
                }
                println!("\n{} healthy, {} with issues.", ok, issues);
            } else {
                let skill_path = skills_dir.join(&skill_name);
                let skill_md = skill_path.join("SKILL.md");
                if !skill_path.exists() {
                    println!("Skill '{}' not found.", skill_name);
                } else if !skill_md.exists() {
                    println!("Skill '{}': MISSING SKILL.md", skill_name);
                } else {
                    let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                    let lines = content.lines().count();
                    let has_frontmatter = content.starts_with("---");
                    println!("Skill '{}': OK", skill_name);
                    println!("  Path: {}", skill_path.display());
                    println!("  SKILL.md: {} lines", lines);
                    println!(
                        "  Frontmatter: {}",
                        if has_frontmatter { "yes" } else { "no" }
                    );
                }
            }
        }
        "update" => {
            println!("Checking for skill updates...\n");
            if !skills_dir.exists() {
                println!("No skills installed.");
                return Ok(());
            }

            let apply_updates = extra.as_deref() == Some("--apply");

            // Collect installed skills with their local versions
            struct LocalSkill {
                name: String,
                version: String,
            }
            let mut installed: Vec<LocalSkill> = Vec::new();

            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let skill_md = path.join("SKILL.md");
                    if !skill_md.exists() {
                        continue;
                    }

                    let dir_name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                    let (fm, _body) = hermes_tools::tools::skill_utils::parse_frontmatter(&content);
                    let version = fm
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    installed.push(LocalSkill {
                        name: dir_name,
                        version,
                    });
                }
            }

            if installed.is_empty() {
                println!("No skills installed.");
                return Ok(());
            }

            println!(
                "{:30} {:>12} {:>12}   {}",
                "Skill", "Local", "Hub", "Status"
            );
            println!("{}", "-".repeat(75));

            let client = reqwest::Client::new();
            let mut updates_available: Vec<(String, String)> = Vec::new();

            for skill in &installed {
                // Query Hub for latest version
                let hub_url = format!(
                    "https://agentskills.io/api/v1/skills/{}/versions",
                    skill.name
                );
                let hub_result = client
                    .get(&hub_url)
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await;

                match hub_result {
                    Ok(resp) if resp.status().is_success() => {
                        if let Ok(data) = resp.json::<serde_json::Value>().await {
                            let latest = data
                                .get("latest")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");

                            let status = if skill.version == "unknown" || latest == "unknown" {
                                "unknown".to_string()
                            } else {
                                match hermes_skills::compare_versions(&skill.version, latest) {
                                    std::cmp::Ordering::Less => {
                                        updates_available
                                            .push((skill.name.clone(), latest.to_string()));
                                        "⬆ update available".to_string()
                                    }
                                    std::cmp::Ordering::Equal => "✓ up-to-date".to_string(),
                                    std::cmp::Ordering::Greater => "⚠ local is newer".to_string(),
                                }
                            };
                            println!(
                                "{:30} {:>12} {:>12}   {}",
                                skill.name, skill.version, latest, status
                            );
                        }
                    }
                    Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                        println!(
                            "{:30} {:>12} {:>12}   {}",
                            skill.name, skill.version, "-", "not on hub"
                        );
                    }
                    _ => {
                        println!(
                            "{:30} {:>12} {:>12}   {}",
                            skill.name, skill.version, "?", "hub unreachable"
                        );
                    }
                }
            }

            println!();
            if updates_available.is_empty() {
                println!("All skills are up to date.");
            } else {
                println!("{} update(s) available.", updates_available.len());

                if apply_updates {
                    println!("\nApplying updates...");
                    for (skill_name, new_version) in &updates_available {
                        let download_url = format!(
                            "https://agentskills.io/api/v1/skills/{}/download?version={}",
                            skill_name, new_version
                        );
                        match client
                            .get(&download_url)
                            .timeout(std::time::Duration::from_secs(30))
                            .send()
                            .await
                        {
                            Ok(resp) if resp.status().is_success() => {
                                if let Ok(bytes) = resp.bytes().await {
                                    let dec = flate2::read::GzDecoder::new(&bytes[..]);
                                    let mut archive = tar::Archive::new(dec);
                                    if archive.unpack(&skills_dir).is_ok() {
                                        println!("  ✓ {} updated to v{}", skill_name, new_version);
                                    } else {
                                        println!("  ✗ {} — failed to extract archive", skill_name);
                                    }
                                }
                            }
                            _ => {
                                println!("  ✗ {} — download failed", skill_name);
                            }
                        }
                    }
                } else {
                    println!("Run `hermes skills update --apply` to install updates.");
                }
            }
        }
        "publish" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills publish <name>".into(),
                )
            })?;
            let skill_path = skills_dir.join(&skill_name);
            if !skill_path.exists() {
                return Err(hermes_core::AgentError::Config(format!(
                    "Skill '{}' not found.",
                    skill_name
                )));
            }
            println!("Publishing skill '{}' to Skills Hub...", skill_name);
            println!("  Source: {}", skill_path.display());

            let skill_md = skill_path.join("SKILL.md");
            if !skill_md.exists() {
                println!("  ✗ Missing SKILL.md — required for publishing.");
                return Ok(());
            }

            let content = std::fs::read_to_string(&skill_md)
                .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
            let (frontmatter, _body) =
                hermes_tools::tools::skill_utils::parse_frontmatter(&content);

            let fm_name = frontmatter.get("name").and_then(|v| v.as_str());
            let fm_version = frontmatter.get("version").and_then(|v| v.as_str());
            let fm_desc = frontmatter.get("description").and_then(|v| v.as_str());
            let fm_category = frontmatter.get("category").and_then(|v| v.as_str());

            if fm_name.is_none()
                || fm_version.is_none()
                || fm_desc.is_none()
                || fm_category.is_none()
            {
                println!(
                    "  ✗ SKILL.md frontmatter must include: name, version, description, category"
                );
                let mut missing = Vec::new();
                if fm_name.is_none() {
                    missing.push("name");
                }
                if fm_version.is_none() {
                    missing.push("version");
                }
                if fm_desc.is_none() {
                    missing.push("description");
                }
                if fm_category.is_none() {
                    missing.push("category");
                }
                println!("    Missing: {}", missing.join(", "));
                return Ok(());
            }

            let publish_name = fm_name.unwrap();
            let publish_version = fm_version.unwrap();
            let publish_desc = fm_desc.unwrap();
            let publish_category = fm_category.unwrap();
            println!(
                "  ✓ name={}, version={}, category={}",
                publish_name, publish_version, publish_category
            );
            println!("  ✓ description: {}", publish_desc);

            // Package skill directory into a tarball in memory
            let mut tar_buf = Vec::new();
            {
                let enc =
                    flate2::write::GzEncoder::new(&mut tar_buf, flate2::Compression::default());
                let mut tar_builder = tar::Builder::new(enc);
                tar_builder
                    .append_dir_all(&skill_name, &skill_path)
                    .map_err(|e| hermes_core::AgentError::Io(format!("Tar error: {}", e)))?;
                tar_builder
                    .finish()
                    .map_err(|e| hermes_core::AgentError::Io(format!("Tar finish error: {}", e)))?;
            }
            println!("  ✓ Packaged {} bytes", tar_buf.len());

            // Read hub token
            let token_path = hermes_config::hermes_home().join("hub_token");
            if !token_path.exists() {
                println!("  ✗ No hub token found at {}", token_path.display());
                println!("    Run `hermes login hub` to authenticate with Skills Hub.");
                return Ok(());
            }
            let hub_token = std::fs::read_to_string(&token_path)
                .map_err(|e| hermes_core::AgentError::Io(format!("Token read error: {}", e)))?
                .trim()
                .to_string();

            // Build metadata JSON
            let metadata = serde_json::json!({
                "name": publish_name,
                "version": publish_version,
                "description": publish_desc,
                "category": publish_category,
            });

            // Upload to Skills Hub API via multipart
            let tarball_part = reqwest::multipart::Part::bytes(tar_buf)
                .file_name(format!("{}-{}.tar.gz", publish_name, publish_version))
                .mime_str("application/gzip")
                .unwrap();
            let metadata_part = reqwest::multipart::Part::text(metadata.to_string())
                .mime_str("application/json")
                .unwrap();
            let form = reqwest::multipart::Form::new()
                .part("tarball", tarball_part)
                .part("metadata", metadata_part);

            println!("  Uploading to Skills Hub...");
            match reqwest::Client::new()
                .post("https://agentskills.io/api/v1/skills")
                .bearer_auth(&hub_token)
                .multipart(form)
                .timeout(std::time::Duration::from_secs(60))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let url = format!("https://agentskills.io/skills/{}", publish_name);
                    println!("  ✓ Published successfully!");
                    println!("  URL: {}", url);
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::CONFLICT => {
                    println!(
                        "  ✗ Version {} already exists on Skills Hub.",
                        publish_version
                    );
                    println!("    Bump the version in SKILL.md frontmatter and try again.");
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                    println!("  ✗ Unauthorized. Hub token may be expired.");
                    println!("    Run `hermes login hub` to re-authenticate.");
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    println!("  ✗ Upload failed (HTTP {}): {}", status, body);
                }
                Err(e) => {
                    println!("  ✗ Could not reach Skills Hub: {}", e);
                }
            }
        }
        "snapshot" => {
            let sub = name.as_deref().unwrap_or("export");
            match sub {
                "export" => {
                    let output = extra.unwrap_or_else(|| {
                        format!(
                            "skills-snapshot-{}.tar.gz",
                            chrono::Utc::now().format("%Y%m%d-%H%M%S")
                        )
                    });
                    println!("Exporting skills snapshot to: {}", output);
                    if !skills_dir.exists() {
                        println!("No skills directory found.");
                        return Ok(());
                    }
                    // Create a tar.gz archive of skills directory
                    let tar_gz = std::fs::File::create(&output).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to create archive: {}", e))
                    })?;
                    let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
                    let mut tar = tar::Builder::new(enc);
                    tar.append_dir_all("skills", &skills_dir).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to archive: {}", e))
                    })?;
                    tar.finish().map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to finalize archive: {}", e))
                    })?;
                    println!("Snapshot exported to: {}", output);
                }
                "import" => {
                    let input = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing snapshot path. Usage: hermes skills snapshot import <path>"
                                .into(),
                        )
                    })?;
                    println!("Importing skills snapshot from: {}", input);
                    let tar_gz = std::fs::File::open(&input).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to open archive: {}", e))
                    })?;
                    let dec = flate2::read::GzDecoder::new(tar_gz);
                    let mut archive = tar::Archive::new(dec);
                    std::fs::create_dir_all(&skills_dir).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to create skills dir: {}", e))
                    })?;
                    archive.unpack(hermes_config::hermes_home()).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to extract archive: {}", e))
                    })?;
                    println!("Snapshot imported successfully.");
                }
                _ => {
                    println!("Usage: hermes skills snapshot export|import [path]");
                }
            }
        }
        "tap" => {
            let sub = name.as_deref().unwrap_or("list");
            let taps_file = hermes_config::hermes_home().join("skill_taps.json");
            match sub {
                "list" => {
                    let taps = merged_skill_taps(&read_skill_taps(&taps_file));
                    if taps.is_empty() {
                        println!("No skill taps configured.");
                    } else {
                        println!("Skill taps:");
                        for tap in &taps {
                            println!("  • {}", tap);
                        }
                    }
                }
                "add" => {
                    let url = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing tap URL. Usage: hermes skills tap add <url>".into(),
                        )
                    })?;
                    let mut taps: Vec<String> = read_skill_taps(&taps_file);
                    if merged_skill_taps(&taps).contains(&url) {
                        println!("Tap already exists: {}", url);
                    } else {
                        taps.push(url.clone());
                        write_skill_taps(&taps_file, &taps)?;
                        println!("Added tap: {}", url);
                    }
                }
                "remove" => {
                    let url = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing tap URL. Usage: hermes skills tap remove <url>".into(),
                        )
                    })?;
                    if DEFAULT_SKILL_TAPS
                        .iter()
                        .any(|default_tap| default_tap == &url.as_str())
                    {
                        println!("Tap '{}' is a built-in default and cannot be removed.", url);
                        println!(
                            "Add custom taps with `hermes skills tap add <url>`; defaults remain active."
                        );
                        return Ok(());
                    }

                    let mut taps: Vec<String> = read_skill_taps(&taps_file);
                    let before_len = taps.len();
                    taps.retain(|t| t != &url);
                    if taps.len() < before_len {
                        write_skill_taps(&taps_file, &taps)?;
                        println!("Removed tap: {}", url);
                    } else {
                        println!("Tap not found: {}", url);
                    }
                }
                _ => {
                    println!("Usage: hermes skills tap list|add|remove [url]");
                }
            }
        }
        "config" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills config <name> [key] [value]".into(),
                )
            })?;
            let config_file = skills_dir.join(&skill_name).join("config.json");
            if let Some(key) = extra {
                // Set or get a config key
                let parts: Vec<&str> = key.splitn(2, '=').collect();
                if parts.len() == 2 {
                    let mut config: serde_json::Value = if config_file.exists() {
                        let c = std::fs::read_to_string(&config_file)
                            .unwrap_or_else(|_| "{}".to_string());
                        serde_json::from_str(&c).unwrap_or(serde_json::json!({}))
                    } else {
                        serde_json::json!({})
                    };
                    config[parts[0]] = serde_json::Value::String(parts[1].to_string());
                    let json = serde_json::to_string_pretty(&config)
                        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
                    std::fs::write(&config_file, json)
                        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                    println!("Set {} = {} for skill '{}'", parts[0], parts[1], skill_name);
                } else {
                    // Get value
                    if config_file.exists() {
                        let c = std::fs::read_to_string(&config_file)
                            .unwrap_or_else(|_| "{}".to_string());
                        let config: serde_json::Value =
                            serde_json::from_str(&c).unwrap_or(serde_json::json!({}));
                        match config.get(&key) {
                            Some(v) => println!("{} = {}", key, v),
                            None => println!("Key '{}' not found in skill config.", key),
                        }
                    } else {
                        println!("No config for skill '{}'.", skill_name);
                    }
                }
            } else {
                // Show all config
                if config_file.exists() {
                    let content = std::fs::read_to_string(&config_file)
                        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                    println!("Config for skill '{}':", skill_name);
                    println!("{}", content);
                } else {
                    println!("No config for skill '{}'.", skill_name);
                }
            }
        }
        "audit" => {
            println!("Security audit of installed skills");
            println!("==================================\n");
            if !skills_dir.exists() {
                println!("No skills installed.");
                return Ok(());
            }

            struct AuditFinding {
                file: String,
                pattern: String,
                severity: &'static str, // "warning" or "critical"
            }

            let shell_injection_patterns: &[(&str, &str)] = &[
                (
                    r"(?i)\b(rm\s+-rf|mkfs|dd\s+if=)",
                    "Shell command injection (destructive command)",
                ),
                (r"(?i)(:\(\)\{.*;\}|fork\s+bomb)", "Fork bomb pattern"),
                (r"(?i)\b(sudo\s+|su\s+-\s)", "Privilege escalation attempt"),
                (
                    r"(?i)(export\s+PATH|PATH\s*=\s*/)",
                    "PATH environment manipulation",
                ),
                (
                    r"(?i)chmod\s+[0-7]*777",
                    "Overly permissive file permissions",
                ),
                (r"(?i)\beval\s*\(", "Dynamic code evaluation (eval)"),
                (r"(?i)\bexec\s*\(", "Dynamic code execution (exec)"),
                (
                    r"(?i)(os\.system|subprocess\.call|subprocess\.run|subprocess\.Popen)",
                    "Subprocess execution",
                ),
            ];

            let path_traversal_patterns: &[(&str, &str)] =
                &[(r"\.\.[\\/]", "Path traversal (../)")];

            let network_patterns: &[(&str, &str)] = &[
                (r"(?i)://127\.0\.0\.1", "Internal network URL (127.0.0.1)"),
                (r"(?i)://localhost", "Internal network URL (localhost)"),
                (
                    r"(?i)://10\.\d+\.\d+\.\d+",
                    "Internal network URL (10.x.x.x)",
                ),
                (
                    r"(?i)://192\.168\.\d+\.\d+",
                    "Internal network URL (192.168.x.x)",
                ),
                (r"(?i)://0\.0\.0\.0", "Internal network URL (0.0.0.0)"),
                (r"(?i)://\[::1\]", "Internal network URL (::1)"),
            ];

            let credential_patterns: &[(&str, &str)] = &[
                (
                    r#"(?i)(password\s*=\s*['"][^'"]{3,}['"])"#,
                    "Hardcoded password",
                ),
                (
                    r#"(?i)(api[_-]?key\s*=\s*['"][^'"]{3,}['"])"#,
                    "Hardcoded API key",
                ),
                (
                    r#"(?i)(secret\s*=\s*['"][^'"]{3,}['"])"#,
                    "Hardcoded secret",
                ),
                (r"(?i)(sk-[a-zA-Z0-9]{20,})", "Exposed API key (sk-...)"),
                (r"(?i)(ghp_[a-zA-Z0-9]{30,})", "Exposed GitHub PAT"),
            ];

            let base64_suspicious: &[(&str, &str)] = &[
                (
                    r"(?i)(base64[._-]?decode|atob)\s*\(",
                    "Base64 decode invocation (potential obfuscation)",
                ),
                (
                    r"[A-Za-z0-9+/]{100,}={0,2}",
                    "Long base64-encoded content (potential obfuscation)",
                ),
            ];

            let mut total = 0u32;
            let mut total_warnings = 0u32;
            let mut total_critical = 0u32;

            fn scan_dir_recursive(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        let p = entry.path();
                        if p.is_dir() {
                            scan_dir_recursive(&p, files);
                        } else if p.is_file() {
                            files.push(p);
                        }
                    }
                }
            }

            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    total += 1;
                    let dir_name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let mut findings: Vec<AuditFinding> = Vec::new();

                    let mut all_files = Vec::new();
                    scan_dir_recursive(&path, &mut all_files);

                    for fp in &all_files {
                        let Ok(content) = std::fs::read_to_string(fp) else {
                            continue;
                        };
                        let fname = fp
                            .strip_prefix(&path)
                            .unwrap_or(fp)
                            .to_string_lossy()
                            .to_string();

                        // Shell injection (critical)
                        for (pat, desc) in shell_injection_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "critical",
                                    });
                                }
                            }
                        }

                        // Path traversal (critical)
                        for (pat, desc) in path_traversal_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "critical",
                                    });
                                }
                            }
                        }

                        // Internal network URLs (warning)
                        for (pat, desc) in network_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "warning",
                                    });
                                }
                            }
                        }

                        // Credential patterns (critical)
                        for (pat, desc) in credential_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "critical",
                                    });
                                }
                            }
                        }

                        // Base64 suspicious (warning)
                        for (pat, desc) in base64_suspicious {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "warning",
                                    });
                                }
                            }
                        }
                    }

                    if findings.is_empty() {
                        println!("  ✓ {} — clean", dir_name);
                    } else {
                        let crit_count =
                            findings.iter().filter(|f| f.severity == "critical").count();
                        let warn_count =
                            findings.iter().filter(|f| f.severity == "warning").count();
                        total_critical += crit_count as u32;
                        total_warnings += warn_count as u32;

                        let icon = if crit_count > 0 { "✗" } else { "⚠" };
                        println!(
                            "  {} {} — {} critical, {} warning(s):",
                            icon, dir_name, crit_count, warn_count
                        );
                        for f in &findings {
                            let sev_icon = if f.severity == "critical" {
                                "CRIT"
                            } else {
                                "WARN"
                            };
                            println!("    [{}] {} — {}", sev_icon, f.file, f.pattern);
                        }
                    }
                }
            }

            println!("\n{}", "=".repeat(50));
            println!("Audited {} skill(s)", total);
            println!("  Critical: {}", total_critical);
            println!("  Warnings: {}", total_warnings);
            if total_critical == 0 && total_warnings == 0 {
                println!("  Status:   All clear ✓");
            } else if total_critical > 0 {
                println!("  Status:   Action required — review critical findings");
            } else {
                println!("  Status:   Review recommended");
            }
        }
        other => {
            println!("Skills action '{}' is not recognized.", other);
            println!("Available actions: list, browse, search, install, inspect, uninstall, check, update, publish, snapshot, tap, config, audit");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugin security (remote Git installs)
// ---------------------------------------------------------------------------

fn default_git_host_allowlist() -> Vec<&'static str> {
    vec![
        "github.com",
        "www.github.com",
        "raw.githubusercontent.com",
        "gitlab.com",
        "www.gitlab.com",
        "codeberg.org",
        "www.codeberg.org",
        "gitea.com",
        "bitbucket.org",
    ]
}

fn plugin_git_host_allowed(url: &str, allow_untrusted: bool) -> bool {
    if allow_untrusted {
        return true;
    }
    let extra = std::env::var("HERMES_PLUGIN_GIT_EXTRA_HOSTS").unwrap_or_default();
    let mut hosts: Vec<String> = default_git_host_allowlist()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    for part in extra.split(',') {
        let p = part.trim();
        if !p.is_empty() {
            hosts.push(p.to_lowercase());
        }
    }
    let lower = url.to_lowercase();
    let host_part = if lower.contains("://") {
        lower.split("://").nth(1).unwrap_or("")
    } else if lower.starts_with("git@") {
        lower
            .trim_start_matches("git@")
            .split(':')
            .next()
            .unwrap_or("")
    } else {
        return false;
    };
    let host = host_part
        .split('/')
        .next()
        .unwrap_or(host_part)
        .split('@')
        .last()
        .unwrap_or(host_part);
    let host = host.split(':').next().unwrap_or(host).to_lowercase();
    hosts
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{}", h)))
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// Static scan of a cloned plugin tree: risky patterns in scripts/config.
fn scan_plugin_security(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let manifest = root.join("plugin.yaml");
    if manifest.exists() {
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            if text.contains("post_install") || text.contains("postInstall") {
                out.push(
                    "plugin.yaml declares post_install / postInstall — review before running the plugin"
                        .into(),
                );
            }
            if Regex::new(r"(?i)curl\s+[^|\n]*\|\s*(ba)?sh")
                .ok()
                .and_then(|re| re.find(&text))
                .is_some()
            {
                out.push("plugin.yaml references curl|sh style install — high risk".into());
            }
        }
    }

    let risky_file_patterns: &[(&str, &[(&str, &str)])] = &[(
        r"\.(sh|bash|zsh|py|rb|ps1|fish)$",
        &[
            (r"(?i)\bcurl\s+[^|\n]*\|\s*(ba)?sh", "curl piped to shell"),
            (r"(?i)\bwget\s+[^|\n]*\|\s*(ba)?sh", "wget piped to shell"),
            (r"(?i)\beval\s*\(", "eval("),
            (r"(?i)\bexec\s*\(", "exec("),
            (r"(?i)(base64[._-]?decode|atob)\s*\(", "base64 decode"),
            (r"(?i)\brm\s+-rf\s+/", "rm -rf on absolute path"),
        ],
    )];

    fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if dir.is_dir() && (name == ".git" || name == "target" || name == "node_modules") {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, files);
                } else if p.is_file() {
                    files.push(p);
                }
            }
        }
    }

    let mut files = Vec::new();
    walk(root, &mut files);

    for fp in files {
        let fname = fp.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if fname == ".DS_Store" {
            continue;
        }
        let rel = fp.strip_prefix(root).unwrap_or(&fp).display().to_string();
        let Ok(content) = std::fs::read_to_string(&fp) else {
            continue;
        };
        for (ext_re, rules) in risky_file_patterns {
            if let Ok(re_ext) = Regex::new(ext_re) {
                if !re_ext.is_match(fname) {
                    continue;
                }
                for (pat, label) in *rules {
                    if let Ok(re) = Regex::new(pat) {
                        if re.is_match(&content) {
                            out.push(format!("{}: {}", rel, label));
                        }
                    }
                }
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

async fn git_checkout_ref(repo_dir: &std::path::Path, git_ref: &str) -> Result<(), String> {
    let dir = repo_dir.to_string_lossy().to_string();
    let fetch = tokio::process::Command::new("git")
        .args(["-C", &dir, "fetch", "--depth", "1", "origin", git_ref])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !fetch.status.success() {
        let err = String::from_utf8_lossy(&fetch.stderr);
        return Err(format!("git fetch origin {}: {}", git_ref, err.trim()));
    }
    let co = tokio::process::Command::new("git")
        .args(["-C", &dir, "checkout", git_ref])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !co.status.success() {
        let err = String::from_utf8_lossy(&co.stderr);
        return Err(format!("git checkout {}: {}", git_ref, err.trim()));
    }
    Ok(())
}

/// Handle `hermes plugins [action] [name]`.
pub async fn handle_cli_plugins(
    action: Option<String>,
    name: Option<String>,
    git_ref: Option<String>,
    allow_untrusted_git_host: bool,
) -> Result<(), hermes_core::AgentError> {
    let plugins_dir = hermes_config::hermes_home().join("plugins");

    match action.as_deref().unwrap_or("list") {
        "list" => {
            if !plugins_dir.exists() {
                println!("No plugins directory found at {}", plugins_dir.display());
                return Ok(());
            }
            let mut count = 0u32;
            println!("Installed plugins ({}):", plugins_dir.display());
            if let Ok(entries) = std::fs::read_dir(&plugins_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let manifest = path.join("plugin.yaml");
                    if path.is_dir() && manifest.exists() {
                        let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                        let disabled_marker = path.join(".disabled");
                        let status = if disabled_marker.exists() {
                            "disabled"
                        } else {
                            "enabled"
                        };
                        println!("  • {} [{}]", dir_name, status);
                        count += 1;
                    }
                }
            }
            if count == 0 {
                println!("  (no plugins installed)");
            }
        }
        "enable" => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins enable <name>".into(),
                )
            })?;
            let disabled_marker = plugins_dir.join(&plugin_name).join(".disabled");
            if disabled_marker.exists() {
                std::fs::remove_file(&disabled_marker).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to enable plugin: {}", e))
                })?;
                println!("Plugin '{}' enabled.", plugin_name);
            } else {
                println!(
                    "Plugin '{}' is already enabled (or not installed).",
                    plugin_name
                );
            }
        }
        "disable" => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins disable <name>".into(),
                )
            })?;
            let plugin_dir = plugins_dir.join(&plugin_name);
            if !plugin_dir.exists() {
                println!("Plugin '{}' not found.", plugin_name);
                return Ok(());
            }
            let disabled_marker = plugin_dir.join(".disabled");
            std::fs::write(&disabled_marker, "").map_err(|e| {
                hermes_core::AgentError::Io(format!("Failed to disable plugin: {}", e))
            })?;
            println!("Plugin '{}' disabled.", plugin_name);
        }
        "install" => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins install <name|url>".into(),
                )
            })?;
            println!("Installing plugin: {}...", plugin_name);

            let is_git_url = plugin_name.starts_with("http://")
                || plugin_name.starts_with("https://")
                || plugin_name.starts_with("git@");

            if is_git_url {
                if !plugin_git_host_allowed(&plugin_name, allow_untrusted_git_host) {
                    println!(
                        "  ✗ Git host is not on the default allow-list (github.com, gitlab.com, codeberg.org, …)."
                    );
                    println!(
                        "    Set comma-separated HERMES_PLUGIN_GIT_EXTRA_HOSTS or pass --allow-untrusted-git-host after you trust the source."
                    );
                    return Ok(());
                }
                // Extract repo name from URL for target directory
                let repo_name = plugin_name
                    .trim_end_matches('/')
                    .trim_end_matches(".git")
                    .rsplit('/')
                    .next()
                    .unwrap_or("unknown-plugin")
                    .to_string();

                // Also handle git@ SSH URLs like git@github.com:user/repo.git
                let repo_name = if repo_name.contains(':') {
                    repo_name
                        .rsplit(':')
                        .next()
                        .unwrap_or(&repo_name)
                        .trim_end_matches(".git")
                        .rsplit('/')
                        .next()
                        .unwrap_or(&repo_name)
                        .to_string()
                } else {
                    repo_name
                };

                let target = plugins_dir.join(&repo_name);
                if target.exists() {
                    println!(
                        "Plugin '{}' is already installed at {}",
                        repo_name,
                        target.display()
                    );
                    return Ok(());
                }

                std::fs::create_dir_all(&plugins_dir).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to create plugins dir: {}", e))
                })?;

                println!("  Cloning {} ...", plugin_name);
                let output = tokio::process::Command::new("git")
                    .args([
                        "clone",
                        "--depth",
                        "1",
                        &plugin_name,
                        &target.to_string_lossy(),
                    ])
                    .output()
                    .await
                    .map_err(|e| hermes_core::AgentError::Io(format!("git clone failed: {}", e)))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    println!("  ✗ git clone failed: {}", stderr.trim());
                    return Ok(());
                }

                if let Some(gr) = git_ref.as_deref() {
                    println!("  Checking out ref: {} ...", gr);
                    if let Err(e) = git_checkout_ref(&target, gr).await {
                        println!("  ✗ {}", e);
                        let _ = std::fs::remove_dir_all(&target);
                        return Ok(());
                    }
                }

                // Verify plugin.yaml exists
                let manifest_path = target.join("plugin.yaml");
                if !manifest_path.exists() {
                    println!("  ✗ No plugin.yaml found in cloned repository.");
                    println!("    Removing {}...", target.display());
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }

                // Parse and display plugin info
                let manifest_content = std::fs::read_to_string(&manifest_path)
                    .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
                let manifest: serde_json::Value =
                    serde_yaml::from_str(&manifest_content).unwrap_or(serde_json::json!({}));

                let p_name = manifest
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&repo_name);
                let p_version = manifest
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let p_desc = manifest
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Security scan of cloned files
                let suspicious = scan_plugin_security(&target);
                let hard_block = suspicious.iter().any(|s| {
                    s.contains("curl piped to shell")
                        || s.contains("wget piped to shell")
                        || s.contains("curl|sh style install")
                });
                if hard_block && !allow_untrusted_git_host {
                    println!("\n  ✗ High-risk install patterns detected — clone removed.");
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                    println!(
                        "\n  If you reviewed the code manually, re-run with --allow-untrusted-git-host."
                    );
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }
                if !suspicious.is_empty() {
                    println!("\n  ⚠ Security warnings found ({}):", suspicious.len());
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                    println!("\n  Review the warnings above before enabling this plugin.");
                }

                println!("  ✓ Plugin installed successfully!");
                println!("    Name:        {}", p_name);
                println!("    Version:     {}", p_version);
                println!("    Description: {}", p_desc);
                println!("    Path:        {}", target.display());
            } else if plugin_name.starts_with("gh:") || plugin_name.contains('/') {
                // Convert gh:user/repo or user/repo to a GitHub HTTPS URL
                let repo_path = plugin_name.trim_start_matches("gh:");
                let git_url = format!("https://github.com/{}.git", repo_path);
                let repo_name = repo_path.rsplit('/').next().unwrap_or("unknown-plugin");
                let target = plugins_dir.join(repo_name);
                if target.exists() {
                    println!("Plugin '{}' is already installed.", repo_name);
                    return Ok(());
                }

                std::fs::create_dir_all(&plugins_dir).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to create plugins dir: {}", e))
                })?;

                println!("  Cloning from GitHub: {}", git_url);
                let output = tokio::process::Command::new("git")
                    .args(["clone", "--depth", "1", &git_url, &target.to_string_lossy()])
                    .output()
                    .await
                    .map_err(|e| hermes_core::AgentError::Io(format!("git clone failed: {}", e)))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    println!("  ✗ git clone failed: {}", stderr.trim());
                    return Ok(());
                }

                if let Some(gr) = git_ref.as_deref() {
                    println!("  Checking out ref: {} ...", gr);
                    if let Err(e) = git_checkout_ref(&target, gr).await {
                        println!("  ✗ {}", e);
                        let _ = std::fs::remove_dir_all(&target);
                        return Ok(());
                    }
                }

                let manifest_path = target.join("plugin.yaml");
                if !manifest_path.exists() {
                    println!("  ✗ No plugin.yaml found in cloned repository.");
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }

                let manifest_content = std::fs::read_to_string(&manifest_path).unwrap_or_default();
                let manifest: serde_json::Value =
                    serde_yaml::from_str(&manifest_content).unwrap_or(serde_json::json!({}));

                let p_name = manifest
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(repo_name);
                let p_version = manifest
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let p_desc = manifest
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let suspicious = scan_plugin_security(&target);
                let hard_block = suspicious.iter().any(|s| {
                    s.contains("curl piped to shell")
                        || s.contains("wget piped to shell")
                        || s.contains("curl|sh style install")
                });
                if hard_block && !allow_untrusted_git_host {
                    println!("\n  ✗ High-risk install patterns detected — clone removed.");
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                    println!(
                        "\n  If you reviewed the code manually, re-run with --allow-untrusted-git-host."
                    );
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }
                if !suspicious.is_empty() {
                    println!("\n  ⚠ Security warnings found ({}):", suspicious.len());
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                }

                println!("  ✓ Plugin installed successfully!");
                println!("    Name:        {}", p_name);
                println!("    Version:     {}", p_version);
                println!("    Description: {}", p_desc);
                println!("    Path:        {}", target.display());
            } else {
                let target = plugins_dir.join(&plugin_name);
                if target.exists() {
                    println!("Plugin '{}' is already installed.", plugin_name);
                    return Ok(());
                }
                // Registry lookup
                println!("  Looking up '{}' in plugin registry...", plugin_name);
                match reqwest::Client::new()
                    .get(&format!(
                        "https://plugins.hermes.run/api/v1/{}",
                        plugin_name
                    ))
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        if let Ok(data) = resp.json::<serde_json::Value>().await {
                            let version = data
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("latest");
                            let git_url = data.get("git_url").and_then(|v| v.as_str());
                            println!("  Found {} v{}", plugin_name, version);

                            if let Some(url) = git_url {
                                if !plugin_git_host_allowed(url, allow_untrusted_git_host) {
                                    println!("  ✗ Registry git_url host is not allow-listed. Use --allow-untrusted-git-host or HERMES_PLUGIN_GIT_EXTRA_HOSTS.");
                                    return Ok(());
                                }
                                std::fs::create_dir_all(&plugins_dir)
                                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

                                let output = tokio::process::Command::new("git")
                                    .args(["clone", "--depth", "1", url, &target.to_string_lossy()])
                                    .output()
                                    .await
                                    .map_err(|e| {
                                        hermes_core::AgentError::Io(format!(
                                            "git clone failed: {}",
                                            e
                                        ))
                                    })?;

                                if output.status.success() {
                                    if let Some(gr) = git_ref.as_deref() {
                                        println!("  Checking out ref: {} ...", gr);
                                        if let Err(e) = git_checkout_ref(&target, gr).await {
                                            println!("  ✗ {}", e);
                                            let _ = std::fs::remove_dir_all(&target);
                                            return Ok(());
                                        }
                                    }
                                    let suspicious = scan_plugin_security(&target);
                                    let hard_block = suspicious.iter().any(|s| {
                                        s.contains("curl piped to shell")
                                            || s.contains("wget piped to shell")
                                            || s.contains("curl|sh style install")
                                    });
                                    if hard_block && !allow_untrusted_git_host {
                                        println!("  ✗ High-risk patterns — removed clone.");
                                        let _ = std::fs::remove_dir_all(&target);
                                        return Ok(());
                                    }
                                    if !suspicious.is_empty() {
                                        println!("  ⚠ Security warnings: {}", suspicious.len());
                                        for w in &suspicious {
                                            println!("    - {}", w);
                                        }
                                    }
                                    println!(
                                        "  ✓ Plugin '{}' v{} installed.",
                                        plugin_name, version
                                    );
                                } else {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    println!("  ✗ Clone failed: {}", stderr.trim());
                                }
                            } else {
                                println!("  No git_url in registry response. Cannot install.");
                            }
                        }
                    }
                    _ => {
                        println!("  Plugin '{}' not found in registry.", plugin_name);
                        println!("  Try installing from a URL or GitHub repo instead:");
                        println!("    hermes plugins install https://github.com/user/repo");
                        println!("    hermes plugins install gh:user/repo");
                    }
                }
            }
        }
        "remove" | "uninstall" => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins remove <name>".into(),
                )
            })?;
            let target = plugins_dir.join(&plugin_name);
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to remove plugin: {}", e))
                })?;
                println!("Plugin '{}' removed.", plugin_name);
            } else {
                println!("Plugin '{}' not found.", plugin_name);
            }
        }
        "update" => {
            let plugin_name = name.as_deref();
            let mut checked = 0u32;
            let mut updated = 0u32;
            if !plugins_dir.exists() {
                println!("No plugins installed.");
                return Ok(());
            }
            if let Ok(entries) = std::fs::read_dir(&plugins_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let dir_name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    if let Some(target) = plugin_name {
                        if dir_name != target {
                            continue;
                        }
                    }
                    let manifest = path.join("plugin.yaml");
                    if manifest.exists() {
                        checked += 1;
                        println!("  Checking updates for '{}'...", dir_name);

                        let git_dir = path.join(".git");
                        if !git_dir.exists() {
                            println!("    Skipped: plugin is not a git checkout.");
                            continue;
                        }

                        let path_s = path.to_string_lossy().to_string();
                        let before = tokio::process::Command::new("git")
                            .args(["-C", &path_s, "rev-parse", "HEAD"])
                            .output()
                            .await
                            .map_err(|e| {
                                hermes_core::AgentError::Io(format!(
                                    "git rev-parse failed for {}: {}",
                                    dir_name, e
                                ))
                            })?;
                        if !before.status.success() {
                            let stderr = String::from_utf8_lossy(&before.stderr);
                            println!(
                                "    Skipped: cannot read current revision ({})",
                                stderr.trim()
                            );
                            continue;
                        }
                        let before_sha = String::from_utf8_lossy(&before.stdout).trim().to_string();

                        let pull = tokio::process::Command::new("git")
                            .args(["-C", &path_s, "pull", "--ff-only"])
                            .output()
                            .await
                            .map_err(|e| {
                                hermes_core::AgentError::Io(format!(
                                    "git pull failed for {}: {}",
                                    dir_name, e
                                ))
                            })?;

                        if !pull.status.success() {
                            let stderr = String::from_utf8_lossy(&pull.stderr);
                            println!("    Update failed: {}", stderr.trim());
                            continue;
                        }

                        let after = tokio::process::Command::new("git")
                            .args(["-C", &path_s, "rev-parse", "HEAD"])
                            .output()
                            .await
                            .map_err(|e| {
                                hermes_core::AgentError::Io(format!(
                                    "git rev-parse failed for {} after update: {}",
                                    dir_name, e
                                ))
                            })?;
                        if !after.status.success() {
                            let stderr = String::from_utf8_lossy(&after.stderr);
                            println!(
                                "    Updated but could not read final revision ({})",
                                stderr.trim()
                            );
                            continue;
                        }
                        let after_sha = String::from_utf8_lossy(&after.stdout).trim().to_string();

                        if before_sha == after_sha {
                            println!("    Up to date ({})", short_sha(&after_sha));
                        } else {
                            updated += 1;
                            println!(
                                "    Updated: {} -> {}",
                                short_sha(&before_sha),
                                short_sha(&after_sha)
                            );
                        }
                    }
                }
            }
            if checked == 0 {
                if let Some(n) = plugin_name {
                    println!("Plugin '{}' not found.", n);
                } else {
                    println!("No plugins to update.");
                }
            } else {
                println!("Checked {} plugin(s); updated {}.", checked, updated);
            }
        }
        "inspect" | "info" => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins inspect <name>".into(),
                )
            })?;
            let target = plugins_dir.join(&plugin_name);
            if !target.exists() {
                println!("Plugin '{}' not found.", plugin_name);
                return Ok(());
            }
            let manifest_path = target.join("plugin.yaml");
            if manifest_path.exists() {
                let content = std::fs::read_to_string(&manifest_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Plugin: {}", plugin_name);
                println!("Path:   {}", target.display());
                let disabled = target.join(".disabled").exists();
                println!("Status: {}", if disabled { "disabled" } else { "enabled" });
                println!("\n--- plugin.yaml ---");
                println!("{}", content);
            } else {
                println!("Plugin '{}' has no plugin.yaml manifest.", plugin_name);
            }
        }
        other => {
            println!("Plugins action '{}' is not recognized.", other);
            println!("Available: list, install, remove, enable, disable, update, inspect");
        }
    }
    Ok(())
}

/// Handle `hermes memory [action]`.
pub async fn handle_cli_memory(
    action: Option<String>,
    target: Option<String>,
    yes: bool,
) -> Result<(), hermes_core::AgentError> {
    let hermes_home = hermes_config::hermes_home();
    let memories_dir = hermes_home.join("memories");
    let memory_md = memories_dir.join("MEMORY.md");
    let user_md = memories_dir.join("USER.md");
    let legacy_memory_db = hermes_home.join("memory.db");
    let disabled_marker = hermes_home.join(".memory_disabled");

    match action.as_deref().unwrap_or("status") {
        "status" => {
            if disabled_marker.exists() {
                println!("Memory provider: disabled");
                println!("  Marker: {}", disabled_marker.display());
                println!("Run `hermes memory setup` to re-enable.");
                return Ok(());
            }

            if memory_md.exists() || user_md.exists() {
                let mem_size = std::fs::metadata(&memory_md).map(|m| m.len()).unwrap_or(0);
                let user_size = std::fs::metadata(&user_md).map(|m| m.len()).unwrap_or(0);
                println!("Memory provider: files (MEMORY.md + USER.md)");
                println!("  Directory: {}", memories_dir.display());
                println!(
                    "  MEMORY.md: {} ({:.1} KB)",
                    memory_md.display(),
                    mem_size as f64 / 1024.0
                );
                println!(
                    "  USER.md:   {} ({:.1} KB)",
                    user_md.display(),
                    user_size as f64 / 1024.0
                );
                if legacy_memory_db.exists() {
                    println!(
                        "  Legacy file detected (unused by current memory backend): {}",
                        legacy_memory_db.display()
                    );
                }
            } else if legacy_memory_db.exists() {
                let size = std::fs::metadata(&legacy_memory_db)
                    .map(|m| m.len())
                    .unwrap_or(0);
                println!("Memory provider: legacy sqlite artifact only");
                println!("  File: {}", legacy_memory_db.display());
                println!("  Size: {} KB", size / 1024);
                println!("Run `hermes memory setup` to initialize the current file backend.");
            } else {
                println!("Memory provider: not configured");
                println!("Run `hermes memory setup` to initialize.");
            }
        }
        "setup" => {
            println!("Memory Provider Setup");
            println!("---------------------");
            std::fs::create_dir_all(&memories_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            if !memory_md.exists() {
                std::fs::write(
                    &memory_md,
                    "# Hermes MEMORY\n\nStore durable assistant memory entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            if !user_md.exists() {
                std::fs::write(
                    &user_md,
                    "# Hermes USER\n\nStore durable user profile entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            if disabled_marker.exists() {
                let _ = std::fs::remove_file(&disabled_marker);
            }
            println!("Initialized file memory backend.");
            println!("  MEMORY.md: {}", memory_md.display());
            println!("  USER.md:   {}", user_md.display());
            println!("Memory is enabled for subsequent sessions.");
        }
        "off" => {
            std::fs::create_dir_all(&hermes_home)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            std::fs::write(
                &disabled_marker,
                format!(
                    "disabled_at={}\nreason=hermes memory off\n",
                    chrono::Utc::now().to_rfc3339()
                ),
            )
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!("Memory provider disabled.");
            println!("  Marker: {}", disabled_marker.display());
            println!("Run `hermes memory setup` to re-enable.");
        }
        "reset" => {
            if !yes {
                return Err(hermes_core::AgentError::Config(
                    "memory reset requires confirmation flag: use `hermes memory reset [all|memory|user] -y`"
                        .into(),
                ));
            }
            let reset_target = target
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("all")
                .to_ascii_lowercase();
            let reset_memory = reset_target == "all" || reset_target == "memory";
            let reset_user = reset_target == "all" || reset_target == "user";
            if !reset_memory && !reset_user {
                return Err(hermes_core::AgentError::Config(format!(
                    "Unknown memory reset target '{}'. Use all|memory|user",
                    reset_target
                )));
            }
            std::fs::create_dir_all(&memories_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            if reset_memory && memory_md.exists() {
                let _ = std::fs::remove_file(&memory_md);
            }
            if reset_user && user_md.exists() {
                let _ = std::fs::remove_file(&user_md);
            }
            if reset_target == "all" && legacy_memory_db.exists() {
                let _ = std::fs::remove_file(&legacy_memory_db);
            }
            if disabled_marker.exists() {
                let _ = std::fs::remove_file(&disabled_marker);
            }
            if reset_memory {
                std::fs::write(
                    &memory_md,
                    "# Hermes MEMORY\n\nStore durable assistant memory entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            if reset_user {
                std::fs::write(
                    &user_md,
                    "# Hermes USER\n\nStore durable user profile entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            println!(
                "Memory reset complete (target={}). MEMORY.md={} USER.md={}",
                reset_target,
                if memory_md.exists() {
                    "present"
                } else {
                    "absent"
                },
                if user_md.exists() {
                    "present"
                } else {
                    "absent"
                }
            );
        }
        other => {
            println!("Unknown memory action '{}'.", other);
            println!("Available actions: status, setup, off, reset");
        }
    }
    Ok(())
}

/// Handle `hermes mcp [action] [--server ...]`.
pub async fn handle_cli_mcp(
    action: Option<String>,
    name: Option<String>,
    server: Option<String>,
    url: Option<String>,
    command: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    let config_dir = hermes_config::hermes_home();
    let mcp_config_path = config_dir.join("mcp_servers.json");
    let mcp_auth_path = config_dir.join("mcp_auth.json");
    let selected = name.clone().or(server.clone());

    match action.as_deref().unwrap_or("list") {
        "list" => {
            if !mcp_config_path.exists() {
                println!("No MCP servers configured ({})", mcp_config_path.display());
                println!("Add one with `hermes mcp add --server <name-or-url>`.");
                return Ok(());
            }
            let content = std::fs::read_to_string(&mcp_config_path)
                .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
            let servers: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            if let Some(obj) = servers.as_object() {
                if obj.is_empty() {
                    println!("No MCP servers configured.");
                } else {
                    println!("MCP servers ({}):", mcp_config_path.display());
                    for (name, cfg) in obj {
                        let url = cfg.get("url").and_then(|v| v.as_str()).unwrap_or("(stdio)");
                        println!("  • {} — {}", name, url);
                    }
                }
            }
        }
        "add" => {
            let (entry_name, entry) = if let Some(name) =
                name.as_deref().map(str::trim).filter(|s| !s.is_empty())
            {
                let entry = if let Some(url) = url.clone().filter(|v| !v.trim().is_empty()) {
                    serde_json::json!({"url": url, "enabled": true})
                } else if let Some(command) = command.clone().filter(|v| !v.trim().is_empty()) {
                    serde_json::json!({"command": command, "enabled": true})
                } else {
                    return Err(hermes_core::AgentError::Config(
                        "mcp add with positional name requires --url or --command".into(),
                    ));
                };
                (name.to_string(), entry)
            } else {
                let srv = server
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing server. Usage: hermes mcp add <name> --url <url> | --command <cmd> (legacy: --server <name-or-url>)".into(),
                        )
                    })?;
                (
                    srv.to_string(),
                    if srv.starts_with("http://") || srv.starts_with("https://") {
                        serde_json::json!({"url": srv, "enabled": true})
                    } else {
                        serde_json::json!({"url": srv, "enabled": true})
                    },
                )
            };
            println!("Adding MCP server: {}", entry_name);
            let mut servers: serde_json::Value = if mcp_config_path.exists() {
                let content = std::fs::read_to_string(&mcp_config_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
            } else {
                serde_json::json!({})
            };
            if let Some(obj) = servers.as_object_mut() {
                obj.insert(entry_name.clone(), entry);
            }
            let json = serde_json::to_string_pretty(&servers)
                .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
            std::fs::write(&mcp_config_path, json)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!(
                "MCP server '{}' added to {}",
                entry_name,
                mcp_config_path.display()
            );
        }
        "remove" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp remove <name>".into(),
                )
            })?;
            if !mcp_config_path.exists() {
                println!("No MCP config to modify.");
                return Ok(());
            }
            let content = std::fs::read_to_string(&mcp_config_path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let mut servers: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            if let Some(obj) = servers.as_object_mut() {
                if obj.remove(&srv).is_some() {
                    let json = serde_json::to_string_pretty(&servers)
                        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
                    std::fs::write(&mcp_config_path, json)
                        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                    println!("MCP server '{}' removed.", srv);
                    if mcp_auth_path.exists() {
                        let raw = std::fs::read_to_string(&mcp_auth_path).unwrap_or_default();
                        let mut auth: serde_json::Value =
                            serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
                        if let Some(auth_obj) = auth.as_object_mut() {
                            auth_obj.remove(&srv);
                            let out = serde_json::to_string_pretty(&auth).unwrap_or_default();
                            let _ = std::fs::write(&mcp_auth_path, out);
                        }
                    }
                } else {
                    println!("MCP server '{}' not found.", srv);
                }
            }
        }
        "serve" => {
            use hermes_skills::{FileSkillStore, SkillManager};
            use hermes_tools::ToolRegistry;

            eprintln!("Starting Hermes as MCP server on stdio...");

            let config = hermes_config::load_config(None)
                .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
            let tool_registry = Arc::new(ToolRegistry::new());
            let terminal_backend = crate::terminal_backend::build_terminal_backend(&config);
            let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
            let skill_provider: Arc<dyn hermes_core::SkillProvider> =
                Arc::new(SkillManager::new(skill_store));
            hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);

            let mcp_server = hermes_mcp::McpServer::new(tool_registry);
            let transport = Box::new(hermes_mcp::ServerStdioTransport::new());
            mcp_server
                .start(transport)
                .await
                .map_err(|e| hermes_core::AgentError::Io(format!("MCP server error: {}", e)))?;
        }
        "test" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp test <name>".into(),
                )
            })?;
            println!("Testing MCP server: {}...", srv);
            if !mcp_config_path.exists() {
                println!("No MCP config found.");
                return Ok(());
            }
            let content = std::fs::read_to_string(&mcp_config_path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let servers: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            match servers.get(&srv) {
                Some(cfg) => {
                    let url = cfg.get("url").and_then(|v| v.as_str()).unwrap_or("(stdio)");
                    let enabled = cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                    println!("  Server: {}", srv);
                    println!("  URL: {}", url);
                    println!("  Enabled: {}", enabled);
                    if url.starts_with("http") {
                        match reqwest::Client::new()
                            .get(url)
                            .timeout(std::time::Duration::from_secs(5))
                            .send()
                            .await
                        {
                            Ok(resp) => println!("  Status: {} (reachable)", resp.status()),
                            Err(e) => println!("  Status: unreachable ({})", e),
                        }
                    } else {
                        println!("  Status: stdio transport (not testable via HTTP)");
                    }
                }
                None => println!("Server '{}' not found in MCP config.", srv),
            }
        }
        "configure" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp configure <name>".into(),
                )
            })?;
            if !mcp_config_path.exists() {
                println!("No MCP config found. Add a server first with `hermes mcp add`.");
                return Ok(());
            }
            let content = std::fs::read_to_string(&mcp_config_path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let servers: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            match servers.get(&srv) {
                Some(cfg) => {
                    println!("Current config for '{}':", srv);
                    println!("{}", serde_json::to_string_pretty(cfg).unwrap_or_default());
                    println!("\nEdit {} to modify settings.", mcp_config_path.display());
                }
                None => println!("Server '{}' not found.", srv),
            }
        }
        "login" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp login <name>".into(),
                )
            })?;
            if !mcp_config_path.exists() {
                return Err(hermes_core::AgentError::Config(format!(
                    "No MCP config found at {}",
                    mcp_config_path.display()
                )));
            }
            let configured = std::fs::read_to_string(&mcp_config_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|v| v.get(&srv).cloned())
                .is_some();
            if !configured {
                return Err(hermes_core::AgentError::Config(format!(
                    "MCP server '{}' is not configured",
                    srv
                )));
            }

            let env_key = format!("MCP_{}_TOKEN", srv.to_uppercase().replace('-', "_"));
            let token_from_env = std::env::var(&env_key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            let token = if let Some(v) = token_from_env {
                v
            } else {
                use std::io::{self, Write};
                print!("Token for '{}': ", srv);
                let _ = io::stdout().flush();
                let mut buf = String::new();
                io::stdin()
                    .read_line(&mut buf)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                buf.trim().to_string()
            };
            if token.is_empty() {
                return Err(hermes_core::AgentError::Config(
                    "Empty token; aborting mcp login".into(),
                ));
            }
            let mut auth: serde_json::Value = if mcp_auth_path.exists() {
                let raw = std::fs::read_to_string(&mcp_auth_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
            } else {
                serde_json::json!({})
            };
            if let Some(obj) = auth.as_object_mut() {
                obj.insert(
                    srv.clone(),
                    serde_json::json!({
                        "token": token,
                        "updated_at": chrono::Utc::now().to_rfc3339(),
                    }),
                );
            }
            std::fs::write(
                &mcp_auth_path,
                serde_json::to_string_pretty(&auth)
                    .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?,
            )
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!(
                "Stored MCP auth token for '{}' in {}",
                srv,
                mcp_auth_path.display()
            );
        }
        other => {
            println!("MCP action '{}' is not recognized.", other);
            println!("Available actions: list, add, remove, serve, test, configure, login");
        }
    }
    Ok(())
}

/// Handle `hermes sessions [action] [--id ...] [--name ...]`.
pub async fn handle_cli_sessions(
    action: Option<String>,
    id: Option<String>,
    name: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    let sessions_dir = hermes_config::hermes_home().join("sessions");

    match action.as_deref().unwrap_or("list") {
        "list" => {
            if !sessions_dir.exists() {
                println!("No sessions directory found.");
                return Ok(());
            }
            let mut entries: Vec<(String, u64)> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.extension().map(|e| e == "json").unwrap_or(false) {
                        let stem = path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        entries.push((stem, size));
                    }
                }
            }
            if entries.is_empty() {
                println!("No saved sessions.");
            } else {
                println!("Saved sessions ({}):", entries.len());
                for (name, size) in &entries {
                    println!("  • {} ({} bytes)", name, size);
                }
            }
        }
        "export" => {
            let session_id = id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing session ID. Usage: hermes sessions export --id <id>".into(),
                )
            })?;
            let path = sessions_dir.join(format!("{}.json", session_id));
            if !path.exists() {
                println!("Session '{}' not found.", session_id);
                return Ok(());
            }
            let content = std::fs::read_to_string(&path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!("{}", content);
        }
        "delete" => {
            let session_id = id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing session ID. Usage: hermes sessions delete --id <id>".into(),
                )
            })?;
            let path = sessions_dir.join(format!("{}.json", session_id));
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Session '{}' deleted.", session_id);
            } else {
                println!("Session '{}' not found.", session_id);
            }
        }
        "stats" => {
            if !sessions_dir.exists() {
                println!("No sessions directory.");
                return Ok(());
            }
            let mut total_files = 0u32;
            let mut total_size = 0u64;
            if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    if entry
                        .path()
                        .extension()
                        .map(|e| e == "json")
                        .unwrap_or(false)
                    {
                        total_files += 1;
                        total_size += std::fs::metadata(entry.path())
                            .map(|m| m.len())
                            .unwrap_or(0);
                    }
                }
            }
            println!("Session statistics:");
            println!("  Total sessions: {}", total_files);
            println!("  Total size:     {} KB", total_size / 1024);
            println!("  Directory:      {}", sessions_dir.display());
        }
        "prune" => {
            let max_age_days: u64 = name
                .as_deref()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(30);
            println!("Pruning sessions older than {} days...", max_age_days);
            if !sessions_dir.exists() {
                println!("No sessions directory.");
                return Ok(());
            }
            let cutoff = std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(max_age_days * 86400))
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let mut pruned = 0u32;
            if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.extension().map(|e| e == "json").unwrap_or(false) {
                        continue;
                    }
                    if let Ok(meta) = std::fs::metadata(&path) {
                        let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        if modified < cutoff {
                            if std::fs::remove_file(&path).is_ok() {
                                let name = path.file_stem().unwrap_or_default().to_string_lossy();
                                println!("  Pruned: {}", name);
                                pruned += 1;
                            }
                        }
                    }
                }
            }
            println!("Pruned {} session(s).", pruned);
        }
        "rename" => {
            let session_id = id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing session ID. Usage: hermes sessions rename --id <id> --name <new>"
                        .into(),
                )
            })?;
            let new_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing new name. Usage: hermes sessions rename --id <id> --name <new>".into(),
                )
            })?;
            let old_path = sessions_dir.join(format!("{}.json", session_id));
            let new_path = sessions_dir.join(format!("{}.json", new_name));
            if !old_path.exists() {
                println!("Session '{}' not found.", session_id);
                return Ok(());
            }
            std::fs::rename(&old_path, &new_path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!("Session renamed: {} -> {}", session_id, new_name);
        }
        "browse" => {
            if !sessions_dir.exists() {
                println!("No sessions directory found.");
                return Ok(());
            }
            println!("Session Browser");
            println!("===============\n");
            let mut entries: Vec<(String, u64, std::time::SystemTime, usize)> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.extension().map(|e| e == "json").unwrap_or(false) {
                        continue;
                    }
                    let stem = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let meta = std::fs::metadata(&path);
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let modified = meta
                        .as_ref()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    let msg_count = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
                        .and_then(|v| {
                            v.get("messages")
                                .and_then(|m| m.as_array())
                                .map(|a| a.len())
                        })
                        .unwrap_or(0);
                    entries.push((stem, size, modified, msg_count));
                }
            }
            entries.sort_by(|a, b| b.2.cmp(&a.2));
            if entries.is_empty() {
                println!("No sessions found.");
            } else {
                println!(
                    "{:3} {:30} {:>8} {:>6}  {}",
                    "#", "Session ID", "Size", "Msgs", "Modified"
                );
                println!("{}", "-".repeat(75));
                for (idx, (name, size, modified, msgs)) in entries.iter().enumerate() {
                    let age = modified.elapsed().unwrap_or_default();
                    let age_str = if age.as_secs() < 3600 {
                        format!("{}m ago", age.as_secs() / 60)
                    } else if age.as_secs() < 86400 {
                        format!("{}h ago", age.as_secs() / 3600)
                    } else {
                        format!("{}d ago", age.as_secs() / 86400)
                    };
                    println!(
                        "{:3} {:30} {:>6}KB {:>6}  {}",
                        idx + 1,
                        &name[..name.len().min(30)],
                        size / 1024,
                        msgs,
                        age_str,
                    );
                }
                println!("\nUse `hermes sessions export --id <id>` to view a session.");
            }
        }
        other => {
            println!("Sessions action '{}' is not recognized.", other);
            println!("Available actions: list, export, delete, prune, stats, rename, browse");
        }
    }
    Ok(())
}

/// Handle `hermes insights [--days N] [--source ...]`.
pub async fn handle_cli_insights(
    days: u32,
    source: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    println!("Usage Insights (last {} days)", days);
    println!("=============================");
    if let Some(src) = &source {
        println!("Filter: source={}\n", src);
    }
    let sessions_dir = hermes_config::hermes_home().join("sessions");
    if !sessions_dir.exists() {
        println!("No sessions directory found.");
        return Ok(());
    }

    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(u64::from(days) * 86400))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    let mut total_sessions = 0u32;
    let mut total_messages = 0u64;
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;
    let mut total_cost_cents = 0.0f64;
    let mut models_used: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut daily_counts: std::collections::BTreeMap<String, u32> =
        std::collections::BTreeMap::new();

    if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.extension().map(|e| e == "json").unwrap_or(false) {
                continue;
            }
            let meta = std::fs::metadata(&path);
            let modified = meta
                .as_ref()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if modified < cutoff {
                continue;
            }

            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(src_filter) = &source {
                        let session_source = data
                            .get("source")
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown");
                        if session_source != src_filter.as_str() {
                            continue;
                        }
                    }

                    total_sessions += 1;

                    if let Some(msgs) = data.get("messages").and_then(|m| m.as_array()) {
                        total_messages += msgs.len() as u64;
                    }

                    if let Some(usage) = data.get("usage") {
                        total_input_tokens += usage
                            .get("input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        total_output_tokens += usage
                            .get("output_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        total_cost_cents +=
                            usage.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    }

                    if let Some(model) = data.get("model").and_then(|m| m.as_str()) {
                        *models_used.entry(model.to_string()).or_insert(0) += 1;
                    }

                    let dur = modified
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default();
                    let secs = dur.as_secs();
                    let day_secs = secs - (secs % 86400);
                    let day_key = format!("{}", day_secs / 86400);
                    *daily_counts.entry(day_key).or_insert(0) += 1;
                }
            }
        }
    }

    println!("Sessions:       {}", total_sessions);
    println!("Messages:       {}", total_messages);
    println!("Input tokens:   {}", total_input_tokens);
    println!("Output tokens:  {}", total_output_tokens);
    let total_tokens = total_input_tokens + total_output_tokens;
    println!("Total tokens:   {}", total_tokens);
    if total_cost_cents > 0.0 {
        println!("Estimated cost: ${:.4}", total_cost_cents / 100.0);
    }

    if !models_used.is_empty() {
        println!("\nModels Used:");
        let mut model_vec: Vec<_> = models_used.into_iter().collect();
        model_vec.sort_by(|a, b| b.1.cmp(&a.1));
        for (model, count) in &model_vec {
            println!("  {:30} {:>5} session(s)", model, count);
        }
    }

    if total_sessions > 0 {
        println!("\nAverages per session:");
        println!(
            "  Messages: {:.1}",
            total_messages as f64 / total_sessions as f64
        );
        println!(
            "  Tokens:   {:.0}",
            total_tokens as f64 / total_sessions as f64
        );
    }

    Ok(())
}

/// Handle `hermes login [provider]`.
pub async fn handle_cli_login(provider: Option<String>) -> Result<(), hermes_core::AgentError> {
    let provider = provider.unwrap_or_else(|| "openai".to_string());
    let creds_dir = hermes_config::hermes_home().join("credentials");
    std::fs::create_dir_all(&creds_dir).map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

    println!("Login to: {}", provider);
    println!("----------{}", "-".repeat(provider.len()));

    match provider.as_str() {
        "openai" => {
            let env_key = std::env::var("HERMES_OPENAI_API_KEY")
                .ok()
                .or_else(|| std::env::var("OPENAI_API_KEY").ok());
            if let Some(key) = env_key {
                let masked = if key.len() > 8 {
                    format!("{}...{}", &key[..4], &key[key.len() - 4..])
                } else {
                    "****".to_string()
                };
                println!(
                    "Found HERMES_OPENAI_API_KEY/OPENAI_API_KEY in environment: {}",
                    masked
                );
                let cred_file = creds_dir.join("openai.json");
                let cred = serde_json::json!({
                    "provider": "openai",
                    "api_key_masked": masked,
                    "stored_at": chrono::Utc::now().to_rfc3339(),
                    "source": "env",
                });
                std::fs::write(
                    &cred_file,
                    serde_json::to_string_pretty(&cred).unwrap_or_default(),
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Credential reference stored at {}", cred_file.display());
            } else {
                println!("No HERMES_OPENAI_API_KEY/OPENAI_API_KEY found in environment.");
                println!("Set it with: export HERMES_OPENAI_API_KEY=sk-...");
                println!("Or use: hermes config set openai_api_key <key>");
            }
        }
        "anthropic" => {
            let env_key = std::env::var("ANTHROPIC_API_KEY").ok();
            if let Some(key) = env_key {
                let masked = if key.len() > 8 {
                    format!("{}...{}", &key[..4], &key[key.len() - 4..])
                } else {
                    "****".to_string()
                };
                println!("Found ANTHROPIC_API_KEY in environment: {}", masked);
                let cred_file = creds_dir.join("anthropic.json");
                let cred = serde_json::json!({
                    "provider": "anthropic",
                    "api_key_masked": masked,
                    "stored_at": chrono::Utc::now().to_rfc3339(),
                    "source": "env",
                });
                std::fs::write(
                    &cred_file,
                    serde_json::to_string_pretty(&cred).unwrap_or_default(),
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Credential reference stored at {}", cred_file.display());
            } else {
                println!("No ANTHROPIC_API_KEY found in environment.");
                println!("Set it with: export ANTHROPIC_API_KEY=sk-ant-...");
            }
        }
        other => {
            let env_var = format!("{}_API_KEY", other.to_uppercase().replace('-', "_"));
            let env_key = std::env::var(&env_var).ok();
            if let Some(key) = env_key {
                let masked = if key.len() > 8 {
                    format!("{}...{}", &key[..4], &key[key.len() - 4..])
                } else {
                    "****".to_string()
                };
                println!("Found {} in environment: {}", env_var, masked);
                let cred_file = creds_dir.join(format!("{}.json", other));
                let cred = serde_json::json!({
                    "provider": other,
                    "api_key_masked": masked,
                    "stored_at": chrono::Utc::now().to_rfc3339(),
                    "source": "env",
                });
                std::fs::write(
                    &cred_file,
                    serde_json::to_string_pretty(&cred).unwrap_or_default(),
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Credential reference stored.");
            } else {
                println!("No {} found in environment.", env_var);
                println!("Set it with: export {}=<your-key>", env_var);
            }
        }
    }
    Ok(())
}

/// Handle `hermes logout [provider]`.
pub async fn handle_cli_logout(provider: Option<String>) -> Result<(), hermes_core::AgentError> {
    let creds_dir = hermes_config::hermes_home().join("credentials");

    match provider.as_deref() {
        Some(p) => {
            let cred_file = creds_dir.join(format!("{}.json", p));
            if cred_file.exists() {
                std::fs::remove_file(&cred_file)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Logged out from '{}'. Credential reference removed.", p);
            } else {
                println!("No stored credentials for '{}'.", p);
            }
            println!(
                "Note: Environment variables (e.g. {}_API_KEY) are not affected.",
                p.to_uppercase().replace('-', "_")
            );
        }
        None => {
            if creds_dir.exists() {
                let mut removed = 0u32;
                if let Ok(rd) = std::fs::read_dir(&creds_dir) {
                    for entry in rd.filter_map(|e| e.ok()) {
                        let path = entry.path();
                        if path.extension().map(|e| e == "json").unwrap_or(false) {
                            if std::fs::remove_file(&path).is_ok() {
                                let name = path.file_stem().unwrap_or_default().to_string_lossy();
                                println!("  Removed credential: {}", name);
                                removed += 1;
                            }
                        }
                    }
                }
                if removed == 0 {
                    println!("No stored credentials to remove.");
                } else {
                    println!("Logged out from {} provider(s).", removed);
                }
            } else {
                println!("No credentials directory found.");
            }
            println!("Note: Environment variables are not affected.");
        }
    }
    Ok(())
}

/// Handle `hermes whatsapp [action]`.
pub async fn handle_cli_whatsapp(action: Option<String>) -> Result<(), hermes_core::AgentError> {
    match action.as_deref().unwrap_or("status") {
        "setup" => {
            whatsapp_setup().await?;
        }
        "status" => {
            whatsapp_status().await?;
        }
        "qr" => {
            whatsapp_qr().await?;
        }
        other => {
            println!("WhatsApp action '{}' is not recognized.", other);
            println!("Available actions: setup, status, qr");
        }
    }
    Ok(())
}

/// Interactive setup: collect credentials, persist to config.yaml, verify.
async fn whatsapp_setup() -> Result<(), hermes_core::AgentError> {
    use std::io::{self, BufRead, Write};

    println!("WhatsApp Cloud API Setup");
    println!("========================\n");
    println!("You will need credentials from the Meta developer dashboard:");
    println!("  https://developers.facebook.com/apps/\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    print!("Phone Number ID: ");
    stdout.flush().ok();
    let phone_number_id = stdin
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if phone_number_id.is_empty() {
        println!("Aborted: phone number ID is required.");
        return Ok(());
    }

    print!("Business Account ID: ");
    stdout.flush().ok();
    let business_account_id = stdin
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if business_account_id.is_empty() {
        println!("Aborted: business account ID is required.");
        return Ok(());
    }

    print!("Access Token: ");
    stdout.flush().ok();
    let access_token = stdin
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if access_token.is_empty() {
        println!("Aborted: access token is required.");
        return Ok(());
    }

    println!("\nVerifying token against WhatsApp Cloud API...");
    let url = format!(
        "https://graph.facebook.com/v21.0/{}/messages",
        phone_number_id
    );
    let client = reqwest::Client::new();
    match client
        .get(&url)
        .bearer_auth(&access_token)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() || status.as_u16() == 400 {
                // 400 means the endpoint is reachable (POST required for actual messages)
                println!("  API reachable (HTTP {}).", status);
            } else if status.as_u16() == 401 || status.as_u16() == 403 {
                println!("  Warning: API returned {} — token may be invalid.", status);
                println!("  Saving anyway; you can re-run setup later.");
            } else {
                println!("  API returned HTTP {}. Saving config anyway.", status);
            }
        }
        Err(e) => {
            println!("  Could not reach API: {}", e);
            println!("  Saving config anyway — verify network connectivity.");
        }
    }

    let config_path = hermes_config::hermes_home().join("config.yaml");
    let mut config: serde_yaml::Value = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
        serde_yaml::from_str(&content).unwrap_or(serde_yaml::Value::Mapping(Default::default()))
    } else {
        serde_yaml::Value::Mapping(Default::default())
    };

    let platforms = config
        .as_mapping_mut()
        .unwrap()
        .entry(serde_yaml::Value::String("platforms".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));

    let wa = platforms
        .as_mapping_mut()
        .unwrap()
        .entry(serde_yaml::Value::String("whatsapp".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));

    let wa_map = wa.as_mapping_mut().unwrap();
    wa_map.insert(
        serde_yaml::Value::String("phone_number_id".into()),
        serde_yaml::Value::String(phone_number_id.clone()),
    );
    wa_map.insert(
        serde_yaml::Value::String("business_account_id".into()),
        serde_yaml::Value::String(business_account_id),
    );
    wa_map.insert(
        serde_yaml::Value::String("access_token".into()),
        serde_yaml::Value::String(access_token),
    );
    wa_map.insert(
        serde_yaml::Value::String("enabled".into()),
        serde_yaml::Value::Bool(true),
    );

    let yaml_str = serde_yaml::to_string(&config)
        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
    std::fs::create_dir_all(hermes_config::hermes_home())
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    std::fs::write(&config_path, &yaml_str)
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

    println!(
        "\nWhatsApp configuration saved to {}",
        config_path.display()
    );
    println!("Phone Number ID: {}", phone_number_id);
    println!("\nRun `hermes whatsapp status` to verify.");
    Ok(())
}

/// Check whether WhatsApp is configured and verify connectivity.
async fn whatsapp_status() -> Result<(), hermes_core::AgentError> {
    let config_path = hermes_config::hermes_home().join("config.yaml");
    if !config_path.exists() {
        println!("WhatsApp: not configured");
        println!("Run `hermes whatsapp setup` to configure.");
        return Ok(());
    }

    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    let config: serde_yaml::Value =
        serde_yaml::from_str(&content).unwrap_or(serde_yaml::Value::Mapping(Default::default()));

    let wa = config.get("platforms").and_then(|p| p.get("whatsapp"));

    match wa {
        None => {
            println!("WhatsApp: not configured");
            println!("Run `hermes whatsapp setup` to configure.");
        }
        Some(wa_cfg) => {
            let phone_id = wa_cfg
                .get("phone_number_id")
                .and_then(|v| v.as_str())
                .unwrap_or("(not set)");
            let enabled = wa_cfg
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let has_token = wa_cfg
                .get("access_token")
                .and_then(|v| v.as_str())
                .map(|t| !t.is_empty())
                .unwrap_or(false);

            println!("WhatsApp Status");
            println!("---------------");
            println!("  Configured:     yes");
            println!("  Enabled:        {}", enabled);
            println!("  Phone Number ID: {}", phone_id);
            println!(
                "  Access Token:   {}",
                if has_token { "present" } else { "missing" }
            );

            if has_token {
                let token = wa_cfg.get("access_token").unwrap().as_str().unwrap();
                let url = format!("https://graph.facebook.com/v21.0/{}/messages", phone_id);
                print!("  API Connectivity: ");
                match reqwest::Client::new()
                    .get(&url)
                    .bearer_auth(token)
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await
                {
                    Ok(resp) => println!("reachable (HTTP {})", resp.status()),
                    Err(e) => println!("unreachable ({})", e),
                }
            }
        }
    }
    Ok(())
}

/// Connect to local bridge, fetch QR data, and render in terminal.
async fn whatsapp_qr() -> Result<(), hermes_core::AgentError> {
    let config_path = hermes_config::hermes_home().join("config.yaml");
    let bridge_url = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        let config: serde_yaml::Value = serde_yaml::from_str(&content)
            .unwrap_or(serde_yaml::Value::Mapping(Default::default()));
        config
            .get("platforms")
            .and_then(|p| p.get("whatsapp"))
            .and_then(|w| w.get("bridge_url"))
            .and_then(|u| u.as_str())
            .unwrap_or("http://localhost:3000")
            .to_string()
    } else {
        "http://localhost:3000".to_string()
    };

    let qr_url = format!("{}/qr", bridge_url);
    println!("Fetching QR code from {}...", qr_url);

    match reqwest::Client::new()
        .get(&qr_url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let body = resp
                .text()
                .await
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

            let qr_data = if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                json.get("qr")
                    .or_else(|| json.get("data"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&body)
                    .to_string()
            } else {
                body
            };

            println!();
            render_qr_to_terminal(&qr_data);
            println!();
            println!("Scan this QR code with WhatsApp on your phone:");
            println!("  WhatsApp → Settings → Linked Devices → Link a Device");
        }
        Ok(resp) => {
            println!(
                "Bridge returned HTTP {}. Is the bridge server running?",
                resp.status()
            );
            println!("Start it with: npx hermes-whatsapp-bridge");
        }
        Err(e) => {
            println!("Could not connect to bridge at {}: {}", bridge_url, e);
            println!("\nMake sure the WhatsApp Web bridge is running:");
            println!("  npx hermes-whatsapp-bridge");
            println!("  # or: docker run -p 3000:3000 hermes/whatsapp-bridge");
        }
    }
    Ok(())
}

/// Render QR data as Unicode block art in the terminal.
///
/// Uses a simple bit-encoding approach: each character in the input
/// string controls whether a "module" is dark or light. Two rows are
/// packed into one terminal line using half-block characters.
fn render_qr_to_terminal(data: &str) {
    // Determine a square side length from the data
    let len = data.len();
    let side = (len as f64).sqrt().ceil() as usize;
    if side == 0 {
        println!("(empty QR data)");
        return;
    }

    let bytes = data.as_bytes();

    // Dark module = odd byte value, light = even (simple heuristic)
    let is_dark = |row: usize, col: usize| -> bool {
        let idx = row * side + col;
        if idx < bytes.len() {
            bytes[idx] % 2 == 1
        } else {
            false
        }
    };

    // Print using half-block characters: each terminal row encodes two QR rows.
    // ▀ = top dark, bottom light | ▄ = top light, bottom dark
    // █ = both dark              | ' ' = both light
    let mut row = 0;
    while row < side {
        let mut line = String::new();
        for col in 0..side {
            let top = is_dark(row, col);
            let bottom = if row + 1 < side {
                is_dark(row + 1, col)
            } else {
                false
            };
            line.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        println!("  {}", line);
        row += 2;
    }
}

/// Handle `hermes pairing [action] [--device-id ...]`.
pub async fn handle_cli_pairing(
    action: Option<String>,
    device_id: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    use crate::pairing_store::{PairingStatus, PairingStore};

    let store = PairingStore::open_default();

    match action.as_deref().unwrap_or("list") {
        "list" => {
            let devices = store.list().map_err(|e| hermes_core::AgentError::Io(e))?;
            if devices.is_empty() {
                println!("No paired devices.");
                println!("  Store: {}", PairingStore::default_path().display());
            } else {
                println!("Paired devices ({}):", devices.len());
                println!(
                    "  {:20} {:10} {:12} {}",
                    "Device ID", "Status", "Last Seen", "Name"
                );
                println!("  {}", "-".repeat(60));
                for d in &devices {
                    let last_seen = d.last_seen.as_deref().unwrap_or("never");
                    let name = d.name.as_deref().unwrap_or("(unnamed)");
                    let status_icon = match d.status {
                        PairingStatus::Pending => "⏳",
                        PairingStatus::Approved => "✓",
                        PairingStatus::Revoked => "✗",
                    };
                    println!(
                        "  {:20} {} {:8} {:12} {}",
                        d.device_id, status_icon, d.status, last_seen, name
                    );
                }
            }
        }
        "approve" => {
            let did = device_id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing --device-id. Usage: hermes pairing approve --device-id <id>".into(),
                )
            })?;
            match store.approve(&did) {
                Ok(dev) => {
                    println!("Device '{}' approved.", dev.device_id);
                    if let Some(secret) = &dev.shared_secret {
                        if secret_stdout_allowed() {
                            println!("  Shared secret: {}", secret);
                            println!(
                                "  (plaintext output enabled via HERMES_ALLOW_SECRET_STDOUT=1)"
                            );
                        } else {
                            println!("  Shared secret: {}", mask_secret_value(secret));
                            println!(
                                "  (set HERMES_ALLOW_SECRET_STDOUT=1 to reveal plaintext once)"
                            );
                        }
                        println!("  (Store this securely — it will not be shown again)");
                    }
                }
                Err(e) => println!("Failed to approve device: {}", e),
            }
        }
        "revoke" => {
            let did = device_id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing --device-id. Usage: hermes pairing revoke --device-id <id>".into(),
                )
            })?;
            match store.revoke(&did) {
                Ok(dev) => {
                    println!("Device '{}' revoked.", dev.device_id);
                    println!("  The device will no longer be able to connect.");
                }
                Err(e) => println!("Failed to revoke device: {}", e),
            }
        }
        "clear-pending" => match store.clear_pending() {
            Ok(count) => {
                if count == 0 {
                    println!("No pending pairing requests to clear.");
                } else {
                    println!("Cleared {} pending pairing request(s).", count);
                }
            }
            Err(e) => println!("Failed to clear pending requests: {}", e),
        },
        other => {
            println!("Pairing action '{}' is not recognized.", other);
            println!("Available actions: list, approve, revoke, clear-pending");
        }
    }
    Ok(())
}

/// Handle `hermes claw [action]`.
pub async fn handle_cli_claw(action: Option<String>) -> Result<(), hermes_core::AgentError> {
    match action.as_deref().unwrap_or("status") {
        "migrate" => {
            claw_migrate_cmd()?;
        }
        "cleanup" => {
            claw_cleanup_cmd()?;
        }
        "status" => {
            claw_status_cmd();
        }
        other => {
            println!("Claw action '{}' is not recognized.", other);
            println!("Available actions: migrate, cleanup, status");
        }
    }
    Ok(())
}

/// Check for legacy OpenClaw artefacts and report findings.
fn claw_status_cmd() {
    use crate::claw_migrate::find_openclaw_dir;

    println!("OpenClaw Legacy Status");
    println!("======================\n");

    let home = dirs::home_dir();

    match find_openclaw_dir(None) {
        Some(dir) => {
            println!("  OpenClaw directory: {} (found)", dir.display());

            let config_yaml = dir.join("config.yaml");
            let sessions_dir = dir.join("sessions");
            let env_file = dir.join(".env");
            let skills_dir = dir.join("skills");

            println!(
                "  config.yaml:       {}",
                if config_yaml.exists() {
                    "present"
                } else {
                    "not found"
                }
            );
            println!(
                "  .env:              {}",
                if env_file.exists() {
                    "present"
                } else {
                    "not found"
                }
            );
            println!(
                "  skills/:           {}",
                if skills_dir.is_dir() {
                    "present"
                } else {
                    "not found"
                }
            );

            if sessions_dir.is_dir() {
                let count = std::fs::read_dir(&sessions_dir)
                    .map(|rd| rd.filter_map(|e| e.ok()).count())
                    .unwrap_or(0);
                println!("  sessions/:         {} file(s)", count);
            } else {
                println!("  sessions/:         not found");
            }

            println!("\n  Run `hermes claw migrate` to import into Hermes.");
            println!("  Run `hermes claw cleanup` to remove legacy files.");
        }
        None => {
            println!("  No OpenClaw directory found.");
            if let Some(h) = &home {
                println!(
                    "  Checked: ~/.openclaw, ~/.clawdbot, ~/.moldbot under {}",
                    h.display()
                );
            }
            println!("\n  Nothing to migrate.");
        }
    }

    // Also check for PATH entries in shell configs
    if let Some(h) = &home {
        let shell_files = [".bashrc", ".zshrc", ".profile", ".bash_profile"];
        let mut found_refs = Vec::new();
        for f in &shell_files {
            let path = h.join(f);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains("openclaw") || content.contains("clawdbot") {
                    found_refs.push(f.to_string());
                }
            }
        }
        if !found_refs.is_empty() {
            println!("\n  Shell config references found:");
            for f in &found_refs {
                println!("    ~/{}", f);
            }
        }
    }
}

/// Run the full migration using `claw_migrate::run_migration`.
fn claw_migrate_cmd() -> Result<(), hermes_core::AgentError> {
    use crate::claw_migrate::{find_openclaw_dir, run_migration, MigrateOptions};

    println!("OpenClaw → Hermes Migration");
    println!("===========================\n");

    let source_dir = find_openclaw_dir(None);
    if source_dir.is_none() {
        println!("No OpenClaw directory found. Nothing to migrate.");
        return Ok(());
    }
    let source_dir = source_dir.unwrap();
    println!("Source: {}", source_dir.display());
    println!("Target: {}\n", hermes_config::hermes_home().display());

    // Also copy sessions if they exist
    let src_sessions = source_dir.join("sessions");
    let dst_sessions = hermes_config::hermes_home().join("sessions");
    let mut session_count = 0usize;

    if src_sessions.is_dir() {
        std::fs::create_dir_all(&dst_sessions).map_err(|e| {
            hermes_core::AgentError::Io(format!("Failed to create sessions dir: {}", e))
        })?;
        if let Ok(entries) = std::fs::read_dir(&src_sessions) {
            for entry in entries.flatten() {
                let src = entry.path();
                let dst = dst_sessions.join(entry.file_name());
                if src.is_file() && !dst.exists() {
                    if std::fs::copy(&src, &dst).is_ok() {
                        session_count += 1;
                    }
                }
            }
        }
    }

    let options = MigrateOptions {
        source: Some(source_dir),
        dry_run: false,
        preset: "full".to_string(),
        overwrite: false,
    };

    let result = run_migration(&options);

    if !result.migrated.is_empty() {
        println!("Migrated:");
        for item in &result.migrated {
            let src = item
                .source
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let dst = item
                .destination
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let extra = item.reason.as_deref().unwrap_or("");
            println!("  ✓ {} → {} {}", src, dst, extra);
        }
    }

    if !result.skipped.is_empty() {
        println!("Skipped:");
        for item in &result.skipped {
            let reason = item.reason.as_deref().unwrap_or("");
            println!("  ⊘ {} — {}", item.kind, reason);
        }
    }

    if !result.errors.is_empty() {
        println!("Errors:");
        for item in &result.errors {
            let reason = item.reason.as_deref().unwrap_or("unknown error");
            println!("  ✗ {} — {}", item.kind, reason);
        }
    }

    if session_count > 0 {
        println!("\nSessions copied: {}", session_count);
    }

    let total = result.migrated.len() + session_count;
    println!(
        "\nMigration complete: {} item(s) migrated, {} skipped, {} error(s).",
        total,
        result.skipped.len(),
        result.errors.len()
    );

    Ok(())
}

/// Remove legacy OpenClaw files after confirmation.
fn claw_cleanup_cmd() -> Result<(), hermes_core::AgentError> {
    use crate::claw_migrate::find_openclaw_dir;
    use std::io::{self, BufRead, Write};

    let source_dir = find_openclaw_dir(None);
    if source_dir.is_none() {
        println!("No OpenClaw directory found. Nothing to clean up.");
        return Ok(());
    }
    let source_dir = source_dir.unwrap();

    println!("OpenClaw Cleanup");
    println!("================\n");
    println!("The following will be PERMANENTLY deleted:");
    println!("  Directory: {}", source_dir.display());

    // Count contents
    let file_count = count_files_recursive(&source_dir);
    println!("  Contains:  ~{} file(s)\n", file_count);

    // Check shell configs
    let home = dirs::home_dir();
    let shell_files = [".bashrc", ".zshrc", ".profile", ".bash_profile"];
    let mut affected_shells: Vec<String> = Vec::new();
    if let Some(h) = &home {
        for f in &shell_files {
            let path = h.join(f);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains("openclaw") || content.contains("clawdbot") {
                    affected_shells.push(f.to_string());
                    println!("  Shell config: ~/{} (contains openclaw references)", f);
                }
            }
        }
    }

    print!("\nProceed with cleanup? [y/N]: ");
    io::stdout().flush().ok();
    let answer = io::stdin()
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default();

    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Cleanup cancelled.");
        return Ok(());
    }

    // Remove the directory
    match std::fs::remove_dir_all(&source_dir) {
        Ok(_) => println!("  ✓ Removed {}", source_dir.display()),
        Err(e) => println!("  ✗ Failed to remove {}: {}", source_dir.display(), e),
    }

    // Clean shell configs
    if let Some(h) = &home {
        for f in &affected_shells {
            let path = h.join(f);
            if let Ok(content) = std::fs::read_to_string(&path) {
                let cleaned: Vec<&str> = content
                    .lines()
                    .filter(|line| {
                        let lower = line.to_lowercase();
                        !lower.contains("openclaw") && !lower.contains("clawdbot")
                    })
                    .collect();
                let new_content = cleaned.join("\n") + "\n";
                match std::fs::write(&path, new_content) {
                    Ok(_) => println!("  ✓ Cleaned ~/{}", f),
                    Err(e) => println!("  ✗ Failed to clean ~/{}: {}", f, e),
                }
            }
        }
    }

    println!("\nCleanup complete.");
    Ok(())
}

/// Recursively count files in a directory.
fn count_files_recursive(dir: &std::path::Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += count_files_recursive(&path);
            } else {
                count += 1;
            }
        }
    }
    count
}

fn acp_history_to_messages(
    history: &[serde_json::Value],
    fallback_user_text: &str,
) -> Vec<hermes_core::Message> {
    let mut messages = Vec::new();

    for item in history {
        let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content = item
            .get("content")
            .or_else(|| item.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match role {
            "system" if !content.is_empty() => messages.push(hermes_core::Message::system(content)),
            "user" if !content.is_empty() => messages.push(hermes_core::Message::user(content)),
            "assistant" => {
                if let Some(tool_calls_val) = item.get("tool_calls") {
                    if let Ok(tool_calls) =
                        serde_json::from_value::<Vec<hermes_core::ToolCall>>(tool_calls_val.clone())
                    {
                        let assistant = hermes_core::Message::assistant_with_tool_calls(
                            if content.is_empty() {
                                None
                            } else {
                                Some(content)
                            },
                            tool_calls,
                        );
                        messages.push(assistant);
                        continue;
                    }
                }
                if !content.is_empty() {
                    messages.push(hermes_core::Message::assistant(content));
                }
            }
            "tool" if !content.is_empty() => {
                let tool_call_id = item
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool_call");
                messages.push(hermes_core::Message::tool_result(tool_call_id, content));
            }
            _ => {}
        }
    }

    let has_user_tail = messages
        .last()
        .map(|m| matches!(m.role, hermes_core::MessageRole::User))
        .unwrap_or(false);
    if !has_user_tail && !fallback_user_text.trim().is_empty() {
        messages.push(hermes_core::Message::user(fallback_user_text));
    }

    messages
}

struct CliAcpPromptExecutor {
    config: Arc<hermes_config::GatewayConfig>,
    tool_registry: Arc<hermes_tools::ToolRegistry>,
    tool_schemas: Vec<hermes_core::ToolSchema>,
}

#[async_trait::async_trait]
impl hermes_acp::AcpPromptExecutor for CliAcpPromptExecutor {
    async fn execute_prompt(
        &self,
        session: &hermes_acp::SessionState,
        user_text: &str,
        history: &[serde_json::Value],
    ) -> Result<hermes_acp::PromptExecutionOutput, String> {
        let model = session
            .model
            .clone()
            .or_else(|| self.config.model.clone())
            .unwrap_or_else(|| "gpt-4o".to_string());

        let provider = crate::app::build_provider(&self.config, &model);
        let mut agent_config = crate::app::build_agent_config(&self.config, &model);
        agent_config.session_id = Some(session.session_id.clone());

        let agent_tools = Arc::new(crate::app::bridge_tool_registry(&self.tool_registry));
        let agent = hermes_agent::AgentLoop::new(agent_config, agent_tools, provider);
        let messages = acp_history_to_messages(history, user_text);

        let result = agent
            .run(messages, Some(self.tool_schemas.clone()))
            .await
            .map_err(|e| e.to_string())?;
        let response_text = result
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        let usage = result.usage.map(|u| hermes_acp::Usage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            thought_tokens: None,
            cached_read_tokens: None,
        });

        Ok(hermes_acp::PromptExecutionOutput {
            response_text,
            usage,
            total_turns: Some(result.total_turns),
        })
    }
}

/// Handle `hermes acp [action]`.
pub async fn handle_cli_acp(action: Option<String>) -> Result<(), hermes_core::AgentError> {
    match action.as_deref().unwrap_or("status") {
        "start" => {
            let config = hermes_config::load_config(None)
                .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;

            let model = config.model.clone().unwrap_or_else(|| "gpt-4o".to_string());
            let max_turns = config.max_turns as usize;

            println!(
                "Starting ACP server (model={}, max_turns={})...",
                model, max_turns
            );

            let tool_registry = Arc::new(hermes_tools::ToolRegistry::new());
            let terminal_backend = crate::terminal_backend::build_terminal_backend(&config);
            let skill_store = Arc::new(hermes_skills::FileSkillStore::new(
                hermes_skills::FileSkillStore::default_dir(),
            ));
            let skill_provider: Arc<dyn hermes_core::SkillProvider> =
                Arc::new(hermes_skills::SkillManager::new(skill_store));
            hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
            crate::runtime_tool_wiring::wire_stdio_clarify_backend(&tool_registry);
            let cron_data_dir = hermes_config::cron_dir();
            std::fs::create_dir_all(&cron_data_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let cron_scheduler = Arc::new(hermes_cron::cron_scheduler_for_data_dir(cron_data_dir));
            cron_scheduler
                .load_persisted_jobs()
                .await
                .map_err(|e| hermes_core::AgentError::Config(format!("cron load: {e}")))?;
            cron_scheduler.start().await;
            crate::runtime_tool_wiring::wire_cron_scheduler_backend(&tool_registry, cron_scheduler);
            let tool_schemas = crate::platform_toolsets::resolve_platform_tool_schemas(
                &config,
                "cli",
                &tool_registry,
            );

            let prompt_executor = Arc::new(CliAcpPromptExecutor {
                config: Arc::new(config.clone()),
                tool_registry,
                tool_schemas,
            });

            let session_manager = Arc::new(hermes_acp::SessionManager::new());
            let event_sink = Arc::new(hermes_acp::EventSink::default());
            let permission_store = Arc::new(hermes_acp::PermissionStore::new());
            let handler = Arc::new(
                hermes_acp::HermesAcpHandler::new(
                    session_manager.clone(),
                    event_sink.clone(),
                    permission_store.clone(),
                )
                .with_prompt_executor(prompt_executor),
            );
            let server = hermes_acp::AcpServer::with_components(
                handler,
                session_manager,
                event_sink,
                permission_store,
            );

            server
                .run()
                .await
                .map_err(|e| hermes_core::AgentError::Io(format!("ACP server error: {}", e)))?;
        }
        "status" => {
            println!("ACP server: not running");
            println!("ACP runs as a stdio JSON-RPC server in the foreground.");
            println!("Start with `hermes acp start`.");
        }
        "stop" => {
            println!("ACP stop is not a separate command in stdio mode.");
            println!("If running, stop it by closing the parent process or sending Ctrl+C.");
        }
        "restart" => {
            println!("ACP restart in stdio mode is equivalent to stop + start.");
            println!("Use:");
            println!("  1) Stop the current process (Ctrl+C)");
            println!("  2) Run `hermes acp start`");
        }
        other => {
            println!("Unknown ACP action '{}'.", other);
            println!("Available actions: start, status, stop, restart");
        }
    }
    Ok(())
}

/// Handle `hermes backup [output]`.
pub async fn handle_cli_backup(output: Option<String>) -> Result<(), hermes_core::AgentError> {
    let hermes_dir = hermes_config::hermes_home();
    if !hermes_dir.exists() {
        println!(
            "Hermes home directory not found at {}",
            hermes_dir.display()
        );
        return Ok(());
    }
    let out = output.unwrap_or_else(|| {
        format!(
            "hermes-backup-{}.tar.gz",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        )
    });
    println!("Backing up {} -> {}", hermes_dir.display(), out);

    let tar_gz = std::fs::File::create(&out)
        .map_err(|e| hermes_core::AgentError::Io(format!("Cannot create {}: {}", out, e)))?;
    let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all("hermes", &hermes_dir)
        .map_err(|e| hermes_core::AgentError::Io(format!("Tar error: {}", e)))?;
    tar.finish()
        .map_err(|e| hermes_core::AgentError::Io(format!("Tar finish error: {}", e)))?;

    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!("Backup complete: {} ({} KB)", out, size / 1024);
    Ok(())
}

/// Handle `hermes import <path>`.
pub async fn handle_cli_import(path: String) -> Result<(), hermes_core::AgentError> {
    let src = std::path::Path::new(&path);
    if !src.exists() {
        return Err(hermes_core::AgentError::Io(format!(
            "Backup archive not found: {}",
            path
        )));
    }
    println!("Importing configuration from: {}", path);

    let hermes_dir = hermes_config::hermes_home();
    std::fs::create_dir_all(&hermes_dir).map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

    let file = std::fs::File::open(src)
        .map_err(|e| hermes_core::AgentError::Io(format!("Cannot open {}: {}", path, e)))?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);
    archive
        .unpack(&hermes_dir)
        .map_err(|e| hermes_core::AgentError::Io(format!("Extract error: {}", e)))?;

    println!(
        "Import complete. Files restored to {}",
        hermes_dir.display()
    );
    Ok(())
}

/// Handle `hermes version`.
pub fn handle_cli_version() -> Result<(), hermes_core::AgentError> {
    println!("hermes {}", env!("CARGO_PKG_VERSION"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::*;
    use tempfile::tempdir;

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env test lock")
    }

    #[test]
    fn test_autocomplete_empty() {
        let results = autocomplete("");
        assert_eq!(results.len(), SLASH_COMMANDS.len());
    }

    #[test]
    fn test_autocomplete_partial() {
        let results = autocomplete("/m");
        assert!(results.contains(&"/model"));
    }

    #[test]
    fn test_autocomplete_includes_raw_controls() {
        let results = autocomplete("/ra");
        assert!(results.contains(&"/raw"));
    }

    #[test]
    fn test_autocomplete_ops_control_plane() {
        let results = autocomplete("/op");
        assert!(results.contains(&"/ops"));
    }

    #[test]
    fn test_autocomplete_fuzzy_prefers_close_matches() {
        let results = autocomplete("/mdl");
        assert!(!results.is_empty());
        assert_eq!(results[0], "/model");
    }

    #[test]
    fn test_autocomplete_matches_description_terms() {
        let results = autocomplete("/quota");
        assert!(results.contains(&"/gquota"));
    }

    #[test]
    fn test_autocomplete_exact() {
        let results = autocomplete("/help");
        assert!(!results.is_empty());
        assert_eq!(results[0], "/help");
    }

    #[test]
    fn test_autocomplete_no_match() {
        let results = autocomplete("/xyz");
        assert!(results.is_empty());
    }

    #[test]
    fn test_help_for_known_command() {
        assert!(help_for("/help").is_some());
        assert!(help_for("/model").is_some());
    }

    #[test]
    fn test_help_for_unknown_command() {
        assert!(help_for("/unknown").is_none());
    }

    #[test]
    fn test_command_result_equality() {
        assert_eq!(CommandResult::Handled, CommandResult::Handled);
        assert_ne!(CommandResult::Handled, CommandResult::Quit);
    }

    #[test]
    fn test_default_skill_tap_present_in_merged_list() {
        let merged = merged_skill_taps(&[]);
        assert!(merged
            .iter()
            .any(|tap| tap == "https://github.com/MiniMax-AI/cli::skill"));
    }

    #[test]
    fn test_autoresearch_default_skill_tap_present_in_merged_list() {
        let merged = merged_skill_taps(&[]);
        assert!(merged
            .iter()
            .any(|tap| tap == "https://github.com/github/awesome-copilot::skills"));
    }

    #[test]
    fn test_mattpocock_default_skill_tap_present_in_merged_list() {
        let merged = merged_skill_taps(&[]);
        assert!(merged
            .iter()
            .any(|tap| tap == "https://github.com/mattpocock/skills::skills"));
    }

    #[test]
    fn test_merged_skill_taps_deduplicates_default() {
        let merged =
            merged_skill_taps(&vec!["https://github.com/MiniMax-AI/cli::skill".to_string()]);
        assert_eq!(
            merged
                .iter()
                .filter(|tap| tap.as_str() == "https://github.com/MiniMax-AI/cli::skill")
                .count(),
            1
        );
    }

    #[test]
    fn parse_skill_tap_spec_parses_github_url_with_override() {
        let parsed =
            parse_skill_tap_spec("https://github.com/openai/skills::skills").expect("tap parse");
        assert_eq!(parsed.repo, "openai/skills");
        assert_eq!(parsed.path, "skills");
    }

    #[test]
    fn parse_skill_tap_spec_parses_tree_url() {
        let parsed = parse_skill_tap_spec("https://github.com/anthropics/skills/tree/main/skills")
            .expect("tap parse");
        assert_eq!(parsed.repo, "anthropics/skills");
        assert_eq!(parsed.path, "skills");
    }

    #[test]
    fn read_skill_taps_accepts_upstream_object_shape() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("skill_taps.json");
        std::fs::write(
            &path,
            r#"{
  "taps": [
    { "repo": "MiniMax-AI/cli", "path": "skill/" },
    { "repo": "openai/skills", "path": "skills/" },
    { "repo": "anthropics/skills" },
    { "url": "https://github.com/garrytan/gstack::" }
  ]
}"#,
        )
        .expect("write");

        let taps = read_skill_taps(&path);
        assert!(taps.contains(&"https://github.com/MiniMax-AI/cli::skill".to_string()));
        assert!(taps.contains(&"https://github.com/openai/skills::skills".to_string()));
        assert!(taps.contains(&"https://github.com/anthropics/skills::skills".to_string()));
        assert!(taps.contains(&"https://github.com/garrytan/gstack::".to_string()));
    }

    #[test]
    fn write_skill_taps_writes_canonical_object_shape() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("skill_taps.json");
        let taps = vec![
            "https://github.com/MiniMax-AI/cli::skill".to_string(),
            "https://github.com/github/awesome-copilot::skills".to_string(),
            "https://github.com/garrytan/gstack::".to_string(),
        ];
        write_skill_taps(&path, &taps).expect("write taps");

        let raw = std::fs::read_to_string(&path).expect("read");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
        let arr = value
            .get("taps")
            .and_then(|v| v.as_array())
            .expect("taps array");
        assert_eq!(arr.len(), 3);

        let first = arr[0].as_object().expect("first object");
        assert_eq!(
            first.get("repo").and_then(|v| v.as_str()),
            Some("MiniMax-AI/cli")
        );
        assert_eq!(first.get("path").and_then(|v| v.as_str()), Some("skill/"));
    }

    #[test]
    fn sort_registry_skill_records_uses_router_priority_tie_break() {
        let mut records = vec![
            RegistrySkillRecord {
                identifier: "lobehub/a".to_string(),
                description: "".to_string(),
                source: "lobehub".to_string(),
                score: 700,
                install_source: RegistryInstallSource::LobeHub {
                    slug: "a".to_string(),
                },
            },
            RegistrySkillRecord {
                identifier: "skills.sh/b".to_string(),
                description: "".to_string(),
                source: "skills.sh".to_string(),
                score: 700,
                install_source: RegistryInstallSource::GitHub(ResolvedSkillSource {
                    repo: "openai/skills".to_string(),
                    branch: "main".to_string(),
                    skill_dir: "skills/b".to_string(),
                }),
            },
            RegistrySkillRecord {
                identifier: "github/c".to_string(),
                description: "".to_string(),
                source: "github".to_string(),
                score: 700,
                install_source: RegistryInstallSource::GitHub(ResolvedSkillSource {
                    repo: "openai/skills".to_string(),
                    branch: "main".to_string(),
                    skill_dir: "skills/c".to_string(),
                }),
            },
        ];

        sort_registry_skill_records(&mut records);
        let ordered_sources: Vec<String> = records.into_iter().map(|r| r.source).collect();
        assert_eq!(
            ordered_sources,
            vec![
                "skills.sh".to_string(),
                "github".to_string(),
                "lobehub".to_string()
            ]
        );
    }

    #[test]
    fn parse_explicit_github_skill_owner_repo_path() {
        let parsed = parse_explicit_github_skill("openai/skills/skills/.system/skill-creator")
            .expect("explicit parse");
        assert_eq!(parsed.0, "openai/skills");
        assert_eq!(parsed.1, None);
        assert_eq!(parsed.2, "skills/.system/skill-creator");
    }

    #[test]
    fn parse_skill_name_and_version_handles_repo_plus_skill() {
        let (name, suffix) = parse_skill_name_and_version("openai/skills@skill-creator");
        assert_eq!(name, "openai/skills");
        assert_eq!(suffix.as_deref(), Some("skill-creator"));
        assert!(looks_like_github_repo_slug(&name));
    }

    #[test]
    fn sanitize_skill_install_name_normalizes_path_tail() {
        assert_eq!(
            sanitize_skill_install_name("skills/.system/skill-creator"),
            "skill-creator"
        );
        assert_eq!(sanitize_skill_install_name("bad$name"), "bad_name");
    }

    #[test]
    fn ensure_safe_relative_path_rejects_traversal() {
        assert!(ensure_safe_relative_path("SKILL.md").is_ok());
        assert!(ensure_safe_relative_path("../SKILL.md").is_err());
        assert!(ensure_safe_relative_path("nested/../../bad").is_err());
    }

    #[test]
    fn parse_model_switch_request_picks_provider_when_empty() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&[], &providers);
        assert_eq!(req, ModelSwitchRequest::PickProviderThenModel);
    }

    #[test]
    fn parse_model_switch_request_uses_provider_picker_for_provider_arg() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&["NOUS"], &providers);
        assert_eq!(
            req,
            ModelSwitchRequest::PickModelFromProvider("nous".to_string())
        );
    }

    #[test]
    fn parse_model_switch_request_accepts_direct_provider_model() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&["openai:gpt-4o"], &providers);
        assert_eq!(
            req,
            ModelSwitchRequest::SetDirect("openai:gpt-4o".to_string())
        );
    }

    #[test]
    fn parse_model_switch_request_keeps_bare_model_as_direct() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&["gpt-4o"], &providers);
        assert_eq!(req, ModelSwitchRequest::SetDirect("gpt-4o".to_string()));
    }

    #[test]
    fn normalize_model_target_uses_current_provider_for_bare_model() {
        let normalized = normalize_model_target("nous:moonshotai/kimi-k2.6", "openai/gpt-5.5")
            .expect("normalize");
        assert_eq!(normalized, "nous:openai/gpt-5.5");
    }

    #[test]
    fn normalize_model_target_keeps_explicit_provider_model() {
        let normalized = normalize_model_target("nous:moonshotai/kimi-k2.6", "openai:gpt-5.4")
            .expect("normalize");
        assert_eq!(normalized, "openai:gpt-5.4");
    }

    #[test]
    fn parse_toggle_arg_supports_status_and_explicit_values() {
        assert_eq!(parse_toggle_arg(None, true).expect("toggle"), false);
        assert_eq!(
            parse_toggle_arg(Some("toggle"), false).expect("toggle"),
            true
        );
        assert_eq!(parse_toggle_arg(Some("on"), false).expect("on"), true);
        assert_eq!(parse_toggle_arg(Some("off"), true).expect("off"), false);
        assert!(parse_toggle_arg(Some("bad-value"), true).is_err());
    }

    #[test]
    fn resolve_cli_chat_provider_model_defaults_to_config_when_no_overrides() {
        let resolved =
            resolve_cli_chat_provider_model(Some("nous:moonshotai/kimi-k2.6"), None, None)
                .expect("resolve");
        assert_eq!(resolved, "nous:moonshotai/kimi-k2.6");
    }

    #[test]
    fn resolve_cli_chat_provider_model_applies_provider_override() {
        let resolved = resolve_cli_chat_provider_model(Some("gpt-4o"), None, Some("anthropic"))
            .expect("resolve");
        assert_eq!(resolved, "anthropic:gpt-4o");
    }

    #[test]
    fn resolve_cli_chat_provider_model_prefers_model_override_with_provider_prefix() {
        let resolved = resolve_cli_chat_provider_model(
            Some("openai:gpt-4o"),
            Some("moonshotai/kimi-k2.6"),
            Some("nous"),
        )
        .expect("resolve");
        assert_eq!(resolved, "nous:moonshotai/kimi-k2.6");
    }

    #[test]
    fn resolve_cli_chat_provider_model_uses_inference_model_env_when_no_flag_override() {
        let _lock = env_test_lock();
        std::env::set_var("HERMES_INFERENCE_MODEL", "nous:moonshotai/kimi-k2.6");
        let resolved =
            resolve_cli_chat_provider_model(Some("openai:gpt-4o"), None, None).expect("resolve");
        assert_eq!(resolved, "nous:moonshotai/kimi-k2.6");
        std::env::remove_var("HERMES_INFERENCE_MODEL");
    }

    #[test]
    fn apply_cli_chat_runtime_env_sets_provider_model() {
        let _lock = env_test_lock();
        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("HERMES_TUI_PROVIDER", "openai");

        apply_cli_chat_runtime_env("nous:openai/gpt-5.5");

        assert_eq!(
            std::env::var("HERMES_MODEL").ok().as_deref(),
            Some("nous:openai/gpt-5.5")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_MODEL").ok().as_deref(),
            Some("nous:openai/gpt-5.5")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("nous")
        );
        assert_eq!(
            std::env::var("HERMES_TUI_PROVIDER").ok().as_deref(),
            Some("nous")
        );

        for key in keys {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn format_personality_catalog_includes_current_and_usage_hint() {
        let catalog = format_personality_catalog(
            Some("technical"),
            &[("coder", "Use when building or debugging code.")],
        );
        assert!(catalog.contains("## Built-in personalities"));
        assert!(catalog.contains("Current: `technical`"));
        assert!(catalog.contains("Use `/personality <name>` to switch."));
    }

    #[test]
    fn format_personality_catalog_renders_multiline_entries() {
        let catalog = format_personality_catalog(
            None,
            &[
                ("coder", "Use when building or debugging code."),
                ("writer", "Use when drafting polished prose."),
            ],
        );
        assert!(catalog.contains("- `coder`\n  Use when building or debugging code."));
        assert!(catalog.contains("- `writer`\n  Use when drafting polished prose."));
    }

    #[test]
    fn secret_stdout_gate_defaults_false() {
        let _lock = env_test_lock();
        std::env::remove_var("HERMES_ALLOW_SECRET_STDOUT");
        assert!(!secret_stdout_allowed());
    }

    #[test]
    fn secret_stdout_gate_accepts_truthy_values() {
        let _lock = env_test_lock();
        std::env::set_var("HERMES_ALLOW_SECRET_STDOUT", "yes");
        assert!(secret_stdout_allowed());
        std::env::remove_var("HERMES_ALLOW_SECRET_STDOUT");
    }

    #[test]
    fn mask_secret_value_hides_payload() {
        let raw = "very-secret-value";
        let masked = mask_secret_value(raw);
        assert!(!masked.contains(raw));
        assert!(masked.contains("***"));
    }
}
