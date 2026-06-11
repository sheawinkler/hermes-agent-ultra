//! Skill installation infrastructure — pure data-processing functions
//! extracted from mod.rs to keep slash command dispatch separate.
//!
//! This module has NO dependency on `App`, `CommandResult`, or slash-command
//! dispatch. It is used by `skills.rs` (CLI skills subcommand) and by unit
//! tests in the parent module.

mod bootstrap;
mod claude_marketplace;
mod clawhub;
mod constants;
mod fallback;
mod github;
mod hub_state;
mod install;
mod lobehub;
mod official;
mod parse;
mod registry;
mod skills_sh;
mod taps;
mod types;

pub(crate) use bootstrap::{
    collect_bootstrap_commands_from_value, execute_bootstrap_command,
    is_allowed_bootstrap_executable, maybe_run_skill_bootstrap, parse_bootstrap_command,
    parse_skill_bootstrap_plan, prompt_bootstrap_yes_no, push_bootstrap_command_if_present,
    skill_auto_bootstrap_enabled, skill_bootstrap_force_confirmed,
};
pub(crate) use claude_marketplace::resolve_claude_marketplace_skill;
pub(crate) use clawhub::{
    detect_archive_format, extract_clawhub_archive, fetch_clawhub_skill_files,
};
pub(crate) use constants::{
    DEFAULT_SKILL_TAPS, SENTRUX_MCP_ARG, SENTRUX_MCP_COMMAND, SENTRUX_MCP_SERVER_NAME,
};
pub(crate) use fallback::resolve_install_via_fallback_router;
pub(crate) use github::{
    fetch_skill_files_from_github, github_default_branch, github_repo_tree, github_request,
};
pub(crate) use hub_state::{
    append_skills_hub_audit, collect_skill_files_recursive, hash_installed_skill_dir,
    hash_skill_bundle, now_rfc3339, read_skills_hub_lock, record_skill_install_in_hub_lock,
    record_skill_uninstall_in_hub_lock, skill_guard_enforce_bundle, skills_hub_audit_path,
    skills_hub_lock_path, skills_hub_state_dir, skills_install_force, write_skills_hub_lock,
};
pub(crate) use install::{fetch_bundle_for_lock_entry, install_skill_files};
pub(crate) use lobehub::fetch_lobehub_skill_files;
pub(crate) use official::{
    canonicalize_official_skill_dir, official_skill_path_candidates, resolve_official_skill_source,
};
pub(crate) use parse::{
    canonicalize_skills_sh_identifier, ensure_safe_relative_path, looks_like_github_repo_slug,
    parse_explicit_github_skill, parse_registry_prefixed_skill, parse_repo_skill_identifier,
    parse_skill_name_and_version, parse_skill_tap_spec, sanitize_skill_install_name,
};
pub(crate) use registry::{
    build_lobehub_skill_markdown, default_trust_level_for_source, fetch_hermes_skills_index,
    resolve_skill_via_registry_index, resolved_source_from_index, score_registry_match,
    search_multi_registry, skill_source_priority, sort_registry_skill_records,
};
pub(crate) use skills_sh::{resolve_skills_sh_source, search_skills_sh_registry};
pub(crate) use taps::{
    effective_skill_taps, merged_skill_taps, normalize_tap_path_for_storage,
    read_skill_subscriptions, read_skill_taps, resolve_skill_in_repo, resolve_skill_via_taps,
    search_skills_via_taps, subscription_entry_to_source, subscription_source_to_tap,
    tap_object_to_string, tap_string_to_object, write_skill_taps,
};
pub(crate) use types::{
    GitHubTreeEntry, HermesSkillsIndexEntry, InstallFallbackSource, LobeHubAgentResponse,
    ParsedBootstrapCommand, RegistryInstallSource, RegistrySkillRecord, ResolvedSkillSource,
    SkillBootstrapPlan, SkillHubInstalledEntry, SkillInstallProvenance, SkillTapSpec,
    SkillsHubLockFile,
};
