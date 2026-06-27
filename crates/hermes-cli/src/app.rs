//! Application state management for the interactive CLI.
//!
//! The `App` struct owns the configuration, agent loop, tool registry,
//! and conversation message history. It coordinates input handling,
//! slash commands, and session management.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use hermes_agent::agent_loop::ToolRegistry as AgentToolRegistry;
use hermes_agent::plugins::HookType;
use hermes_agent::sub_agent_orchestrator::SubAgentOrchestrator;
use hermes_agent::{AgentCallbacks, AgentLoop, InterruptController, SessionPersistence};
pub use hermes_app_runtime::build_agent_config;
use hermes_app_runtime::{
    build_runtime_reformulation_message as build_runtime_reformulation_message_for_runtime,
    RuntimeReformulationObjective,
};
use hermes_config::{hermes_home as hermes_home_dir, load_config, state_dir, GatewayConfig};
use hermes_core::ToolSchema;
use hermes_core::{AgentError, LlmProvider, UsageStats};
use hermes_cron::{CronRunner, CronScheduler, FileJobPersistence};
use hermes_intelligence::{build_model_switch_preflight_warning, estimate_messages_tokens_rough};
pub use hermes_provider_runtime::{
    active_llm_provider_config, allow_no_api_key, normalize_runtime_provider_name,
    provider_api_key_from_env, provider_base_url_from_env, provider_default_base_url,
    resolve_provider_and_model, select_startup_model_with_fallback_and_auth_resolver,
};
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_tools::ToolRegistry;

use crate::alpha_runtime::{
    canonical_objective_behavior_mode, clear_objective_contract_wait_barrier,
    load_objective_contract, load_quorum_policy, objective_lifecycle_is_active,
    objective_now_unix_ms, objective_wait_remaining_seconds, objective_wait_target,
    summarize_objective_wait_barrier, ObjectiveContract, ObjectiveWaitTarget, QuorumPolicy,
};
use crate::auth::{
    login_nous_device_code, resolve_gemini_oauth_runtime_credentials,
    resolve_nous_runtime_credentials, resolve_qwen_runtime_credentials, save_nous_auth_state,
    NousDeviceCodeOptions, NousRuntimeCredentials, DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS, QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
};
use crate::cli::Cli;
use crate::commands::recover_queued_background_jobs;
use crate::model_switch::{provider_model_ids, MOA_DEFAULT_PRESET, MOA_PROVIDER};
use crate::runtime_tool_wiring::{wire_cron_scheduler_backend, wire_stdio_clarify_backend};
use crate::terminal_backend::build_terminal_backend;
use crate::tui::StreamHandle;

// Keep these includes in original item order. This is a layout-only split that
// leaves the app surface in the same module namespace while making each major
// subsystem independently reviewable.
include!("app/app_types.rs");
include!("app/app_impl.rs");
include!("app/runtime_overrides.rs");
include!("app/tests.rs");
include!("app/tool_runtime.rs");
