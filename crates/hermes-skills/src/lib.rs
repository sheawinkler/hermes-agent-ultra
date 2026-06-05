//! Hermes Skills Crate
//!
//! Implements the skills system (Requirement 12) for Hermes Agent.
//! Provides skill management, local file storage, hub client, security
//! validation, and versioning.

mod guard;
mod hub;
mod provenance;
mod skill;
mod store;
mod sync;
mod usage;
mod version;

pub use guard::{
    check_skill_structure, content_hash, determine_verdict, resolve_trust_level, scan_skill_dir,
    scan_skill_file, should_allow_install, validate_skill, validate_skill_url, SkillGuard,
    SkillScanFinding, SkillScanReport, SkillScanVerdict, SkillTrustLevel,
    MAX_SINGLE_SKILL_FILE_BYTES, MAX_SKILL_FILE_COUNT,
};
pub use hub::{
    clawhub_file_refs, clawhub_finalize_search_results, clawhub_latest_version,
    clawhub_meta_from_payload, clawhub_metas_from_listing, ClawHubBundle, ClawHubFileRef,
    RegistrySkillMeta, SkillUpdate, SkillsHubClient,
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
    agent_created_report, archive_skill, bump_patch, bump_use, bump_view, forget, get_record,
    is_agent_created, is_protected_skill, list_agent_created_skill_names, load_usage,
    mark_agent_created, restore_skill, save_usage, set_pinned, set_state, usage_file,
    SkillUsageRecord, SkillUsageReportRow, STATE_ACTIVE, STATE_ARCHIVED, STATE_STALE,
};
pub use version::{compare_versions, compute_version, track_change, SkillChange, SkillVersion};
