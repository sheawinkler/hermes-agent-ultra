//! Slash command handler (Requirement 9.2).
//!
//! Defines and dispatches all supported `/` commands in the interactive
//! REPL, and provides auto-completion suggestions.

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use hermes_agent::plugins::PluginManifest;
use hermes_agent::{MemoryProviderPlugin, SessionPersistence};
use hermes_app_runtime::{
    apply_cli_chat_runtime_env, query_mode_model_not_found,
    query_mode_remediation_target_from_catalog, query_mode_tools_enabled,
    rank_catalog_model_candidates, resolve_catalog_model_candidate,
    resolve_cli_chat_provider_model_with, run_noninteractive_query, split_provider_model,
    QueryModelRemediation, QUERY_DISABLE_TOOLS_ENV_KEY,
};
use hermes_core::auth_gate::{
    load_oauth_runtime_gate_manifest_from_path,
    oauth_runtime_gate_for_provider as shared_oauth_runtime_gate_for_provider,
    oauth_runtime_gate_manifest_default, OAuthRuntimeGateManifest,
};
use hermes_core::subprocess::CommandNoWindowExt;
use hermes_core::AgentError;
use hermes_cron::{
    BlueprintCommandAction, CronJob, DeliverConfig, DeliverTarget, SuggestionJobSpec,
};
use hermes_intelligence::model_metadata::{get_model_context_length, get_model_info};
use hermes_intelligence::models_dev::default_client;
use hermes_intelligence::{build_swarm_execution_plan, swarm_runtime_status, SwarmExecutionMode};
use hermes_tools::skill_commands::{
    build_skill_reload_system_note, installed_skill_slash_command_snapshot,
    render_skill_slash_command_snapshot, resolve_installed_skill_slash_command,
    SkillCommandResolverConfig, SkillSlashInvocation,
};
use hermes_tools::ToolPolicyEngine;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::alpha_runtime::{
    append_counterfactual, append_objective_learning_entry, build_objective_dag_from_contract,
    canonical_objective_behavior_mode, canonical_objective_lifecycle_status,
    clear_objective_contract, clear_objective_contract_wait_barrier, clear_objective_dag,
    clear_objective_learning_ledger, enqueue_loop_event, ensure_alpha_runtime_bootstrap,
    ensure_trading_runtime_bootstrap, load_alpha_loops, load_claim_verifier_policy,
    load_contextlattice_policy, load_last_trading_alpha_report, load_objective_contract,
    load_objective_dag, load_objective_ensemble_policy, load_objective_eval_trend,
    load_objective_learning_ledger, load_objective_profile, load_objective_simulation_policy,
    load_quorum_policy, objective_lifecycle_is_active, objective_profile_specialized_for,
    recover_orphan_loop_events, refresh_trading_alpha_report, render_mission_board,
    render_trading_alpha_board, replay_loop_queue, reset_objective_profile_generalized,
    set_claim_verifier_enabled, set_contextlattice_policy_mode,
    set_objective_contract_behavior_mode, set_objective_contract_lifecycle_status,
    set_objective_contract_wait_pid, set_objective_contract_wait_seconds,
    set_objective_contract_wait_session, set_objective_ensemble_mode, set_objective_profile,
    set_objective_simulation_mode, set_quorum_policy, summarize_objective_contract,
    summarize_objective_wait_barrier, upsert_objective_contract, utility_terms_from_contract,
    ObjectiveLearningLedgerEntry,
};
use crate::app::{build_runtime_cron_scheduler, App, PetDock, PetSettings};
use crate::kanban::{
    add_attachment_to_task, add_task, archive_done, build_worker_context, claim_task,
    create_or_select_board, ensure_board, find_task_mut, lane_counts, load_store,
    maybe_checkpoint_to_contextlattice, move_task, remove_attachment_from_task, save_store,
    set_blocked, KanbanActionInput, KanbanBoard, KanbanLane, NewKanbanTaskInput,
};
use crate::mcp_config::{load_mcp_config, load_mcp_config_if_exists, McpTransportKind};
use crate::model_switch::{
    cached_provider_catalog_status, clear_provider_catalog_cache, curated_provider_slugs,
    format_stale_auxiliary_warning, normalize_provider_model, provider_catalog_entries_for_config,
    provider_model_ids, provider_model_ids_for_config, provider_slug_from_provider_model,
    provider_slugs_for_config,
};
use crate::pairing_store::{PairingStatus, PairingStore};
use crate::providers::canonical_provider_id;
use crate::skin_engine::{canonical_skin_name, BUILTIN_SKINS};
use hermes_config::{find_node_executable, set_user_config_value, GatewayConfig};

// The command surface is intentionally split by subsystem. These files are
// included in the original item order so this refactor changes layout only;
// follow-up passes can promote stable shards into stricter Rust submodules.
include!("command_catalog.rs");
include!("command_dispatch_model.rs");
include!("command_ops_eval.rs");
include!("command_background_browser.rs");
include!("command_reasoning_session.rs");
include!("command_kanban_objective.rs");
include!("command_agentic_workflows.rs");
include!("command_cli_chat_skills.rs");
include!("command_cli_plugins_memory_mcp.rs");
include!("command_cli_sessions_acp.rs");
include!("command_tests.rs");
