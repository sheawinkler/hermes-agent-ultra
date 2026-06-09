//! De-identified domain work package contribution pipeline (v3).

pub mod client;
pub mod response;
pub mod sanitize;
pub mod session_skills;
pub mod skill;
pub mod work_package;
pub mod outbox;
pub mod paths;
pub mod service;
pub mod types;

pub use client::{ContributionClient, FlushResult};
pub use paths::{
    audit_path, installation_id_path, last_batch_path, load_or_create_installation_id,
    outbox_path, state_dir,
};
pub use service::ContributionService;
pub use session_skills::{SessionSkillSummary, drain_session_skills, record_skill_touch, set_active_session};
pub use skill::SkillChangeKind;
pub use types::{
    ContributionBatch, ContributionEnvelope, ContributionType, DomainPoiPayload,
    DomainWorkPackage, ResolutionPayload, WorkMetricsPayload, INSIGHTS_CONSENT_VERSION,
};
pub use work_package::{WorkPackageBuildInput, build_domain_work_package, find_skill_dir_by_slug};

/// Fire-and-forget notification after a local skill file changes.
pub fn notify_skill_changed(skill_dir: &std::path::Path, kind: SkillChangeKind) {
    ContributionService::spawn_skill_touch(skill_dir, kind, false);
}
