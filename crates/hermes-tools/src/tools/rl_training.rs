use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};
use hermes_rl::{
    BatchRunner, BatchRunnerConfig, RlEnvironment, RunManager, TrainingConfig, TrainingMetrics,
    TrainingStatus,
};

// ---------------------------------------------------------------------------
// Shared state across all RL tool handlers
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct RlState {
    inner: Arc<Mutex<RlStateInner>>,
}

struct RlStateInner {
    run_manager: RunManager,
    selected_environment: Option<String>,
    current_config: TrainingConfig,
}

impl RlState {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RlStateInner {
                run_manager: RunManager::new(data_dir),
                selected_environment: None,
                current_config: TrainingConfig::default(),
            })),
        }
    }
}

// ---------------------------------------------------------------------------
// 1. rl_list_environments
// ---------------------------------------------------------------------------

pub struct RlListEnvironmentsHandler;

#[async_trait]
impl ToolHandler for RlListEnvironmentsHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let envs = RlEnvironment::builtin_environments();
        Ok(json!({ "environments": envs }).to_string())
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "rl_list_environments",
            "List available RL environments (Tinker, Atropos, custom).",
            JsonSchema::object(IndexMap::new(), vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// 2. rl_select_environment
// ---------------------------------------------------------------------------

pub struct RlSelectEnvironmentHandler {
    pub state: RlState,
}

#[async_trait]
impl ToolHandler for RlSelectEnvironmentHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("environment")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'environment'".into()))?;

        let envs = RlEnvironment::builtin_environments();
        let found = envs.iter().any(|e| e.name == name);
        if !found {
            return Err(ToolError::InvalidParams(format!(
                "Unknown environment '{}'. Use rl_list_environments to see available options.",
                name
            )));
        }

        let mut inner = self.state.inner.lock().await;
        inner.selected_environment = Some(name.to_string());

        Ok(json!({
            "selected": name,
            "config_schema": envs.iter().find(|e| e.name == name).unwrap().config_schema,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "environment".into(),
            json!({"type": "string", "description": "Name of the RL environment to select"}),
        );
        tool_schema(
            "rl_select_environment",
            "Select and configure an RL environment for training.",
            JsonSchema::object(props, vec!["environment".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 3. rl_get_current_config
// ---------------------------------------------------------------------------

pub struct RlGetCurrentConfigHandler {
    pub state: RlState,
}

#[async_trait]
impl ToolHandler for RlGetCurrentConfigHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let inner = self.state.inner.lock().await;
        Ok(json!({
            "environment": inner.selected_environment,
            "config": inner.current_config,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "rl_get_current_config",
            "Show current RL training configuration.",
            JsonSchema::object(IndexMap::new(), vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// 4. rl_edit_config
// ---------------------------------------------------------------------------

pub struct RlEditConfigHandler {
    pub state: RlState,
}

#[async_trait]
impl ToolHandler for RlEditConfigHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let mut inner = self.state.inner.lock().await;
        let cfg = &mut inner.current_config;

        if let Some(v) = params.get("algo").and_then(|v| v.as_str()) {
            cfg.algo = v.to_string();
        }
        if let Some(v) = params.get("learning_rate").and_then(|v| v.as_f64()) {
            cfg.learning_rate = v;
        }
        if let Some(v) = params.get("batch_size").and_then(|v| v.as_u64()) {
            cfg.batch_size = v as usize;
        }
        if let Some(v) = params.get("max_steps").and_then(|v| v.as_u64()) {
            cfg.max_steps = v as usize;
        }
        if let Some(v) = params.get("reward_model").and_then(|v| v.as_str()) {
            cfg.reward_model = Some(v.to_string());
        }

        Ok(json!({ "updated_config": *cfg }).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("algo".into(), json!({"type": "string", "description": "Training algorithm (ppo, dpo, grpo)"}));
        props.insert("learning_rate".into(), json!({"type": "number"}));
        props.insert("batch_size".into(), json!({"type": "integer"}));
        props.insert("max_steps".into(), json!({"type": "integer"}));
        props.insert("reward_model".into(), json!({"type": "string"}));
        tool_schema(
            "rl_edit_config",
            "Modify training hyperparameters.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// 5. rl_start_training
// ---------------------------------------------------------------------------

pub struct RlStartTrainingHandler {
    pub state: RlState,
}

#[async_trait]
impl ToolHandler for RlStartTrainingHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let mut inner = self.state.inner.lock().await;
        let env = inner
            .selected_environment
            .clone()
            .unwrap_or_else(|| "tinker".to_string());
        let config = inner.current_config.clone();
        let run_id = inner.run_manager.create_run(&env, config);
        inner
            .run_manager
            .set_status(&run_id, TrainingStatus::Running);

        Ok(json!({
            "run_id": run_id,
            "environment": env,
            "status": "running",
            "note": "Training started in background. Use rl_check_status to monitor progress."
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "rl_start_training",
            "Start a training run with the current configuration.",
            JsonSchema::object(IndexMap::new(), vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// 6. rl_check_status
// ---------------------------------------------------------------------------

pub struct RlCheckStatusHandler {
    pub state: RlState,
}

#[async_trait]
impl ToolHandler for RlCheckStatusHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let run_id = params
            .get("run_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'run_id'".into()))?;

        let inner = self.state.inner.lock().await;
        let run = inner
            .run_manager
            .get_run(run_id)
            .ok_or_else(|| ToolError::InvalidParams(format!("Unknown run '{}'", run_id)))?;

        Ok(json!({
            "run_id": run.id,
            "status": run.status,
            "environment": run.environment,
            "metrics": run.metrics,
            "started_at": run.started_at.to_rfc3339(),
            "finished_at": run.finished_at.map(|t| t.to_rfc3339()),
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "run_id".into(),
            json!({"type": "string", "description": "ID of the training run to check"}),
        );
        tool_schema(
            "rl_check_status",
            "Check training progress (loss, rewards, steps).",
            JsonSchema::object(props, vec!["run_id".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 7. rl_stop_training
// ---------------------------------------------------------------------------

pub struct RlStopTrainingHandler {
    pub state: RlState,
}

#[async_trait]
impl ToolHandler for RlStopTrainingHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let run_id = params
            .get("run_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'run_id'".into()))?;

        let mut inner = self.state.inner.lock().await;
        if !inner.run_manager.set_status(run_id, TrainingStatus::Stopped) {
            return Err(ToolError::InvalidParams(format!(
                "Unknown run '{}'",
                run_id
            )));
        }

        Ok(json!({
            "run_id": run_id,
            "status": "stopped",
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "run_id".into(),
            json!({"type": "string", "description": "ID of the training run to stop"}),
        );
        tool_schema(
            "rl_stop_training",
            "Stop a running training job.",
            JsonSchema::object(props, vec!["run_id".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 8. rl_get_results
// ---------------------------------------------------------------------------

pub struct RlGetResultsHandler {
    pub state: RlState,
}

#[async_trait]
impl ToolHandler for RlGetResultsHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let run_id = params
            .get("run_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'run_id'".into()))?;

        let inner = self.state.inner.lock().await;
        let run = inner
            .run_manager
            .get_run(run_id)
            .ok_or_else(|| ToolError::InvalidParams(format!("Unknown run '{}'", run_id)))?;

        if run.status == TrainingStatus::Running || run.status == TrainingStatus::Pending {
            return Ok(json!({
                "run_id": run_id,
                "status": run.status,
                "note": "Training is still in progress. Check back later."
            })
            .to_string());
        }

        Ok(json!({
            "run_id": run.id,
            "status": run.status,
            "environment": run.environment,
            "config": run.config,
            "final_metrics": run.metrics,
            "started_at": run.started_at.to_rfc3339(),
            "finished_at": run.finished_at.map(|t| t.to_rfc3339()),
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "run_id".into(),
            json!({"type": "string", "description": "ID of the completed training run"}),
        );
        tool_schema(
            "rl_get_results",
            "Get results from a completed training run.",
            JsonSchema::object(props, vec!["run_id".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// 9. rl_list_runs
// ---------------------------------------------------------------------------

pub struct RlListRunsHandler {
    pub state: RlState,
}

#[async_trait]
impl ToolHandler for RlListRunsHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let inner = self.state.inner.lock().await;
        let runs: Vec<Value> = inner
            .run_manager
            .list_runs()
            .iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "environment": r.environment,
                    "status": r.status,
                    "algo": r.config.algo,
                    "started_at": r.started_at.to_rfc3339(),
                    "finished_at": r.finished_at.map(|t| t.to_rfc3339()),
                })
            })
            .collect();

        Ok(json!({ "runs": runs, "count": runs.len() }).to_string())
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "rl_list_runs",
            "List all training runs with metadata.",
            JsonSchema::object(IndexMap::new(), vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// 10. rl_test_inference
// ---------------------------------------------------------------------------

pub struct RlTestInferenceHandler {
    pub state: RlState,
}

#[async_trait]
impl ToolHandler for RlTestInferenceHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let prompt = params
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'prompt'".into()))?;

        let run_id = params.get("run_id").and_then(|v| v.as_str());

        let inner = self.state.inner.lock().await;
        let run_info = if let Some(rid) = run_id {
            inner.run_manager.get_run(rid).map(|r| {
                json!({
                    "run_id": r.id,
                    "algo": r.config.algo,
                    "status": r.status,
                })
            })
        } else {
            None
        };

        let runner = BatchRunner::new(BatchRunnerConfig {
            max_parallel_jobs: 1,
            max_turns: 1,
        });
        let stubs = runner.generate_stub(&[prompt.to_string()]);
        let response = stubs
            .first()
            .map(|t| t.response.clone())
            .unwrap_or_default();

        Ok(json!({
            "prompt": prompt,
            "response": response,
            "run_info": run_info,
            "note": "Inference is stub-only; no trained model checkpoint is loaded."
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "prompt".into(),
            json!({"type": "string", "description": "Prompt to test inference with"}),
        );
        props.insert(
            "run_id".into(),
            json!({"type": "string", "description": "Optional run ID to use trained checkpoint from"}),
        );
        tool_schema(
            "rl_test_inference",
            "Test the trained model on a prompt.",
            JsonSchema::object(props, vec!["prompt".into()]),
        )
    }
}
