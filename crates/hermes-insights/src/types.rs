//! REST API payload types (v3 domain work package).

use serde::{Deserialize, Serialize};

/// Consent document version shown on `hermes contribute enable`.
pub const INSIGHTS_CONSENT_VERSION: &str = "2026-06-15";

pub const DOMAIN_WORK_PACKAGE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributionType {
    DomainWorkPackage,
}

impl ContributionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DomainWorkPackage => "domain_work_package",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributionEnvelope {
    #[serde(rename = "type")]
    pub kind: String,
    pub collected_at: String,
    pub content_hash: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributionBatch {
    pub batch_id: String,
    pub consent_version: String,
    pub contributions: Vec<ContributionEnvelope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProblemClass {
    Operational,
    Technical,
    Compliance,
    Creative,
    Research,
}

impl ProblemClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operational => "operational",
            Self::Technical => "technical",
            Self::Compliance => "compliance",
            Self::Creative => "creative",
            Self::Research => "research",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DifficultyBand {
    Low,
    Med,
    High,
}

impl DifficultyBand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Med => "med",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainPoiPayload {
    pub domain_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taxonomy_code: Option<String>,
    pub problem_class: String,
    pub problem_statement_redacted: String,
    pub difficulty_band: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionVerdict {
    SolvedConfirmed,
    SolvedInferred,
    Partial,
    Unresolved,
    Failed,
    Abandoned,
    Indeterminate,
}

impl ResolutionVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SolvedConfirmed => "solved_confirmed",
            Self::SolvedInferred => "solved_inferred",
            Self::Partial => "partial",
            Self::Unresolved => "unresolved",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
            Self::Indeterminate => "indeterminate",
        }
    }

    pub fn is_reportable(self, exclude: &[String]) -> bool {
        if self == Self::Indeterminate {
            return false;
        }
        !exclude.iter().any(|v| v == self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTier {
    A,
    B,
    C,
    D,
}

impl EvidenceTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
        }
    }

    pub fn meets_min(self, min: &str) -> bool {
        let min = min.trim().to_ascii_uppercase();
        let order = |t: Self| match t {
            Self::A => 4,
            Self::B => 3,
            Self::C => 2,
            Self::D => 1,
        };
        let min_tier = match min.as_str() {
            "A" => Self::A,
            "B" => Self::B,
            "C" => Self::C,
            _ => Self::D,
        };
        order(self) >= order(min_tier)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionPayload {
    pub verdict: String,
    pub confidence_band: String,
    pub evidence_tier: String,
    pub user_feedback_band: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective_check_band: Option<String>,
    pub signal_codes: Vec<String>,
    pub recovery_attempted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillStructure {
    pub headings: Vec<String>,
    pub step_count: u32,
    pub mentions_subagent: bool,
    pub mentions_cron: bool,
    pub mentions_mcp: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTriggerHints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slash_command: Option<String>,
    #[serde(default)]
    pub from_background_review: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillProvenance {
    AgentCreated,
    UserCreated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkPackageSkillPayload {
    pub pattern_id: String,
    pub display_name: String,
    pub name_slug: String,
    pub binding_role: String,
    pub domain_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description_redacted: String,
    pub structure: SkillStructure,
    pub tool_chain: Vec<String>,
    pub trigger_hints: SkillTriggerHints,
    pub provenance: SkillProvenance,
    pub content_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_body: Option<String>,
    #[serde(default)]
    pub references_redacted: Vec<SkillReferenceSnippet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillReferenceSnippet {
    pub relative_path: String,
    pub content_redacted: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkMetricsPayload {
    pub turn_band: String,
    pub duration_band: String,
    pub tool_failure_band: String,
    pub skill_patch_count_band: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainWorkPackage {
    pub schema_version: u32,
    pub work_id: String,
    pub session_id_hash: String,
    pub domain_poi: DomainPoiPayload,
    pub resolution: ResolutionPayload,
    pub skill: WorkPackageSkillPayload,
    pub work_metrics: WorkMetricsPayload,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BatchUploadResponse {
    #[serde(default, alias = "accepted_count", alias = "acceptedCount")]
    pub accepted: u32,
    #[serde(default, alias = "duplicate_count", alias = "duplicateCount")]
    pub duplicates: u32,
    #[serde(default)]
    pub rejected: Vec<RejectedContribution>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RejectedContribution {
    pub content_hash: String,
    pub reason: String,
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

pub fn envelope_from_value(
    kind: ContributionType,
    collected_at: &str,
    payload: &impl Serialize,
) -> Result<ContributionEnvelope, String> {
    let payload_value =
        serde_json::to_value(payload).map_err(|e| format!("serialize payload: {e}"))?;
    let canonical =
        serde_json::to_string(&payload_value).map_err(|e| format!("canonical payload: {e}"))?;
    Ok(ContributionEnvelope {
        kind: kind.as_str().to_string(),
        collected_at: collected_at.to_string(),
        content_hash: sha256_hex(canonical.as_bytes()),
        payload: payload_value,
    })
}

/// Drop duplicate work packages in one batch (same `work_id`, keep last).
pub fn dedupe_batch_contributions(contribs: Vec<ContributionEnvelope>) -> Vec<ContributionEnvelope> {
    let mut idx: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(contribs.len());
    for env in contribs {
        if env.kind != ContributionType::DomainWorkPackage.as_str() {
            out.push(env);
            continue;
        }
        let Some(wid) = work_package_id(&env.payload) else {
            out.push(env);
            continue;
        };
        if let Some(&i) = idx.get(&wid) {
            out[i] = env;
        } else {
            idx.insert(wid, out.len());
            out.push(env);
        }
    }
    out
}

pub fn work_package_id(payload: &serde_json::Value) -> Option<String> {
    payload.get("work_id")?.as_str().map(str::to_string)
}

pub fn validate_signal_codes(codes: &[String]) -> bool {
    codes.iter().all(|c| ALLOWED_SIGNAL_CODES.contains(&c.as_str()))
}

pub const ALLOWED_SIGNAL_CODES: &[&str] = &[
    "user_explicit_positive",
    "user_explicit_negative",
    "user_correction_loop",
    "closure_without_followup",
    "followup_same_poi_later",
    "objective_test_pass",
    "objective_test_fail",
    "objective_not_applicable",
    "skill_created_this_session",
    "skill_patched_this_session",
    "insufficient_turns",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_tier_ordering() {
        assert!(EvidenceTier::A.meets_min("B"));
        assert!(EvidenceTier::B.meets_min("B"));
        assert!(!EvidenceTier::C.meets_min("B"));
    }
}
