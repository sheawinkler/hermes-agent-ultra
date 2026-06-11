//! Skill installation types.

use serde::Deserialize;

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
pub(super) struct HermesSkillsIndexResponse {
    #[serde(default)]
    pub(super) skills: Vec<HermesSkillsIndexEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HermesSkillsIndexEntry {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) description: String,
    #[serde(default)]
    pub(super) source: String,
    #[serde(default)]
    pub(super) identifier: String,
    #[serde(default)]
    pub(super) repo: String,
    #[serde(default)]
    pub(super) path: String,
    #[serde(default)]
    pub(super) resolved_github_id: Option<String>,
    #[serde(default)]
    pub(super) tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct SkillsShSearchResponse {
    #[serde(default)]
    pub(super) skills: Vec<SkillsShSearchEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct SkillsShSearchEntry {
    #[serde(default)]
    pub(super) id: String,
    #[serde(default)]
    #[serde(rename = "skillId")]
    pub(super) skill_id: String,
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) source: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct LobeHubMeta {
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) description: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LobeHubAgentResponse {
    #[serde(default)]
    pub(super) author: String,
    #[serde(default)]
    pub(super) homepage: String,
    #[serde(default)]
    pub(super) summary: String,
    #[serde(default)]
    pub(super) meta: LobeHubMeta,
    #[serde(default)]
    pub(super) config: LobeHubConfig,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct LobeHubConfig {
    #[serde(default)]
    #[serde(rename = "systemRole")]
    pub(super) system_role: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ClawHubSkillDetailResponse {
    #[serde(default)]
    #[serde(rename = "latestVersion")]
    pub(super) latest_version: ClawHubLatestVersion,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ClawHubLatestVersion {
    #[serde(default)]
    pub(super) version: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct GitHubRepoInfo {
    pub(super) default_branch: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GitHubTreeEntry {
    pub(super) path: String,
    #[serde(rename = "type")]
    pub(super) kind: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct GitHubTreeResponse {
    pub(super) tree: Vec<GitHubTreeEntry>,
}
