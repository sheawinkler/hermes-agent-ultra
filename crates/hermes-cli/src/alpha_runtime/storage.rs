fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

pub fn alpha_state_dir() -> PathBuf {
    hermes_config::hermes_home().join(ALPHA_STATE_DIR)
}

fn objective_contract_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_CONTRACT_FILE)
}

fn objective_profile_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_PROFILE_FILE)
}

fn objective_simulation_policy_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_SIMULATION_POLICY_FILE)
}

fn objective_ensemble_policy_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_ENSEMBLE_POLICY_FILE)
}

fn objective_learning_ledger_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_LEARNING_LEDGER_FILE)
}

fn objective_dag_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_DAG_FILE)
}

fn claim_verifier_policy_path() -> PathBuf {
    alpha_state_dir().join(CLAIM_VERIFIER_POLICY_FILE)
}

fn quorum_policy_path() -> PathBuf {
    alpha_state_dir().join(QUORUM_POLICY_FILE)
}

fn objective_eval_trend_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_EVAL_TREND_FILE)
}

fn subagent_registry_path() -> PathBuf {
    alpha_state_dir().join(SUBAGENT_REGISTRY_FILE)
}

fn contextlattice_policy_path() -> PathBuf {
    alpha_state_dir().join(CONTEXTLATTICE_POLICY_FILE)
}

fn loops_path() -> PathBuf {
    alpha_state_dir().join(LOOPS_FILE)
}

fn loop_queue_path() -> PathBuf {
    alpha_state_dir().join(LOOP_QUEUE_FILE)
}

fn loop_runtime_path() -> PathBuf {
    alpha_state_dir().join(LOOP_RUNTIME_FILE)
}

fn ensure_alpha_dir() -> Result<(), AgentError> {
    let dir = alpha_state_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| AgentError::Io(format!("failed to create {}: {}", dir.display(), e)))
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), AgentError> {
    let serialized = serde_json::to_string_pretty(value)
        .map_err(|e| AgentError::Config(format!("serialize {} failed: {}", path.display(), e)))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("failed to create {}: {}", parent.display(), e)))?;
    }
    std::fs::write(path, serialized)
        .map_err(|e| AgentError::Io(format!("write {} failed: {}", path.display(), e)))
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, AgentError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {} failed: {}", path.display(), e)))?;
    serde_json::from_str::<T>(&raw)
        .map_err(|e| AgentError::Config(format!("parse {} failed: {}", path.display(), e)))
}

