//! Hermes Skills Crate
//!
//! Implements the skills system (Requirement 12) for Hermes Agent.
//! Provides skill management, local file storage, hub client, security
//! validation, and versioning.

mod curator;
mod curator_prompt;
mod guard;
mod hub;
mod hub_lock;
mod provenance;
mod skill;
mod skills_guard;
mod store;
mod sync;
mod usage;
mod version;

pub use guard::{
    check_skill_structure, content_hash as guard_content_hash, determine_verdict as guard_determine_verdict,
    resolve_trust_level as guard_resolve_trust_level, scan_skill_dir, scan_skill_file,
    should_allow_install as guard_should_allow_install, validate_skill, validate_skill_url,
    SkillGuard, SkillScanFinding, SkillScanReport, SkillScanVerdict, SkillTrustLevel,
    MAX_SINGLE_SKILL_FILE_BYTES, MAX_SKILL_FILE_COUNT,
};
pub use skills_guard::{
    content_hash, determine_verdict, resolve_trust_level, scan_bundle, scan_content, scan_skill,
    should_allow_install, Finding, InstallDecision, ScanResult, TRUSTED_REPOS,
};
pub use hub::{
    clawhub_file_refs, clawhub_finalize_search_results, clawhub_latest_version,
    clawhub_meta_from_payload, clawhub_metas_from_listing, ClawHubBundle, ClawHubFileRef,
    RegistrySkillMeta, SkillUpdate, SkillsHubClient,
};
pub use hub_lock::{
    hub_lock_path, read_hub_lock, resolve_scan_source, SkillHubInstalledEntry, SkillsHubLock,
    HUB_LOCK_FILE, HUB_LOCK_VERSION, HUB_STATE_DIR,
};
pub use provenance::{
    get_current_write_origin, is_background_review, normalize_write_origin,
    set_current_write_origin, WriteOriginGuard, ASSISTANT_TOOL, BACKGROUND_REVIEW, FOREGROUND,
};
pub use skill::{SkillError, SkillManager, MAX_SKILL_CONTENT_CHARS};
pub use store::{FileSkillStore, SkillStore, MAX_SKILL_NAME_LENGTH};
pub use sync::{
    bundled_skills_opt_out_marker, compute_relative_dest, dir_hash, discover_bundled_skills,
    is_bundled_skills_opt_out, read_manifest, read_skill_name, remove_pristine_bundled_skills,
    reset_bundled_skill, restore_official_optional_skill, set_bundled_skills_opt_out, sync_skills,
    write_manifest, BundledSkill, BundledSkillsOptOutResult, OfficialOptionalRestoreResult,
    PristineBundledSkillSkip, RemovePristineBundledSkillsResult, SkillResetResult, SkillSyncConfig,
    SkillSyncResult, NO_BUNDLED_SKILLS_MARKER,
};
pub use usage::{
    UsageStore, SkillUsageRecord, SkillUsageReportRow,
    is_agent_created, is_protected_skill,
    STATE_ACTIVE, STATE_ARCHIVED, STATE_STALE,
};
pub use version::{compare_versions, compute_version, track_change, SkillChange, SkillVersion};
pub use curator::{
    CuratorConfig, CuratorError, CuratorReviewResult, CuratorRunRecord, CuratorState,
    ToolCallRecord, TransitionResult, apply_automatic_transitions, build_curator_prompt,
    is_paused, load_curator_state, maybe_run_curator, run_curator_review, save_curator_state,
    set_paused, should_run_now,
    ConsolidationEntry, PruningEntry, StructuredSummary, AbsorbedDeclaration,
    ClassificationResult, CuratorRunReport, CuratorRunCounts,
    parse_structured_summary, extract_absorbed_into_declarations,
    classify_removed_skills, reconcile_classification, write_curator_report,
};
pub use curator_prompt::CURATOR_REVIEW_PROMPT;
