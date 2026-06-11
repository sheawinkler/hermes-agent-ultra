use hermes_core::AgentError;

use crate::commands::{CommandResult, emit_command_output};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepoReviewBudgetProfile {
    Balanced,
    Aggressive,
    Relaxed,
    Off,
}

impl RepoReviewBudgetProfile {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "balanced" => Some(Self::Balanced),
            "aggressive" => Some(Self::Aggressive),
            "relaxed" => Some(Self::Relaxed),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Aggressive => "aggressive",
            Self::Relaxed => "relaxed",
            Self::Off => "off",
        }
    }
}

const REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD: &str = "HERMES_REPO_REVIEW_REPEAT_STREAK_THRESHOLD";
const REPO_REVIEW_BUDGET_ENV_LOW_SIGNAL_THRESHOLD: &str =
    "HERMES_REPO_REVIEW_LOW_SIGNAL_STREAK_THRESHOLD";
const REPO_REVIEW_BUDGET_ENV_KEEP_REPEAT: &str = "HERMES_REPO_REVIEW_KEEP_LIMIT_REPEAT";
const REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL: &str = "HERMES_REPO_REVIEW_KEEP_LIMIT_LOW_SIGNAL";
const REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE: &str = "HERMES_REPO_REVIEW_MIN_SIGNAL_SCORE";
const REPO_REVIEW_BUDGET_ENV_PROFILE: &str = "HERMES_REPO_REVIEW_BUDGET_PROFILE";

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RepoReviewBudgetRuntime {
    pub(crate) repeat_threshold: usize,
    pub(crate) low_signal_threshold: usize,
    pub(crate) keep_repeat: usize,
    pub(crate) keep_low_signal: usize,
    pub(crate) min_signal_score: f64,
    pub(crate) profile: RepoReviewBudgetProfile,
}

impl RepoReviewBudgetRuntime {
    pub(crate) fn from_env() -> Self {
        let repeat_threshold = std::env::var(REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(2)
            .clamp(1, 12);
        let low_signal_threshold = std::env::var(REPO_REVIEW_BUDGET_ENV_LOW_SIGNAL_THRESHOLD)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(2)
            .clamp(1, 12);
        let keep_repeat = std::env::var(REPO_REVIEW_BUDGET_ENV_KEEP_REPEAT)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(2)
            .clamp(1, 12);
        let keep_low_signal = std::env::var(REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .clamp(1, 12);
        let min_signal_score = std::env::var(REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE)
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(0.22)
            .clamp(0.0, 1.0);
        let profile = std::env::var(REPO_REVIEW_BUDGET_ENV_PROFILE)
            .ok()
            .as_deref()
            .and_then(RepoReviewBudgetProfile::parse)
            .unwrap_or(RepoReviewBudgetProfile::Balanced);
        Self {
            repeat_threshold,
            low_signal_threshold,
            keep_repeat,
            keep_low_signal,
            min_signal_score,
            profile,
        }
    }
}

pub(crate) fn apply_repo_review_budget_profile(profile: RepoReviewBudgetProfile) {
    let (repeat_threshold, low_signal_threshold, keep_repeat, keep_low_signal, min_signal_score) =
        match profile {
            RepoReviewBudgetProfile::Balanced => (2usize, 2usize, 2usize, 1usize, 0.22f64),
            RepoReviewBudgetProfile::Aggressive => (1usize, 1usize, 1usize, 1usize, 0.35f64),
            RepoReviewBudgetProfile::Relaxed => (3usize, 3usize, 3usize, 2usize, 0.15f64),
            RepoReviewBudgetProfile::Off => (12usize, 12usize, 12usize, 12usize, 0.01f64),
        };
    crate::env_vars::set_var(
        REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD,
        repeat_threshold.to_string(),
    );
    crate::env_vars::set_var(
        REPO_REVIEW_BUDGET_ENV_LOW_SIGNAL_THRESHOLD,
        low_signal_threshold.to_string(),
    );
    crate::env_vars::set_var(REPO_REVIEW_BUDGET_ENV_KEEP_REPEAT, keep_repeat.to_string());
    crate::env_vars::set_var(
        REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL,
        keep_low_signal.to_string(),
    );
    crate::env_vars::set_var(
        REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE,
        format!("{:.3}", min_signal_score),
    );
    crate::env_vars::set_var(REPO_REVIEW_BUDGET_ENV_PROFILE, profile.as_str());
}

pub(crate) fn handle_ops_budget_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let budget = RepoReviewBudgetRuntime::from_env();
        emit_command_output(
            host,
            format!(
                "repo_review_budget profile={}\n\
                 repeat_threshold={} low_signal_threshold={} keep_repeat={} keep_low_signal={} min_signal_score={:.2}",
                budget.profile.as_str(),
                budget.repeat_threshold,
                budget.low_signal_threshold,
                budget.keep_repeat,
                budget.keep_low_signal,
                budget.min_signal_score
            ),
        );
        return Ok(CommandResult::Handled);
    }
    match args[0].to_ascii_lowercase().as_str() {
        "list" => emit_command_output(
            host,
            "Repo-review budget profiles:\n- balanced: default trim cadence\n- aggressive: trim repetitive discovery quickly\n- relaxed: allow broader exploration before trimming\n- off: effectively disable trimming",
        ),
        "clear" => {
            for key in [
                REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD,
                REPO_REVIEW_BUDGET_ENV_LOW_SIGNAL_THRESHOLD,
                REPO_REVIEW_BUDGET_ENV_KEEP_REPEAT,
                REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL,
                REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE,
                REPO_REVIEW_BUDGET_ENV_PROFILE,
            ] {
                crate::env_vars::remove_var(key);
            }
            emit_command_output(host, "Cleared repo-review budget runtime overrides.");
        }
        profile_raw => {
            let Some(profile) = RepoReviewBudgetProfile::parse(profile_raw) else {
                emit_command_output(
                    host,
                    "Usage: /ops budget [status|list|balanced|aggressive|relaxed|off|clear]",
                );
                return Ok(CommandResult::Handled);
            };
            apply_repo_review_budget_profile(profile);
            let budget = RepoReviewBudgetRuntime::from_env();
            emit_command_output(
                host,
                format!(
                    "repo_review_budget set to '{}' (repeat={} low_signal={} keep_repeat={} keep_low_signal={} min_signal={:.2})",
                    profile.as_str(),
                    budget.repeat_threshold,
                    budget.low_signal_threshold,
                    budget.keep_repeat,
                    budget.keep_low_signal,
                    budget.min_signal_score
                ),
            );
        }
    }
    Ok(CommandResult::Handled)
}
