//! Hermes Skills Crate
//!
//! Implements the skills system (Requirement 12) for Hermes Agent.
//! Provides skill management, local file storage, hub client, security
//! validation, and versioning.

mod guard;
mod hub;
mod hub_lock;
mod skill;
mod skills_guard;
mod store;
mod version;

pub use guard::{validate_skill, validate_skill_url, SkillGuard};
pub use skills_guard::{
    content_hash, determine_verdict, resolve_trust_level, scan_bundle, scan_content, scan_skill,
    should_allow_install, Finding, InstallDecision, ScanResult, TRUSTED_REPOS,
};
pub use hub::{SkillUpdate, SkillsHubClient};
pub use hub_lock::{read_hub_lock, resolve_scan_source, hub_lock_path, SkillHubInstalledEntry, SkillsHubLock};
pub use skill::{SkillError, SkillManager};
pub use store::{FileSkillStore, SkillStore};
pub use version::{compare_versions, compute_version, track_change, SkillChange, SkillVersion};
