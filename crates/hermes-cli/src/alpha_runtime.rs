use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Utc};
use hermes_core::AgentError;
use hermes_intelligence::model_metadata::{get_model_context_length, get_model_info};
use hermes_intelligence::models_dev::default_client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::model_switch::{curated_provider_slugs, provider_model_ids};

const ALPHA_STATE_DIR: &str = "alpha";
const OBJECTIVE_CONTRACT_FILE: &str = "objective_contract.json";
const OBJECTIVE_PROFILE_FILE: &str = "objective_profile.json";
const OBJECTIVE_SIMULATION_POLICY_FILE: &str = "objective_simulation_policy.json";
const OBJECTIVE_ENSEMBLE_POLICY_FILE: &str = "objective_ensemble_policy.json";
const OBJECTIVE_LEARNING_LEDGER_FILE: &str = "objective_learning_ledger.json";
const OBJECTIVE_DAG_FILE: &str = "objective_dag.json";
const CLAIM_VERIFIER_POLICY_FILE: &str = "claim_verifier_policy.json";
const QUORUM_POLICY_FILE: &str = "quorum_policy.json";
const OBJECTIVE_EVAL_TREND_FILE: &str = "objective_eval_trend.json";
const SUBAGENT_REGISTRY_FILE: &str = "subagents.json";
const CONTEXTLATTICE_POLICY_FILE: &str = "contextlattice_policy.json";
const LOOPS_FILE: &str = "loops.json";
const LOOP_QUEUE_FILE: &str = "loop_queue.jsonl";
const LOOP_RUNTIME_FILE: &str = "loop_runtime.json";

include!("alpha_runtime/types.rs");
include!("alpha_runtime/storage.rs");
include!("alpha_runtime/defaults.rs");
include!("alpha_runtime/bootstrap.rs");
include!("alpha_runtime/objective_contract.rs");
include!("alpha_runtime/policies.rs");
include!("alpha_runtime/runtime_status.rs");

include!("alpha_runtime/trading.rs");

#[cfg(test)]
mod tests;
