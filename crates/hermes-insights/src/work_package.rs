//! Build v3 `domain_work_package` payloads from local skill dirs + session metadata.

use std::path::{Path, PathBuf};

use crate::sanitize::contains_residual_pii;
use crate::skill::{build_work_package_skill, SkillPatternOptions};
use crate::types::{
    validate_signal_codes, DomainPoiPayload, DomainWorkPackage, ResolutionPayload,
    WorkMetricsPayload, DOMAIN_WORK_PACKAGE_SCHEMA_VERSION,
};

#[derive(Debug, Clone)]
pub struct WorkPackageBuildInput {
    pub work_id: String,
    pub session_id_hash: String,
    pub domain_poi: DomainPoiPayload,
    pub resolution: ResolutionPayload,
    pub skill_dir: PathBuf,
    pub skills_root: PathBuf,
    pub binding_role: String,
    pub include_body: bool,
    pub work_metrics: WorkMetricsPayload,
}

pub fn build_domain_work_package(input: &WorkPackageBuildInput) -> Option<DomainWorkPackage> {
    if !validate_signal_codes(&input.resolution.signal_codes) {
        return None;
    }
    if input.resolution.verdict == "indeterminate" {
        return None;
    }
    if contains_residual_pii(&input.domain_poi.problem_statement_redacted) {
        return None;
    }

    let mut opts = SkillPatternOptions::default_for_work_package();
    opts.include_body = input.include_body;
    opts.domain_keys = vec![input.domain_poi.domain_key.clone()];
    opts.binding_role = input.binding_role.clone();

    let skill = build_work_package_skill(&input.skill_dir, &input.skills_root, &opts)?;

    Some(DomainWorkPackage {
        schema_version: DOMAIN_WORK_PACKAGE_SCHEMA_VERSION,
        work_id: input.work_id.clone(),
        session_id_hash: input.session_id_hash.clone(),
        domain_poi: input.domain_poi.clone(),
        resolution: input.resolution.clone(),
        skill,
        work_metrics: input.work_metrics.clone(),
    })
}

pub fn find_skill_dir_by_slug(skills_root: &Path, slug: &str) -> Option<PathBuf> {
    crate::skill::find_skill_dir_by_slug(skills_root, slug)
}
