//! Session-end orchestration: POI ingest → resolution → work package upload.

use std::path::PathBuf;
use std::sync::Arc;

use hermes_config::{InsightsContributionConfig, InterestConfig};
use hermes_insights::{
    drain_session_skills, find_skill_dir_by_slug, set_active_session, ContributionService,
    WorkPackageBuildInput,
};
use hermes_intelligence::auxiliary::AuxiliaryClient;
use tracing::{info, warn};

use crate::user_interest::{
    apply_signal_batch, extract_signals_from_messages, extract_signals_from_transcript_llm,
    filter_persistable_signals, filter_poi_signals, format_user_transcript_for_llm,
    InterestSignal, InterestStore,
};

use super::domain::{candidate_to_poi, extract_domain_candidate};
use super::metrics::build_work_metrics;
use super::resolution::{analyze_session, fuse_verdict};

pub fn spawn_session_end_pipeline(
    hermes_home: PathBuf,
    interest_cfg: InterestConfig,
    insights_cfg: InsightsContributionConfig,
    session_id: String,
    messages: Vec<serde_json::Value>,
    buffered: Vec<InterestSignal>,
    auxiliary: Option<Arc<AuxiliaryClient>>,
) {
    if !interest_cfg.enabled && !insights_cfg.enabled {
        return;
    }
    tokio::spawn(async move {
        if interest_cfg.enabled {
            run_poi_ingest(
                &hermes_home,
                &interest_cfg,
                &messages,
                buffered,
                auxiliary.as_ref(),
            )
            .await;
        }

        if !insights_cfg.enabled {
            return;
        }

        let packages = build_work_packages(&hermes_home, &insights_cfg, &session_id, &messages);
        if packages.is_empty() {
            return;
        }
        info!(count = packages.len(), "work_session: enqueue domain work packages");
        ContributionService::spawn_work_packages(hermes_home, insights_cfg, packages);
    });
}

async fn run_poi_ingest(
    hermes_home: &PathBuf,
    config: &InterestConfig,
    messages: &[serde_json::Value],
    buffered: Vec<InterestSignal>,
    auxiliary: Option<&Arc<AuxiliaryClient>>,
) {
    let db_path = hermes_home.join("interest.db");
    let Ok(store) = InterestStore::open(&db_path, config.clone()) else {
        warn!(path = %db_path.display(), "interest: failed to open interest.db for session-end ingest");
        return;
    };
    let transcript = format_user_transcript_for_llm(messages);
    let transcript_chars = transcript.chars().count();
    let mut all_signals = buffered;
    let buffered_n = all_signals.len();
    if config.session_end_llm_enabled() {
        if let Some(aux) = auxiliary {
            if transcript_chars == 0 {
                warn!("interest: session-end LLM skipped — empty user transcript");
            } else {
                let existing_labels = store.top_labels_for_llm(5).unwrap_or_default();
                let llm_signals = extract_signals_from_transcript_llm(
                    aux,
                    &transcript,
                    &existing_labels,
                )
                .await;
                let llm_n = llm_signals.len();
                all_signals.extend(llm_signals);
                info!(transcript_chars, llm_n, "interest: session-end LLM extraction");
            }
        } else {
            warn!("interest: session-end LLM enabled but auxiliary client unavailable");
        }
    }
    if config.uses_rules() {
        let rules = extract_signals_from_messages(messages);
        let rules_n = rules.len();
        all_signals.extend(rules);
        info!(
            transcript_chars,
            buffered_n,
            rules_n,
            "interest: session-end rule supplement"
        );
    }
    let pre_filter_n = all_signals.len();
    let all_signals = filter_persistable_signals(filter_poi_signals(all_signals));
    if all_signals.is_empty() {
        info!(
            transcript_chars,
            buffered_n,
            pre_filter_n,
            "interest: session-end POI pipeline — no persistable signals after gates"
        );
        return;
    }
    let _ = store.apply_decay();
    match apply_signal_batch(&store, config, all_signals) {
        Ok(report) => {
            if report.inserted + report.reinforced + report.merged > 0 {
                info!(
                    inserted = report.inserted,
                    reinforced = report.reinforced,
                    merged = report.merged,
                    promoted = report.promoted,
                    skipped = report.skipped,
                    "interest: session-end POI pipeline applied"
                );
            } else {
                info!(
                    skipped = report.skipped,
                    "interest: session-end POI pipeline — signals present but compare/update made no changes"
                );
            }
        }
        Err(err) => warn!("interest: session-end pipeline failed: {err}"),
    }
}

fn build_work_packages(
    hermes_home: &PathBuf,
    insights_cfg: &InsightsContributionConfig,
    session_id: &str,
    messages: &[serde_json::Value],
) -> Vec<WorkPackageBuildInput> {
    let skill_summary = drain_session_skills(hermes_home, session_id);
    if insights_cfg.require_skill_binding && skill_summary.slugs.is_empty() {
        return Vec::new();
    }

    let signals = analyze_session(messages, &skill_summary);
    if signals.user_turns < insights_cfg.min_work_turns {
        return Vec::new();
    }

    let Some(candidate) = extract_domain_candidate(messages) else {
        return Vec::new();
    };

    let resolution = fuse_verdict(&signals);
    let domain_poi = candidate_to_poi(&candidate);
    let session_id_hash = hermes_insights::types::sha256_hex(session_id.as_bytes());
    let work_metrics = build_work_metrics(
        signals.user_turns,
        signals.tool_failures,
        skill_summary.patch_count,
    );

    let skills_root = hermes_home.join("skills");
    let primary_slug = skill_summary.slugs.first().cloned().or_else(|| {
        messages.iter().find_map(|m| {
            m.get("tool_calls")?.as_array()?.iter().find_map(|tc| {
                let name = tc.get("function")?.get("name")?.as_str()?;
                if name == "skill_manage" {
                    tc.get("function")?
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
                } else {
                    None
                }
            })
        })
    });

    let Some(slug) = primary_slug else {
        return Vec::new();
    };
    let Some(skill_dir) = find_skill_dir_by_slug(&skills_root, &slug) else {
        return Vec::new();
    };

    let binding_role = if skill_summary.skill_created {
        "primary".to_string()
    } else if resolution.recovery_attempted {
        "recovery".to_string()
    } else {
        "primary".to_string()
    };

    vec![WorkPackageBuildInput {
        work_id: uuid::Uuid::new_v4().to_string(),
        session_id_hash,
        domain_poi,
        resolution,
        skill_dir,
        skills_root,
        binding_role,
        include_body: insights_cfg.redacted_body,
        work_metrics,
    }]
}

pub fn touch_active_session(hermes_home: &PathBuf, session_id: &str) {
    set_active_session(hermes_home, session_id);
}
