use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};
use hermes_rl::{BatchRunner, BatchRunnerConfig};

pub struct RlTrainingHandler;

#[async_trait]
impl ToolHandler for RlTrainingHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let dataset = params.get("dataset").and_then(|v| v.as_str()).unwrap_or("");
        if dataset.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'dataset'".into()));
        }

        let prompts: Vec<String> = if tokio::fs::try_exists(dataset)
            .await
            .unwrap_or(false)
        {
            let raw = tokio::fs::read_to_string(dataset)
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("Cannot read dataset file: {e}")))?;
            raw.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        } else {
            vec![dataset.to_string()]
        };

        if prompts.is_empty() {
            return Err(ToolError::InvalidParams(
                "Dataset path is empty or file has no non-empty lines".into(),
            ));
        }

        let algo = params
            .get("algo")
            .and_then(|v| v.as_str())
            .unwrap_or("ppo");

        let max_turns = params
            .get("max_turns")
            .and_then(|v| v.as_u64())
            .unwrap_or(32) as usize;

        let runner = BatchRunner::new(BatchRunnerConfig {
            max_parallel_jobs: 4,
            max_turns,
        });

        let stubs = runner.generate_stub(&prompts);
        Ok(json!({
            "status": "stub_batch_generated",
            "mode": "stub_only",
            "note": "No distributed trainer or real LLM rollout is wired here; trajectories are synthetic for pipeline/tests.",
            "algo": algo,
            "trajectory_count": stubs.len(),
            "trajectories": stubs,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("dataset".into(), json!({"type":"string","description":"Prompt text, or path to a UTF-8 file with one prompt per line"}));
        props.insert("algo".into(), json!({"type":"string","default":"ppo"}));
        props.insert("max_turns".into(), json!({"type":"integer","default":32}));
        tool_schema(
            "rl_training",
            "Synthetic RL batch trajectories from prompts (file or one inline string) via hermes-rl BatchRunner::generate_stub — not production distributed training.",
            JsonSchema::object(props, vec!["dataset".into()]),
        )
    }
}
