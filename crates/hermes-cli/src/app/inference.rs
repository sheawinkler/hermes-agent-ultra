use crate::alpha_runtime::canonical_objective_behavior_mode;

use super::App;

const RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS: usize = 1_600;

impl App {
    pub(crate) const RUNTIME_REFORMULATION_PREFIX: &'static str = "[HERMES_RUNTIME_REFORMULATION] ";

    pub(super) fn runtime_prompt_reformulation_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_RUNTIME_PROMPT_REFORMULATION")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    pub(super) fn runtime_contradiction_self_check_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_RUNTIME_CONTRADICTION_SELF_CHECK")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    pub(super) fn runtime_reformulation_prompt_preview_chars() -> usize {
        std::env::var("HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS)
    }

    pub(super) fn current_tool_profile_mode() -> String {
        std::env::var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "balanced".to_string())
    }

    pub(super) fn build_runtime_reformulation_message(
        &self,
        latest_user_prompt: &str,
    ) -> Option<String> {
        if !Self::runtime_prompt_reformulation_enabled() {
            return None;
        }
        let prompt = latest_user_prompt.trim();
        if prompt.is_empty() {
            return None;
        }
        let tool_profile_mode = Self::current_tool_profile_mode();
        let contradiction_check = Self::runtime_contradiction_self_check_enabled();
        let context_topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "runbooks/hermes".to_string());

        let objective_contract = Self::load_active_objective_contract();
        let objective_line = objective_contract
            .as_ref()
            .map(|contract| {
                format!(
                    "objective(active): {} | behavior={} | text={}",
                    contract.id,
                    canonical_objective_behavior_mode(&contract.behavior_mode),
                    Self::preview_for_status(&contract.objective_text, 220)
                )
            })
            .unwrap_or_else(|| "objective(active): none".to_string());
        let objective_directives = objective_contract
            .as_ref()
            .map(|contract| {
                contract
                    .behavior_directives
                    .iter()
                    .take(6)
                    .map(|line| format!("- {}", line.trim()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "- (none)".to_string());
        let objective_success = objective_contract
            .as_ref()
            .map(|contract| {
                contract
                    .success_criteria
                    .iter()
                    .take(5)
                    .map(|line| format!("- {}", line.trim()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "- (none)".to_string());

        let contradiction_line = if contradiction_check {
            "before final response: self-audit contradictions across tool outputs, runtime facts, and claims; unresolved items must be marked UNPROVEN/CONTRADICTORY."
        } else {
            "before final response: consistency self-audit optional (disabled by runtime toggle)."
        };

        let mut out = String::new();
        out.push_str(Self::RUNTIME_REFORMULATION_PREFIX);
        out.push_str(
            "\nRuntime execution reformulation (internal):\n\
             1) apply anti-scheming evidence-first discipline\n\
             2) pull ContextLattice context first when relevant\n\
             3) route tool usage intentionally and avoid repetitive low-signal loops\n\
             4) match requested output shape exactly (count/format), with no template placeholders or duplicate list items\n\
             5) for open-ended missions, execute at least one concrete action before returning status text\n\
             6) maintain iterative objective momentum: gather evidence, test, refine, then continue with next high-value action\n",
        );
        out.push_str(&format!(
            "tool-profile(mode): {}\ncontextlattice(topic): {}\n{}\n",
            tool_profile_mode, context_topic, objective_line
        ));
        out.push_str("objective behavior directives:\n");
        out.push_str(&objective_directives);
        out.push('\n');
        out.push_str("objective success criteria:\n");
        out.push_str(&objective_success);
        out.push('\n');
        out.push_str(
            "objective loop protocol:\n\
             - baseline: state current objective KPI and latest known value\n\
             - execute: perform concrete highest-leverage action now\n\
             - verify: present measurable delta or explicit blocked evidence\n\
             - continue: state next action with no soft deferral\n",
        );
        out.push_str(contradiction_line);
        out.push_str("\nuser-request(routing-preview):\n");
        let preview_cap = Self::runtime_reformulation_prompt_preview_chars();
        let prompt_preview = Self::preview_for_status(prompt, preview_cap);
        out.push_str(&prompt_preview);
        if prompt.chars().count() > preview_cap {
            out.push_str(
                "\n[preview truncated; the full user request remains available as the next user message]",
            );
        } else {
            out.push_str("\n[full user request remains available as the next user message]");
        }
        Some(out)
    }

    pub(super) fn build_inference_messages(&self) -> (Vec<hermes_core::Message>, bool) {
        let mut messages = self.session.messages.clone();
        let Some(last_user_idx) = messages
            .iter()
            .rposition(|m| m.role == hermes_core::MessageRole::User)
        else {
            return (messages, false);
        };
        let user_prompt = messages[last_user_idx]
            .content
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string();
        let Some(reformulation) = self.build_runtime_reformulation_message(&user_prompt) else {
            return (messages, false);
        };
        messages.insert(last_user_idx, hermes_core::Message::system(reformulation));
        (messages, true)
    }

    pub(super) fn apply_explore_first_runtime_defaults() {
        if std::env::var("HERMES_SKILL_GUARD_MODE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            crate::env_vars::set_var("HERMES_SKILL_GUARD_MODE", "off");
        }
        if std::env::var("HERMES_GUARD_MODE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            crate::env_vars::set_var("HERMES_GUARD_MODE", "off");
        }
        if std::env::var("HERMES_TOOL_POLICY_PRESET")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            crate::env_vars::set_var("HERMES_TOOL_POLICY_PRESET", "dev");
        }
        if std::env::var("HERMES_TOOL_POLICY_MODE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            crate::env_vars::set_var("HERMES_TOOL_POLICY_MODE", "audit");
        }
        if std::env::var("HERMES_REPO_REVIEW_BUDGET_PROFILE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            crate::env_vars::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "off");
        }
        if std::env::var("HERMES_MAX_ITERATIONS")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            crate::env_vars::set_var("HERMES_MAX_ITERATIONS", "250");
        }
        if std::env::var("HERMES_TOOL_CALL_MAX_CONCURRENCY")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            crate::env_vars::set_var("HERMES_TOOL_CALL_MAX_CONCURRENCY", "12");
        }
        if std::env::var("HERMES_MAX_DELEGATE_DEPTH")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            crate::env_vars::set_var("HERMES_MAX_DELEGATE_DEPTH", "4");
        }
    }
}
