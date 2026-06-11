//! Skill installation constants.

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

pub(super) const GITHUB_API_BASE: &str = "https://api.github.com";
pub(super) const OFFICIAL_SKILLS_REPO: &str = "nousresearch/hermes-agent";
pub(super) const HERMES_SKILLS_INDEX_URL: &str =
    "https://hermes-agent.nousresearch.com/docs/api/skills-index.json";
pub(super) const SKILLS_SH_SEARCH_URL: &str = "https://skills.sh/api/search";
pub(super) const CLAWHUB_API_BASE: &str = "https://clawhub.ai/api/v1";
pub(super) const SKILLS_HUB_STATE_DIR: &str = hermes_skills::HUB_STATE_DIR;
pub(super) const SKILLS_HUB_AUDIT_FILE: &str = "audit.log";
pub(crate) const SENTRUX_MCP_SERVER_NAME: &str = "sentrux";
pub(crate) const SENTRUX_MCP_COMMAND: &str = "sentrux";
pub(crate) const SENTRUX_MCP_ARG: &str = "--mcp";
pub(super) const SKILL_BOOTSTRAP_ALLOWED_EXECUTABLES: &[&str] = &[
    "bash", "sh", "python", "python3", "pip", "pip3", "pipx", "uv", "uvx", "node", "npm", "npx",
    "pnpm", "yarn", "bun", "cargo", "rustup", "go", "make", "cmake", "git", "brew", "apt",
    "apt-get", "dnf", "yum", "pacman", "zypper", "apk",
];
