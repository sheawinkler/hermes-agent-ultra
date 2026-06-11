use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use hermes_core::AgentError;

use crate::alpha_runtime::{QuorumPolicy, load_quorum_policy};
use crate::model_switch::provider_model_ids;
use hermes_config::GatewayConfig;

use super::App;
use super::provider::resolve_provider_and_model;
pub(crate) const QUORUM_HINT_PREFIX: &str = "[QUORUM_MODE] ";
const QUORUM_MAX_VOTER_OUTPUT_CHARS: usize = 120_000;
pub(crate) const QUORUM_DEFAULT_VOTER_PASSES: usize = 6;
const QUORUM_AGENT_CONTRACT_DEFAULT_PATH: &str =
    "/Users/sheawinkler/Documents/Projects/hermes-agent-ultra/docs/QUORUM_AGENTS.md";

#[derive(Debug, Clone, Serialize)]
struct QuorumVoterOutcome {
    model: String,
    status: String,
    duration_ms: u64,
    total_turns: u32,
    tool_errors: usize,
    output: String,
    error: Option<String>,
}

impl App {
    fn quorum_voter_retry_limit() -> usize {
        if let Ok(raw) = std::env::var("HERMES_QUORUM_VOTER_MAX_RETRIES") {
            if Self::is_unbounded_token(&raw) {
                return 16;
            }
            if let Some(parsed) = raw.trim().parse::<usize>().ok().filter(|v| *v > 0) {
                return parsed.max(2);
            }
        }
        Self::auth_refresh_retry_limit().max(6)
    }
    fn quorum_force_refresh_each_voter() -> bool {
        Self::bool_env("HERMES_QUORUM_FORCE_REFRESH_EACH_VOTER").unwrap_or(false)
    }

    fn quorum_toolless_provider_fallback_enabled() -> bool {
        !matches!(
            Self::bool_env("HERMES_QUORUM_TOOLLESS_PROVIDER_FALLBACK"),
            Some(false)
        )
    }

    fn quorum_voter_tools_enabled() -> bool {
        !matches!(Self::bool_env("HERMES_QUORUM_VOTER_TOOLS"), Some(false))
    }

    fn quorum_synthesis_tools_enabled() -> bool {
        !matches!(Self::bool_env("HERMES_QUORUM_SYNTHESIS_TOOLS"), Some(false))
    }
    pub(crate) fn compose_quorum_messages(
        control_sections: Vec<String>,
        base_messages: Vec<hermes_core::Message>,
        trailing_user_context: Option<String>,
    ) -> Vec<hermes_core::Message> {
        let control_context = control_sections
            .into_iter()
            .map(|section| section.trim().to_string())
            .filter(|section| !section.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut merged_system_sections: Vec<String> = Vec::new();
        let mut non_system_messages: Vec<hermes_core::Message> = Vec::new();

        for message in base_messages {
            if message.role == hermes_core::MessageRole::System {
                if let Some(content) = message.content.as_deref().map(str::trim) {
                    if !content.is_empty() {
                        merged_system_sections.push(content.to_string());
                    }
                }
            } else {
                non_system_messages.push(message);
            }
        }

        let mut messages = Vec::new();
        if !merged_system_sections.is_empty() {
            messages.push(hermes_core::Message::system(
                merged_system_sections.join("\n\n"),
            ));
        }
        if !control_context.is_empty() {
            messages.push(hermes_core::Message::user(format!(
                "[QUORUM_CONTROL]\n{}",
                control_context
            )));
        }
        messages.extend(non_system_messages);
        if let Some(context) = trailing_user_context
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            messages.push(hermes_core::Message::user(context));
        }
        messages
    }

