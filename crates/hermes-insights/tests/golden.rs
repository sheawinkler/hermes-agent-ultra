//! Golden tests for sanitizer and v3 work package building.

use std::fs;
use std::path::Path;

use hermes_insights::sanitize::sanitize_text;
use hermes_insights::skill::{build_work_package_skill, SkillPatternOptions};
use hermes_insights::types::{
    DomainPoiPayload, ResolutionPayload, WorkMetricsPayload, INSIGHTS_CONSENT_VERSION,
};
use hermes_insights::work_package::{build_domain_work_package, WorkPackageBuildInput};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

#[test]
fn golden_skill_with_pii_strips_sensitive_content() {
    let raw = fs::read_to_string(Path::new(FIXTURES).join("skill_with_pii.md")).unwrap();
    let redacted = sanitize_text(&raw);
    assert!(!redacted.contains("user@example.com"));
    assert!(!redacted.contains("sk-live-secretkey"));
    assert!(!redacted.contains("C:\\Users\\alice"));
}

#[test]
fn golden_domain_work_package_shape() {
    assert_eq!(INSIGHTS_CONSENT_VERSION, "2026-06-15");
    let tmp = tempfile::tempdir().unwrap();
    let skills_root = tmp.path().join("skills");
    let skill_dir = skills_root.join("demo-skill");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: Demo workflow\n---\n\n## Steps\n1. Do thing\n",
    )
    .unwrap();

    let input = WorkPackageBuildInput {
        work_id: "7c9e6679-7425-40de-944b-e07fc1f90ae7".into(),
        session_id_hash: hermes_insights::types::sha256_hex(b"session-1"),
        domain_poi: DomainPoiPayload {
            domain_key: "topic:demo-skill".into(),
            taxonomy_code: None,
            problem_class: "technical".into(),
            problem_statement_redacted: "Demo workflow for tests".into(),
            difficulty_band: "low".into(),
        },
        resolution: ResolutionPayload {
            verdict: "solved_inferred".into(),
            confidence_band: "medium".into(),
            evidence_tier: "B".into(),
            user_feedback_band: "neutral".into(),
            objective_check_band: Some("pass".into()),
            signal_codes: vec![
                "objective_test_pass".into(),
                "closure_without_followup".into(),
            ],
            recovery_attempted: false,
        },
        skill_dir: skill_dir.clone(),
        skills_root: skills_root.clone(),
        binding_role: "primary".into(),
        include_body: true,
        work_metrics: WorkMetricsPayload {
            turn_band: "3-5".into(),
            duration_band: "unknown".into(),
            tool_failure_band: "0".into(),
            skill_patch_count_band: "1".into(),
        },
    };

    let package = build_domain_work_package(&input).expect("package");
    assert_eq!(package.schema_version, 1);
    assert_eq!(package.resolution.verdict, "solved_inferred");
    assert!(package.skill.display_name.contains("demo"));

    let opts = SkillPatternOptions::default_for_work_package();
    assert!(build_work_package_skill(&skill_dir, &skills_root, &opts).is_some());
}
