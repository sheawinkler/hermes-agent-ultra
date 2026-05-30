//! Hermes Skills Crate
//!
//! Implements the skills system (Requirement 12) for Hermes Agent.
//! Provides skill management, local file storage, hub client, security
//! validation, and versioning.

mod guard;
mod hub;
mod skill;
mod store;
mod version;

pub use guard::{
    check_skill_structure, content_hash, determine_verdict, resolve_trust_level, scan_skill_dir,
    scan_skill_file, should_allow_install, validate_skill, validate_skill_url, SkillGuard,
    SkillScanFinding, SkillScanReport, SkillScanVerdict, SkillTrustLevel,
    MAX_SINGLE_SKILL_FILE_BYTES, MAX_SKILL_FILE_COUNT,
};
pub use hub::{SkillUpdate, SkillsHubClient};
pub use skill::{SkillError, SkillManager, MAX_SKILL_CONTENT_CHARS};
pub use store::{FileSkillStore, SkillStore, MAX_SKILL_NAME_LENGTH};
pub use version::{compare_versions, compute_version, track_change, SkillChange, SkillVersion};