    pub(super) fn quorum_mode_armed_for_turn(&self) -> Option<QuorumPolicy> {
        let policy = match load_quorum_policy() {
            Ok(policy) => policy,
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream.stream_handle_shared,
                    format!("quorum policy load failed: {}", err),
                );
                return None;
            }
        };
        if !policy.enabled {
            if self.runtime.quorum_armed_once {
                Self::emit_lifecycle_event(
                    &self.stream.stream_handle_shared,
                    "quorum run requested but policy is disabled; run `/quorum on` first",
                );
            }
            return None;
        }
        let has_hint = self.session.messages.iter().any(|message| {
            message.role == hermes_core::MessageRole::System
                && message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with(QUORUM_HINT_PREFIX)
        });
        let has_user_turn = self
            .session
            .messages
            .iter()
            .any(|m| m.role == hermes_core::MessageRole::User);
        if !has_user_turn {
            if self.runtime.quorum_armed_once || has_hint {
                Self::emit_lifecycle_event(
                    &self.stream.stream_handle_shared,
                    "quorum armed but no user turn present yet; waiting for next user prompt",
                );
            }
            return None;
        }
        if !(self.runtime.quorum_armed_once || has_hint) {
            let auto_arm = std::env::var("HERMES_QUORUM_AUTO_ARM")
                .ok()
                .map(|raw| {
                    matches!(
                        raw.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on" | "auto"
                    )
                })
                .unwrap_or(false);
            if auto_arm {
                Self::emit_lifecycle_event(
                    &self.stream.stream_handle_shared,
                    "quorum auto-arm enabled via HERMES_QUORUM_AUTO_ARM=1",
                );
                return Some(policy);
            }
            return None;
        }
        Some(policy)
    }

    pub(super) fn clear_quorum_system_hints_inplace(&mut self) {
        self.session.messages.retain(|message| {
            if message.role != hermes_core::MessageRole::System {
                return true;
            }
            !message
                .content
                .as_deref()
                .unwrap_or_default()
                .starts_with(QUORUM_HINT_PREFIX)
        });
    }

    pub(crate) fn collect_quorum_models(policy: &QuorumPolicy, current_model: &str) -> Vec<String> {
        let mut models: Vec<String> = Vec::new();
        let push_unique = |target: &mut Vec<String>, raw: &str| {
            let candidate = raw.trim();
            if candidate.is_empty() {
                return;
            }
            if target.iter().any(|existing| existing == candidate) {
                return;
            }
            target.push(candidate.to_string());
        };
        for model in &policy.models {
            push_unique(&mut models, model);
        }
        if models.is_empty() {
            push_unique(&mut models, current_model);
        }
        let max_voters = policy.voters.clamp(2, 8);
        if models.len() < max_voters {
            push_unique(&mut models, current_model);
        }
        if models.len() > max_voters {
            models.truncate(max_voters);
        }
        models
    }

    pub(crate) fn quorum_voter_passes() -> usize {
        if let Ok(raw) = std::env::var("HERMES_QUORUM_VOTER_PASSES") {
            if Self::is_unbounded_token(&raw) {
                return 16;
            }
            if let Some(parsed) = raw.trim().parse::<usize>().ok().filter(|v| *v > 0) {
                return parsed.clamp(1, 16);
            }
        }
        QUORUM_DEFAULT_VOTER_PASSES
    }

    fn normalize_quorum_model_target(current_model: &str, raw: &str) -> String {
        let candidate = raw.trim();
        if candidate.is_empty() {
            return current_model.trim().to_string();
        }
        if let Some((provider, model)) = candidate.split_once(':') {
            return format!("{}:{}", provider.trim().to_ascii_lowercase(), model.trim());
        }
        let (provider, _) = resolve_provider_and_model(&GatewayConfig::default(), current_model);
        format!("{}:{}", provider.trim().to_ascii_lowercase(), candidate)
    }

    fn split_provider_model(provider_model: &str) -> (&str, &str) {
        if let Some((provider, model)) = provider_model.split_once(':') {
            (provider, model)
        } else {
            ("", provider_model)
        }
    }

    fn looks_like_version_pinned_model(model_id: &str) -> bool {
        let tail = model_id
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or(model_id)
            .to_ascii_lowercase();
        tail.as_bytes()
            .windows(8)
            .any(|window| window.iter().all(|byte| byte.is_ascii_digit()))
    }

    pub(super) fn resolve_quorum_catalog_candidate(
        requested_model: &str,
        catalog: &[String],
    ) -> Option<String> {
        if catalog.is_empty() {
            return None;
        }
        let requested_trimmed = requested_model.trim();
        if requested_trimmed.is_empty() {
            return catalog.first().cloned();
        }
        if let Some(hit) = catalog
            .iter()
            .find(|m| m.trim().eq_ignore_ascii_case(requested_trimmed))
        {
            return Some(hit.clone());
        }
        let requested_lc = requested_trimmed.to_ascii_lowercase();
        let slash_suffix = format!("/{}", requested_lc);
        if let Some(hit) = catalog.iter().find(|m| {
            let lower = m.trim().to_ascii_lowercase();
            lower.ends_with(&slash_suffix) || lower == requested_lc
        }) {
            return Some(hit.clone());
        }
        if Self::looks_like_version_pinned_model(requested_trimmed) {
            return None;
        }
        Self::rank_catalog_candidates(requested_trimmed, catalog, 1)
            .into_iter()
            .next()
    }

    pub(super) fn rank_catalog_candidates(
        requested_model: &str,
        catalog: &[String],
        limit: usize,
    ) -> Vec<String> {
        if catalog.is_empty() || limit == 0 {
            return Vec::new();
        }
        let requested = requested_model.trim().to_ascii_lowercase();
        if requested.is_empty() {
            return catalog.iter().take(limit).cloned().collect();
        }
        let requested_tail = requested.rsplit('/').next().unwrap_or(requested.as_str());
        let requested_norm: String = requested
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();

        let mut scored: Vec<(usize, usize, String)> = catalog
            .iter()
            .enumerate()
            .filter_map(|(idx, candidate)| {
                let cand_trimmed = candidate.trim();
                if cand_trimmed.is_empty() {
                    return None;
                }
                let cand = cand_trimmed.to_ascii_lowercase();
                let cand_tail = cand.rsplit('/').next().unwrap_or(cand.as_str());
                let cand_norm: String =
                    cand.chars().filter(|c| c.is_ascii_alphanumeric()).collect();

                let mut score = 0usize;
                if cand == requested {
                    score += 10_000;
                }
                if cand_tail == requested_tail {
                    score += 8_000;
                }
                if cand.ends_with(&format!("/{}", requested_tail)) {
                    score += 6_000;
                }
                if cand.contains(requested_tail) || requested_tail.contains(cand_tail) {
                    score += 2_000;
                }

                let shared_prefix = requested_norm
                    .chars()
                    .zip(cand_norm.chars())
                    .take_while(|(a, b)| a == b)
                    .count();
                score += shared_prefix.saturating_mul(40);

                let shared_chars = requested_norm
                    .chars()
                    .filter(|ch| cand_norm.contains(*ch))
                    .count();
                score += shared_chars.saturating_mul(12);

                let len_delta = requested_norm.len().abs_diff(cand_norm.len());
                score = score.saturating_sub(len_delta.saturating_mul(4));
                if score == 0 {
                    return None;
                }
                Some((score, idx, cand_trimmed.to_string()))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, _, candidate)| candidate)
            .collect()
    }

    async fn resolve_quorum_models(&self, policy: &QuorumPolicy) -> (Vec<String>, Vec<String>) {
        let raw = Self::collect_quorum_models(policy, &self.model.current_model);
        if raw.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut notes = Vec::new();
        let mut resolved = Vec::new();
        for raw_target in raw {
            let normalized =
                Self::normalize_quorum_model_target(&self.model.current_model, &raw_target);
            let (provider, model_id) = Self::split_provider_model(&normalized);
            let provider = provider.trim().to_ascii_lowercase();
            let model_id = model_id.trim();
            if provider.is_empty() || model_id.is_empty() {
                continue;
            }
            let mut final_target = normalized.clone();
            let catalog = provider_model_ids(&provider).await;
            if !catalog.is_empty() {
                if let Some(candidate) = Self::resolve_quorum_catalog_candidate(model_id, &catalog)
                {
                    final_target = format!("{}:{}", provider, candidate.trim());
                    if !final_target.eq_ignore_ascii_case(&normalized) {
                        notes.push(format!(
                            "quorum model remapped via catalog: {} -> {}",
                            normalized, final_target
                        ));
                    }
                } else if Self::looks_like_version_pinned_model(model_id) {
                    notes.push(format!(
                        "quorum model preserved despite catalog miss: {}",
                        normalized
                    ));
                } else if let Some(fallback) = catalog.first() {
                    let ranked = Self::rank_catalog_candidates(model_id, &catalog, 3);
                    final_target = format!("{}:{}", provider, fallback.trim());
                    notes.push(format!(
                        "quorum model not in provider catalog: {} ; fallback -> {} ; close matches: {}",
                        normalized,
                        final_target,
                        if ranked.is_empty() {
                            "(none)".to_string()
                        } else {
                            ranked.join(", ")
                        }
                    ));
                }
            }
            if !resolved
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&final_target))
            {
                resolved.push(final_target);
            }
        }
        (resolved, notes)
    }

    fn quorum_output_char_cap() -> Option<usize> {
        if let Ok(raw) = std::env::var("HERMES_QUORUM_MAX_VOTER_OUTPUT_CHARS") {
            if Self::is_unbounded_token(&raw) {
                return None;
            }
            if let Some(parsed) = raw.trim().parse::<usize>().ok().filter(|v| *v > 0) {
                return Some(parsed);
            }
        }
        Some(QUORUM_MAX_VOTER_OUTPUT_CHARS)
    }

    fn load_quorum_agent_contract_text(&self) -> Option<(PathBuf, String)> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(raw) = std::env::var("HERMES_QUORUM_AGENT_CONTRACT_PATH") {
            let path = PathBuf::from(raw.trim());
            if !path.as_os_str().is_empty() {
                candidates.push(path);
            }
        }
        candidates.push(self.state_root.join("quorum").join("AGENTS.md"));
        candidates.push(PathBuf::from(QUORUM_AGENT_CONTRACT_DEFAULT_PATH));
        for path in candidates {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let trimmed = content.trim();
            if trimmed.is_empty() {
                continue;
            }
            return Some((path, trimmed.to_string()));
        }
        None
    }

    fn build_quorum_voter_prompt(pass_index: usize, total_passes: usize, model: &str) -> String {
        if pass_index == 0 {
            return format!(
                "[QUORUM_VOTER] model={}\n\
                 You are in deep-voter mode. Act like quality is existential.\n\
                 Hard requirements:\n\
                 1) exhaustive exploration before conclusion,\n\
                 2) contradiction/null-hypothesis attack,\n\
                 3) final synthesis with explicit confidence and risk caveats,\n\
                 4) no placeholder names, no fake files, no invented metrics.\n\
                 Verification requirements:\n\
                 - every file/module claim must include an absolute path and exists_now=true/false\n\
                 - if you cannot verify a claim, mark it UNPROVEN (never guess)\n\
                 - include evidence bullets from tools/data/reasoning traces\n\
                 - include at least one counter-argument before final answer.\n\
                 Language requirement: answer in English unless the user explicitly requests another language.\n\
                 This is pass {}/{}.",
                model,
                pass_index + 1,
                total_passes
            );
        }
        format!(
            "[QUORUM_VOTER_REVIEW] pass {}/{}\n\
             Critique and strengthen your prior answer.\n\
             - Assume the previous draft is partially wrong.\n\
             - Remove any unverified file names/modules/metrics.\n\
             - Fix weak claims, tighten evidence, and improve actionability.\n\
             - Keep the answer in English unless the user explicitly requested another language.\n\
             - Keep objective truth over optimism.",
            pass_index + 1,
            total_passes
        )
    }

    pub(super) fn extract_last_assistant_output(messages: &[hermes_core::Message]) -> String {
        for message in messages.iter().rev() {
            if message.role != hermes_core::MessageRole::Assistant {
                continue;
            }
            if let Some(content) = message.content.as_deref() {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
            if let Some(reasoning) = message.reasoning_content.as_deref() {
                let trimmed = reasoning.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
        String::new()
    }

    fn truncate_for_quorum(text: &str, max_chars: Option<usize>) -> String {
        let Some(max_chars) = max_chars else {
            return text.to_string();
        };
        if max_chars == 0 || text.chars().count() <= max_chars {
            return text.to_string();
        }
        let keep = max_chars.saturating_sub(1);
        let mut out = String::with_capacity(max_chars + 24);
        for ch in text.chars().take(keep) {
            out.push(ch);
        }
        out.push('…');
        out
    }

    fn build_quorum_synthesis_prompt(
        policy: &QuorumPolicy,
        voter_outcomes: &[QuorumVoterOutcome],
    ) -> String {
        let required_success = Self::required_quorum_success(voter_outcomes.len());
        let mut prompt = String::new();
        prompt.push_str(
            "[QUORUM_SYNTHESIS] You must synthesize across independent model voters.\n\
             Rules:\n\
             1) Use only the voter outputs below as evidence.\n\
             2) Call out disagreements explicitly.\n\
             3) If a voter failed, mark it failed and continue.\n\
             4) Return: (a) strongest case, (b) strongest counter-case, (c) final synthesis with confidence.\n\
             5) Do not claim quorum executed unless voter outputs are present.\n\
             6) Reject placeholder names/fake files/fake metrics; keep only verified claims.\n\
             7) Any file claim in final synthesis must include absolute path + exists_now status or be marked UNPROVEN.\n",
        );
        prompt.push_str(
            "             8) Do not invent commands, tool calls, benchmark results, repository paths, execution evidence, or research citations.\n\
             9) Only cite a command/file/result if it appears verbatim in the voter output or the original user prompt; otherwise mark it UNPROVEN.\n\
             10) If voter evidence is thin or failed, say that directly instead of filling the gap.\n",
        );
        prompt.push_str(&format!(
            "Configured voters: {} | mode={} | enabled={} | required_success={}\n\n",
            policy.voters, policy.mode, policy.enabled, required_success
        ));
        for (idx, voter) in voter_outcomes.iter().enumerate() {
            prompt.push_str(&format!(
                "=== VOTER {} ===\nmodel: {}\nstatus: {}\nduration_ms: {}\nturns: {}\ntool_errors: {}\n",
                idx + 1,
                voter.model,
                voter.status,
                voter.duration_ms,
                voter.total_turns,
                voter.tool_errors
            ));
            if let Some(err) = &voter.error {
                prompt.push_str("error:\n");
                prompt.push_str(err);
                prompt.push('\n');
            }
            prompt.push_str("output:\n");
            prompt.push_str(&voter.output);
            prompt.push_str("\n\n");
        }
        prompt
    }

    fn persist_quorum_artifact(
        &self,
        policy: &QuorumPolicy,
        voter_outcomes: &[QuorumVoterOutcome],
    ) -> Result<PathBuf, AgentError> {
        let dir = self.state_root.join("quorum");
        std::fs::create_dir_all(&dir).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create quorum artifact dir {}: {}",
                dir.display(),
                e
            ))
        })?;
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
        let file_name = format!("{}-{}.json", self.session.session_id, timestamp);
        let path = dir.join(file_name);
        let payload = serde_json::json!({
            "session_id": self.session.session_id,
            "saved_at": chrono::Utc::now().to_rfc3339(),
            "policy": policy,
            "model_at_start": self.model.current_model,
            "voters": voter_outcomes,
        });
        let raw = serde_json::to_string_pretty(&payload)
            .map_err(|e| AgentError::Config(format!("Failed to serialize quorum artifact: {e}")))?;
        std::fs::write(&path, raw).map_err(|e| {
            AgentError::Io(format!(
                "Failed to write quorum artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        Ok(path)
    }

    fn update_quorum_artifact_with_synthesis(
        path: &Path,
        synthesis: &str,
    ) -> Result<(), AgentError> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            AgentError::Io(format!(
                "Failed to read quorum artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        let mut payload: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            AgentError::Config(format!(
                "Failed to parse quorum artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        payload["synthesis"] = serde_json::Value::String(synthesis.trim().to_string());
        payload["synthesis_saved_at"] = serde_json::Value::String(chrono::Utc::now().to_rfc3339());
        let updated = serde_json::to_string_pretty(&payload).map_err(|e| {
            AgentError::Config(format!(
                "Failed to serialize quorum synthesis artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        std::fs::write(path, updated).map_err(|e| {
            AgentError::Io(format!(
                "Failed to write quorum synthesis artifact {}: {}",
                path.display(),
                e
            ))
        })
    }
    pub(super) async fn run_quorum_fanout_turn(
        &mut self,
        run_started_at: Instant,
        policy: QuorumPolicy,
    ) -> Result<bool, AgentError> {
        let quorum_contract = self.load_quorum_agent_contract_text();
        let (voter_models, model_resolution_notes) = self.resolve_quorum_models(&policy).await;
        for note in model_resolution_notes {
            Self::emit_lifecycle_event(&self.stream.stream_handle_shared, note);
        }
        if voter_models.len() < 2 {
            Self::emit_lifecycle_event(
                &self.stream.stream_handle_shared,
                format!(
                    "quorum armed but only {} distinct model configured; falling back to normal run",
                    voter_models.len()
                ),
            );
            return Ok(false);
        }

        let (base_messages, reformulated) = self.build_inference_messages();
        if reformulated {
            Self::emit_lifecycle_event(
                &self.stream.stream_handle_shared,
                "runtime prompt reformulation injected (anti-scheming + context + tool routing + contradiction self-check)",
            );
        }
        let original_model = self.model.current_model.clone();
        let mut outcomes: Vec<QuorumVoterOutcome> = Vec::new();
        let mut succeeded = 0usize;
        let output_char_cap = Self::quorum_output_char_cap();

        Self::emit_phase_event(
            &self.stream.stream_handle_shared,
            "quorum",
            "multi-voter fan-out dispatch",
            30,
        );

        for (idx, model) in voter_models.iter().enumerate() {
            let display_index = idx + 1;
            Self::emit_lifecycle_event(
                &self.stream.stream_handle_shared,
                format!(
                    "quorum voter {}/{} dispatch -> {}",
                    display_index,
                    voter_models.len(),
                    model
                ),
            );
            if self.model.current_model != *model {
                self.switch_model(model);
            }
            let force_refresh = display_index == 1 || Self::quorum_force_refresh_each_voter();
            self.refresh_runtime_provider_credentials_if_needed(force_refresh)
                .await;

            let started = Instant::now();
            let max_attempts = Self::quorum_voter_retry_limit();
            let voter_passes = Self::quorum_voter_passes();
            let mut pass_errors: Vec<String> = Vec::new();
            let mut combined_output = String::new();
            let mut combined_turns: u32 = 0;
            let mut combined_tool_errors: usize = 0;
            let mut last_err: Option<AgentError> = None;
            let mut toolless_fallback_used = false;
            let voter_tools_enabled = Self::quorum_voter_tools_enabled();

            for pass_idx in 0..voter_passes {
                Self::emit_lifecycle_event(
                    &self.stream.stream_handle_shared,
                    format!(
                        "quorum voter {}/{} pass {}/{}",
                        display_index,
                        voter_models.len(),
                        pass_idx + 1,
                        voter_passes
                    ),
                );

                let mut system_sections = Vec::new();
                if let Some((contract_path, contract_text)) = quorum_contract.as_ref() {
                    system_sections.push(format!(
                        "[QUORUM_AGENT_CONTRACT]\npath={}\nApply this contract strictly for this voter pass:\n{}",
                        contract_path.display(),
                        contract_text
                    ));
                }
                system_sections.push(Self::build_quorum_voter_prompt(
                    pass_idx,
                    voter_passes,
                    model,
                ));
                let trailing_user_context = if pass_idx > 0 && !combined_output.trim().is_empty() {
                    Some(format!(
                        "[PRIOR_VOTER_DRAFT]\n{}\n\nCritique and strengthen this prior draft for pass {}/{}.",
                        combined_output,
                        pass_idx + 1,
                        voter_passes
                    ))
                } else {
                    None
                };
                let pass_messages = Self::compose_quorum_messages(
                    system_sections,
                    base_messages.clone(),
                    trailing_user_context,
                );

                let mut attempts = 0usize;
                let mut maybe_result: Option<hermes_core::AgentResult> = None;
                while attempts < max_attempts {
                    attempts += 1;
                    match self
                        .run_messages_with_current_agent_tools(
                            pass_messages.clone(),
                            false,
                            voter_tools_enabled,
                        )
                        .await
                    {
                        Ok(result) => {
                            maybe_result = Some(result);
                            break;
                        }
                        Err(err) => {
                            if Self::is_provider_tool_payload_error(&err)
                                && Self::quorum_toolless_provider_fallback_enabled()
                                && voter_tools_enabled
                                && !toolless_fallback_used
                            {
                                toolless_fallback_used = true;
                                pass_errors.push(format!(
                                    "pass {}: provider rejected tool schema on requested model; retried this voter pass without tool schemas",
                                    pass_idx + 1
                                ));
                                Self::emit_lifecycle_event(
                                    &self.stream.stream_handle_shared,
                                    format!(
                                        "quorum voter {}/{} provider rejected tool schema; retrying this voter pass without tool schemas",
                                        display_index,
                                        voter_models.len()
                                    ),
                                );
                                match self
                                    .run_messages_with_current_agent_tools(
                                        pass_messages.clone(),
                                        false,
                                        false,
                                    )
                                    .await
                                {
                                    Ok(result) => {
                                        maybe_result = Some(result);
                                        break;
                                    }
                                    Err(fallback_err) => {
                                        last_err = Some(fallback_err);
                                        break;
                                    }
                                }
                            }
                            if Self::is_provider_auth_or_session_error(&err)
                                && attempts < max_attempts
                            {
                                let refreshed = self.force_auth_refresh_after_error().await;
                                if refreshed {
                                    continue;
                                }
                            }
                            if Self::is_transient_retryable_error(&err) && attempts < max_attempts {
                                let backoff_ms = (attempts as u64).saturating_mul(750).max(500);
                                Self::emit_lifecycle_event(
                                    &self.stream.stream_handle_shared,
                                    format!(
                                        "quorum voter {}/{} transient error (attempt {}/{}): {} — retrying after {}ms",
                                        display_index,
                                        voter_models.len(),
                                        attempts,
                                        max_attempts,
                                        err,
                                        backoff_ms
                                    ),
                                );
                                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms))
                                    .await;
                                continue;
                            }
                            last_err = Some(err);
                            break;
                        }
                    }
                }

                let Some(result) = maybe_result else {
                    if let Some(err) = &last_err {
                        pass_errors.push(format!("pass {}: {}", pass_idx + 1, err));
                    } else {
                        pass_errors.push(format!("pass {}: unknown error", pass_idx + 1));
                    }
                    break;
                };

                combined_turns = combined_turns.saturating_add(result.total_turns);
                combined_tool_errors =
                    combined_tool_errors.saturating_add(result.tool_errors.len());
                let latest = Self::extract_last_assistant_output(&result.messages);
                if !latest.trim().is_empty() {
                    combined_output = latest;
                } else {
                    pass_errors.push(format!("pass {}: empty assistant output", pass_idx + 1));
                    break;
                }
            }

            if !combined_output.trim().is_empty() {
                let output = Self::truncate_for_quorum(&combined_output, output_char_cap);
                let degraded = Self::quorum_output_is_degraded_non_answer(&output);
                let status = if output.trim().is_empty() {
                    "empty"
                } else if degraded {
                    pass_errors.push("voter returned degraded non-answer".to_string());
                    "degraded"
                } else {
                    succeeded += 1;
                    "ok"
                };
                let error = if !pass_errors.is_empty() {
                    Some(pass_errors.join(" | "))
                } else if output.trim().is_empty() {
                    Some("voter returned empty assistant output".to_string())
                } else {
                    None
                };
                outcomes.push(QuorumVoterOutcome {
                    model: model.clone(),
                    status: status.to_string(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    total_turns: combined_turns,
                    tool_errors: combined_tool_errors,
                    output,
                    error,
                });
            } else {
                let err_text = last_err
                    .as_ref()
                    .map(ToString::to_string)
                    .or_else(|| (!pass_errors.is_empty()).then(|| pass_errors.join(" | ")))
                    .unwrap_or_else(|| "unknown voter error".to_string());
                outcomes.push(QuorumVoterOutcome {
                    model: model.clone(),
                    status: "error".to_string(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    total_turns: combined_turns,
                    tool_errors: combined_tool_errors,
                    output: String::new(),
                    error: Some(err_text),
                });
            }
        }

        if self.model.current_model != original_model {
            self.switch_model(&original_model);
        }
        let artifact_path = self.persist_quorum_artifact(&policy, &outcomes)?;
        Self::emit_lifecycle_event(
            &self.stream.stream_handle_shared,
            format!("quorum voter artifact saved: {}", artifact_path.display()),
        );

        let required_success = Self::required_quorum_success(voter_models.len());
        if succeeded < required_success {
            let error_summary = outcomes
                .iter()
                .map(|o| {
                    format!(
                        "{} => {}",
                        o.model,
                        match (o.status.as_str(), o.error.as_deref()) {
                            ("ok", _) => "ok".to_string(),
                            ("empty", Some(e)) => format!("empty ({})", e),
                            (_, Some(e)) => e.to_string(),
                            _ => "unknown error".to_string(),
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(" | ");
            return Err(AgentError::LlmApi(format!(
                "Quorum fan-out did not meet success threshold (required={}, got={}): {}",
                required_success, succeeded, error_summary
            )));
        }

        let synthesis_system = Self::build_quorum_synthesis_prompt(&policy, &outcomes);
        let mut synthesis_system_sections = Vec::new();
        if let Some((contract_path, contract_text)) = quorum_contract.as_ref() {
            synthesis_system_sections.push(format!(
                "[QUORUM_AGENT_CONTRACT]\npath={}\nApply this contract strictly for synthesis:\n{}",
                contract_path.display(),
                contract_text
            ));
        }
        synthesis_system_sections.push(synthesis_system);
        let synthesis_messages =
            Self::compose_quorum_messages(synthesis_system_sections, base_messages, None);

        Self::emit_phase_event(
            &self.stream.stream_handle_shared,
            "synthesis",
            "quorum synthesis from voter outputs",
            75,
        );
        let result = self
            .run_messages_with_current_agent_tools(
                synthesis_messages,
                true,
                Self::quorum_synthesis_tools_enabled(),
            )
            .await?;
        let total_turns = result.total_turns;
        let synthesis_text = Self::extract_last_assistant_output(&result.messages);
        if let Err(err) =
            Self::update_quorum_artifact_with_synthesis(&artifact_path, &synthesis_text)
        {
            tracing::warn!("quorum synthesis artifact update skipped: {}", err);
            Self::emit_lifecycle_event(
                &self.stream.stream_handle_shared,
                format!("warning: quorum synthesis artifact update skipped: {}", err),
            );
        }
        if let Err(err) = self.apply_agent_result_and_persist(result) {
            tracing::warn!("session autosave skipped: {}", err);
        }
        Self::emit_lifecycle_event(
            &self.stream.stream_handle_shared,
            format!(
                "quorum run finished in {:.2}s (voters={} succeeded={} total_turns={})",
                run_started_at.elapsed().as_secs_f64(),
                voter_models.len(),
                succeeded,
                total_turns
            ),
        );
        Self::emit_phase_event(
            &self.stream.stream_handle_shared,
            "finalize",
            "transcript finalization + persistence",
            100,
        );
        if let Some(handle) = &self.stream.stream_handle {
            handle.send_done();
        }
        Ok(true)
    }

    pub(crate) fn required_quorum_success(voter_count: usize) -> usize {
        let n = voter_count.max(1);
        (n / 2) + 1
    }
    pub(crate) fn quorum_output_is_degraded_non_answer(output: &str) -> bool {
        let lower = output.to_ascii_lowercase();
        lower.contains("objective delivery compromised")
            || lower.contains("reverting to hermes")
            || lower.contains("safe-mode response")
            || lower.contains("safe mode response")
            || (lower.contains("i do not have") && lower.contains("tools"))
            || (lower.contains("cannot access") && lower.contains("tools"))
    }
}
