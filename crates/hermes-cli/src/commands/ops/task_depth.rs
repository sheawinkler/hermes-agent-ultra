#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskDepthProfile {
    Shallow,
    Balanced,
    Deep,
    Max,
}

impl TaskDepthProfile {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "shallow" | "fast" => Some(Self::Shallow),
            "balanced" | "default" => Some(Self::Balanced),
            "deep" | "thorough" => Some(Self::Deep),
            "max" | "exhaustive" => Some(Self::Max),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Shallow => "shallow",
            Self::Balanced => "balanced",
            Self::Deep => "deep",
            Self::Max => "max",
        }
    }
}

fn set_env_var_u64(key: &str, value: u64) {
    crate::env_vars::set_var(key, value.to_string());
}

fn set_env_var_f64(key: &str, value: f64) {
    crate::env_vars::set_var(key, format!("{value:.2}"));
}

pub(crate) fn apply_task_depth_profile(profile: TaskDepthProfile) {
    crate::env_vars::set_var("HERMES_TASK_DEPTH_PROFILE", profile.as_str());
    match profile {
        TaskDepthProfile::Shallow => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 18);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 10);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 1);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 6);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 2800.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 5200.0);
            crate::env_vars::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "aggressive");
        }
        TaskDepthProfile::Balanced => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 250);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 12);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 4);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 8);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 3500.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 6500.0);
            crate::env_vars::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "off");
        }
        TaskDepthProfile::Deep => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 120);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 6);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 3);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 10);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 4800.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 9000.0);
            crate::env_vars::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "relaxed");
        }
        TaskDepthProfile::Max => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 250);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 5);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 4);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 12);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 6500.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 12000.0);
            crate::env_vars::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "off");
        }
    }
}

fn current_task_depth_profile() -> TaskDepthProfile {
    std::env::var("HERMES_TASK_DEPTH_PROFILE")
        .ok()
        .as_deref()
        .and_then(TaskDepthProfile::parse)
        .unwrap_or(TaskDepthProfile::Balanced)
}

pub(crate) fn task_depth_runtime_summary() -> String {
    let profile = current_task_depth_profile();
    let max_iters = std::env::var("HERMES_MAX_ITERATIONS").unwrap_or_else(|_| "250".to_string());
    let tool_concurrency =
        std::env::var("HERMES_TOOL_CALL_MAX_CONCURRENCY").unwrap_or_else(|_| "12".to_string());
    let delegate_depth =
        std::env::var("HERMES_MAX_DELEGATE_DEPTH").unwrap_or_else(|_| "4".to_string());
    let repo_budget =
        std::env::var("HERMES_REPO_REVIEW_BUDGET_PROFILE").unwrap_or_else(|_| "off".to_string());
    format!(
        "task_depth profile={} max_iterations={} tool_concurrency={} max_delegate_depth={} repo_budget_profile={}",
        profile.as_str(),
        max_iters,
        tool_concurrency,
        delegate_depth,
        repo_budget
    )
}
