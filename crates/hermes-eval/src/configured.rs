//! Config-driven benchmark adapter and verifier.
//!
//! Supports loading benchmark sets from TOML or JSON, plus a production-usable
//! verifier path:
//! - heuristic expectations (`expected_contains`, `expected_regex`, `min_length`)
//! - optional LLM-as-judge pass (OpenAI-compatible endpoint)

use std::fs;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::adapter::{BenchmarkAdapter, BenchmarkMetadata, TaskSpec};
use crate::error::{EvalError, EvalResult};
use crate::verifier::{VerificationOutcome, Verifier};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSetFile {
    pub benchmark: BenchmarkMetadata,
    #[serde(default)]
    pub tasks: Vec<ConfiguredTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfiguredTask {
    pub task_id: String,
    #[serde(default)]
    pub category: Option<String>,
    pub instruction: String,
    #[serde(default)]
    pub context: Value,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub expected_contains: Vec<String>,
    #[serde(default)]
    pub expected_regex: Vec<String>,
    #[serde(default)]
    pub expected_any: Vec<String>,
    #[serde(default)]
    pub min_length: Option<usize>,
    #[serde(default)]
    pub judge_prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConfiguredBenchmarkAdapter {
    spec: BenchmarkSetFile,
}

impl ConfiguredBenchmarkAdapter {
    pub fn from_path(path: impl AsRef<Path>) -> EvalResult<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .map_err(|e| EvalError::DatasetLoad(format!("{}: {}", path.display(), e)))?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let spec = match ext.as_str() {
            "json" => serde_json::from_str::<BenchmarkSetFile>(&content)
                .map_err(|e| EvalError::DatasetLoad(format!("{}: {}", path.display(), e)))?,
            "toml" | "tml" => toml::from_str::<BenchmarkSetFile>(&content)
                .map_err(|e| EvalError::DatasetLoad(format!("{}: {}", path.display(), e)))?,
            _ => {
                return Err(EvalError::DatasetLoad(format!(
                    "unsupported benchmark file extension for {} (expected .toml or .json)",
                    path.display()
                )))
            }
        };
        if spec.tasks.is_empty() {
            return Err(EvalError::DatasetLoad(format!(
                "benchmark {} has no tasks",
                spec.benchmark.id
            )));
        }
        Ok(Self { spec })
    }
}

#[async_trait]
impl BenchmarkAdapter for ConfiguredBenchmarkAdapter {
    fn metadata(&self) -> BenchmarkMetadata {
        self.spec.benchmark.clone()
    }

    async fn load_tasks(&self) -> EvalResult<Vec<TaskSpec>> {
        let mut out = Vec::with_capacity(self.spec.tasks.len());
        for task in &self.spec.tasks {
            let timeout = Duration::from_secs(task.timeout_secs.unwrap_or(300).max(1));
            let merged_context = json!({
                "context": task.context,
                "expected_contains": task.expected_contains,
                "expected_regex": task.expected_regex,
                "expected_any": task.expected_any,
                "min_length": task.min_length,
                "judge_prompt": task.judge_prompt,
            });
            out.push(TaskSpec {
                task_id: task.task_id.clone(),
                category: task.category.clone(),
                instruction: task.instruction.clone(),
                context: merged_context,
                timeout,
            });
        }
        Ok(out)
    }

    fn verifier(&self) -> Box<dyn Verifier> {
        Box::new(ConfiguredVerifier::default())
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConfiguredVerifier;

#[async_trait]
impl Verifier for ConfiguredVerifier {
    async fn verify(
        &self,
        task: &TaskSpec,
        agent_final_state: &serde_json::Value,
    ) -> EvalResult<VerificationOutcome> {
        let text = extract_agent_text(agent_final_state);
        let heur = heuristic_verify(task, &text)?;

        if !llm_judge_enabled() {
            return Ok(heur);
        }
        let Some(prompt) = task
            .context
            .get("judge_prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(heur);
        };

        match llm_judge(task, &text, prompt).await {
            Ok(judged) => Ok(judged),
            Err(e) => Ok(VerificationOutcome {
                score: heur.score,
                passed: heur.passed,
                detail: Some(format!(
                    "{}; llm-judge-unavailable: {}",
                    heur.detail
                        .unwrap_or_else(|| "heuristic verdict".to_string()),
                    e
                )),
                metadata: json!({"judge": "fallback_heuristic"}),
            }),
        }
    }
}

fn extract_agent_text(agent_state: &Value) -> String {
    if let Some(s) = agent_state
        .get("last_assistant_preview")
        .and_then(|v| v.as_str())
    {
        return s.to_string();
    }
    if let Some(s) = agent_state.get("response").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(messages) = agent_state.get("messages").and_then(|v| v.as_array()) {
        if let Some(msg) = messages.iter().rev().find_map(|m| {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
            if role.eq_ignore_ascii_case("assistant") {
                m.get("content").and_then(|v| v.as_str())
            } else {
                None
            }
        }) {
            return msg.to_string();
        }
    }
    agent_state.to_string()
}

fn heuristic_verify(task: &TaskSpec, output_text: &str) -> EvalResult<VerificationOutcome> {
    let mut failures = Vec::new();
    let text = output_text;

    if let Some(min_len) = task.context.get("min_length").and_then(|v| v.as_u64()) {
        if text.chars().count() < min_len as usize {
            failures.push(format!("output shorter than min_length={min_len}"));
        }
    }

    if let Some(arr) = task
        .context
        .get("expected_contains")
        .and_then(|v| v.as_array())
    {
        for token in arr.iter().filter_map(|v| v.as_str()) {
            if !text
                .to_ascii_lowercase()
                .contains(&token.to_ascii_lowercase())
            {
                failures.push(format!("missing expected_contains token: {}", token));
            }
        }
    }

    if let Some(arr) = task
        .context
        .get("expected_regex")
        .and_then(|v| v.as_array())
    {
        for pattern in arr.iter().filter_map(|v| v.as_str()) {
            let re = Regex::new(pattern).map_err(|e| {
                EvalError::Verification(format!("invalid expected_regex `{pattern}`: {e}"))
            })?;
            if !re.is_match(text) {
                failures.push(format!("regex did not match: {}", pattern));
            }
        }
    }

    if let Some(arr) = task.context.get("expected_any").and_then(|v| v.as_array()) {
        let tokens = arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>();
        if !tokens.is_empty() {
            let matched = tokens.iter().any(|token| {
                text.to_ascii_lowercase()
                    .contains(&token.to_ascii_lowercase())
            });
            if !matched {
                failures.push(format!(
                    "none of expected_any tokens matched ({})",
                    tokens.join(", ")
                ));
            }
        }
    }

    if failures.is_empty() {
        Ok(VerificationOutcome {
            score: 1.0,
            passed: true,
            detail: Some("heuristic checks passed".to_string()),
            metadata: json!({"judge": "heuristic"}),
        })
    } else {
        Ok(VerificationOutcome {
            score: 0.0,
            passed: false,
            detail: Some(failures.join("; ")),
            metadata: json!({"judge": "heuristic", "failures": failures}),
        })
    }
}

fn llm_judge_enabled() -> bool {
    std::env::var("HERMES_EVAL_LLM_JUDGE")
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

async fn llm_judge(
    task: &TaskSpec,
    output_text: &str,
    judge_prompt: &str,
) -> EvalResult<VerificationOutcome> {
    let api_key = std::env::var("HERMES_EVAL_JUDGE_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .map_err(|_| {
            EvalError::Verification("missing HERMES_EVAL_JUDGE_API_KEY/OPENAI_API_KEY".to_string())
        })?;
    let base_url = std::env::var("HERMES_EVAL_JUDGE_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
    let model =
        std::env::var("HERMES_EVAL_JUDGE_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let body = json!({
        "model": model,
        "temperature": 0,
        "messages": [
            {
                "role": "system",
                "content": "You are an evaluation judge. Return strict JSON: {\"score\":0..1,\"passed\":bool,\"detail\":string}. No extra text."
            },
            {
                "role": "user",
                "content": format!(
                    "Task: {}\nJudge policy: {}\nOutput:\n{}",
                    task.instruction,
                    judge_prompt,
                    output_text
                )
            }
        ]
    });

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;

    if resp.status() != StatusCode::OK {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(EvalError::Verification(format!(
            "judge http {}: {}",
            status, text
        )));
    }

    let payload: Value = resp.json().await?;
    let content = payload
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("message"))
        .and_then(|v| v.get("content"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            EvalError::Verification("judge response missing choices[0].message.content".to_string())
        })?;
    let judged = parse_judge_json(content)?;
    Ok(judged)
}

fn parse_judge_json(content: &str) -> EvalResult<VerificationOutcome> {
    let parsed: Value = serde_json::from_str(content).map_err(|e| {
        EvalError::Verification(format!(
            "judge content is not valid JSON: {} ({})",
            content, e
        ))
    })?;
    let score = parsed
        .get("score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let passed = parsed
        .get("passed")
        .and_then(|v| v.as_bool())
        .unwrap_or(score >= 0.5);
    let detail = parsed
        .get("detail")
        .and_then(|v| v.as_str())
        .unwrap_or("llm judge")
        .to_string();
    Ok(VerificationOutcome {
        score,
        passed,
        detail: Some(detail),
        metadata: json!({"judge": "llm"}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_toml_benchmark_set() {
        let toml = r#"
[benchmark]
id = "demo-bench"
name = "Demo"
source = "local"
version = "1"

[[tasks]]
task_id = "t1"
instruction = "say hello"
expected_contains = ["hello"]
timeout_secs = 30
"#;
        let parsed: BenchmarkSetFile = toml::from_str(toml).expect("parse");
        assert_eq!(parsed.benchmark.id, "demo-bench");
        assert_eq!(parsed.tasks.len(), 1);
        assert_eq!(parsed.tasks[0].task_id, "t1");
    }

    #[tokio::test]
    async fn heuristic_verifier_passes_and_fails() {
        let task = TaskSpec {
            task_id: "x".into(),
            category: None,
            instruction: "demo".into(),
            timeout: Duration::from_secs(10),
            context: json!({
                "expected_contains": ["hello", "world"],
                "expected_regex": ["h.llo"],
                "min_length": 5
            }),
        };
        let state_ok = json!({"last_assistant_preview": "hello world"});
        let state_bad = json!({"last_assistant_preview": "hola"});
        let v = ConfiguredVerifier;
        let ok = v.verify(&task, &state_ok).await.unwrap();
        let bad = v.verify(&task, &state_bad).await.unwrap();
        assert!(ok.passed);
        assert!(!bad.passed);
    }
}
