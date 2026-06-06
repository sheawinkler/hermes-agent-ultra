use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::future::join_all;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

const DEFAULT_MOA_BASE_URL: &str = "https://inference-api.nousresearch.com/v1";
const DEFAULT_REFERENCE_MODELS: &[&str] = &[
    "claude-opus-4-20250514",
    "gemini-2.5-pro",
    "o4-mini",
    "deepseek-r1",
];
const DEFAULT_AGGREGATOR_MODEL: &str = "claude-opus-4-20250514";
const DEFAULT_REFERENCE_TEMPERATURE: f64 = 0.6;
const DEFAULT_AGGREGATOR_TEMPERATURE: f64 = 0.4;
const DEFAULT_MIN_SUCCESSFUL_REFERENCES: usize = 1;
const DEFAULT_MAX_RETRIES: usize = 3;
const DEFAULT_REFERENCE_MAX_TOKENS: u32 = 128_000;
const DEFAULT_AGGREGATOR_MAX_TOKENS: u32 = 16_000;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 120;

const AGGREGATOR_SYSTEM_PROMPT: &str = "You have been provided with a set of responses from various open-source models to the latest user query. Your task is to synthesize these responses into a single, high-quality response. It is crucial to critically evaluate the information provided in these responses, recognizing that some of it may be biased or incorrect. Your response should not simply replicate the given answers but should offer a refined, accurate, and comprehensive reply to the instruction. Ensure your response is well-structured, coherent, and adheres to the highest standards of accuracy and reliability.\n\nResponses from models:";

pub struct MixtureOfAgentsHandler;

#[derive(Clone, Debug)]
struct MoaRuntimeConfig {
    api_key: Option<String>,
    base_url: String,
    reference_models: Vec<String>,
    aggregator_model: String,
    reference_temperature: f64,
    aggregator_temperature: f64,
    min_successful_references: usize,
    max_retries: usize,
    reference_max_tokens: u32,
    aggregator_max_tokens: u32,
    request_timeout: Duration,
}

#[derive(Clone, Debug)]
struct ReferenceOutcome {
    model: String,
    content: Option<String>,
    error: Option<String>,
}

impl ReferenceOutcome {
    fn success(model: String, content: String) -> Self {
        Self {
            model,
            content: Some(content),
            error: None,
        }
    }

    fn failure(model: String, error: String) -> Self {
        Self {
            model,
            content: None,
            error: Some(error),
        }
    }

    fn is_success(&self) -> bool {
        self.content.is_some()
    }
}

#[async_trait]
impl ToolHandler for MixtureOfAgentsHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let started = Instant::now();
        let prompt = required_prompt(&params)?;
        let config = MoaRuntimeConfig::from_params(&params)?;

        if config
            .api_key
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            return moa_failure_json(
                &config,
                started,
                "NOUS_API_KEY, HERMES_MOA_API_KEY, or MOA_TOOLS_API_KEY environment variable not set",
                0,
                Vec::new(),
            );
        }

        let client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(|e| ToolError::ExecutionFailed(format!("MoA HTTP client: {e}")))?;

        let reference_futures = config.reference_models.iter().cloned().map(|model| {
            run_reference_model_safe(client.clone(), config.clone(), model, prompt.clone())
        });
        let model_results = join_all(reference_futures).await;

        let successful_responses: Vec<String> = model_results
            .iter()
            .filter_map(|outcome| outcome.content.clone())
            .collect();
        let failed_models: Vec<Value> = model_results
            .iter()
            .filter(|outcome| !outcome.is_success())
            .map(|outcome| {
                json!({
                    "model": outcome.model,
                    "error": outcome.error.clone().unwrap_or_else(|| "unknown error".to_string()),
                })
            })
            .collect();

        if successful_responses.len() < config.min_successful_references {
            return moa_failure_json(
                &config,
                started,
                format!(
                    "Insufficient successful reference models ({}/{}). Need at least {} successful responses.",
                    successful_responses.len(),
                    config.reference_models.len(),
                    config.min_successful_references
                ),
                successful_responses.len(),
                failed_models,
            );
        }

        let aggregator_system_prompt = construct_aggregator_prompt(&successful_responses);
        match chat_completion(
            &client,
            &config,
            &config.aggregator_model,
            vec![
                json!({"role": "system", "content": aggregator_system_prompt}),
                json!({"role": "user", "content": prompt}),
            ],
            config.aggregator_temperature,
            config.aggregator_max_tokens,
        )
        .await
        {
            Ok(final_response) => moa_success_json(
                &config,
                started,
                final_response,
                successful_responses.len(),
                failed_models,
            ),
            Err(error) => moa_failure_json(
                &config,
                started,
                format!("Aggregator model failed: {error}"),
                successful_responses.len(),
                failed_models,
            ),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "prompt".into(),
            json!({"type":"string","description":"Complex query or problem to solve with a two-layer mixture-of-agents workflow."}),
        );
        props.insert(
            "reference_models".into(),
            json!({"type":"array","items":{"type":"string"},"description":"Optional OpenAI-compatible reference models for the parallel first layer."}),
        );
        props.insert(
            "agents".into(),
            json!({"type":"array","items":{"type":"string"},"description":"Alias for reference_models, retained for older Hermes tool calls."}),
        );
        props.insert(
            "aggregator_model".into(),
            json!({"type":"string","description":"Optional OpenAI-compatible model used to synthesize reference responses."}),
        );
        props.insert(
            "base_url".into(),
            json!({"type":"string","description":"Optional OpenAI-compatible base URL. Defaults to Nous inference API or HERMES_MOA_BASE_URL."}),
        );
        props.insert(
            "min_successful_references".into(),
            json!({"type":"integer","minimum":1,"description":"Minimum successful reference responses required before aggregation."}),
        );
        props.insert(
            "max_retries".into(),
            json!({"type":"integer","minimum":1,"description":"Retry attempts per reference model."}),
        );
        props.insert(
            "reference_temperature".into(),
            json!({"type":"number","description":"Sampling temperature for reference models. Defaults to 0.6."}),
        );
        props.insert(
            "aggregator_temperature".into(),
            json!({"type":"number","description":"Sampling temperature for the aggregator model. Defaults to 0.4."}),
        );
        tool_schema(
            "mixture_of_agents",
            "Run a Rust-native mixture-of-agents reasoning workflow through OpenAI-compatible chat completions.",
            JsonSchema::object(props, vec!["prompt".into()]),
        )
    }
}

impl MoaRuntimeConfig {
    fn from_params(params: &Value) -> Result<Self, ToolError> {
        let reference_models = model_list_from_params(params, "reference_models")
            .or_else(|| model_list_from_params(params, "agents"))
            .or_else(|| env_csv(&["HERMES_MOA_REFERENCE_MODELS", "MOA_REFERENCE_MODELS"]))
            .unwrap_or_else(|| {
                DEFAULT_REFERENCE_MODELS
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            });

        if reference_models.is_empty() {
            return Err(ToolError::InvalidParams(
                "reference_models must contain at least one model".into(),
            ));
        }

        let aggregator_model = string_param(params, "aggregator_model")
            .or_else(|| env_nonempty(&["HERMES_MOA_AGGREGATOR_MODEL", "MOA_AGGREGATOR_MODEL"]))
            .unwrap_or_else(|| DEFAULT_AGGREGATOR_MODEL.to_string());
        if aggregator_model.trim().is_empty() {
            return Err(ToolError::InvalidParams(
                "aggregator_model must not be empty".into(),
            ));
        }

        let min_successful_references = usize_param(params, "min_successful_references")
            .or_else(|| {
                env_usize(&[
                    "HERMES_MOA_MIN_SUCCESSFUL_REFERENCES",
                    "MOA_MIN_SUCCESSFUL_REFERENCES",
                ])
            })
            .unwrap_or(DEFAULT_MIN_SUCCESSFUL_REFERENCES);
        if min_successful_references == 0 {
            return Err(ToolError::InvalidParams(
                "min_successful_references must be at least 1".into(),
            ));
        }
        if min_successful_references > reference_models.len() {
            return Err(ToolError::InvalidParams(format!(
                "min_successful_references ({min_successful_references}) exceeds reference model count ({})",
                reference_models.len()
            )));
        }

        let max_retries = usize_param(params, "max_retries")
            .or_else(|| env_usize(&["HERMES_MOA_MAX_RETRIES", "MOA_MAX_RETRIES"]))
            .unwrap_or(DEFAULT_MAX_RETRIES);
        if max_retries == 0 {
            return Err(ToolError::InvalidParams(
                "max_retries must be at least 1".into(),
            ));
        }

        Ok(Self {
            api_key: env_nonempty(&["HERMES_MOA_API_KEY", "MOA_TOOLS_API_KEY", "NOUS_API_KEY"]),
            base_url: string_param(params, "base_url")
                .or_else(|| {
                    env_nonempty(&["HERMES_MOA_BASE_URL", "MOA_TOOLS_BASE_URL", "NOUS_BASE_URL"])
                })
                .unwrap_or_else(|| DEFAULT_MOA_BASE_URL.to_string())
                .trim()
                .trim_end_matches('/')
                .to_string(),
            reference_models,
            aggregator_model: aggregator_model.trim().to_string(),
            reference_temperature: f64_param(params, "reference_temperature")
                .or_else(|| {
                    env_f64(&[
                        "HERMES_MOA_REFERENCE_TEMPERATURE",
                        "MOA_REFERENCE_TEMPERATURE",
                    ])
                })
                .unwrap_or(DEFAULT_REFERENCE_TEMPERATURE),
            aggregator_temperature: f64_param(params, "aggregator_temperature")
                .or_else(|| {
                    env_f64(&[
                        "HERMES_MOA_AGGREGATOR_TEMPERATURE",
                        "MOA_AGGREGATOR_TEMPERATURE",
                    ])
                })
                .unwrap_or(DEFAULT_AGGREGATOR_TEMPERATURE),
            min_successful_references,
            max_retries,
            reference_max_tokens: u32_param(params, "reference_max_tokens")
                .or_else(|| {
                    env_u32(&[
                        "HERMES_MOA_REFERENCE_MAX_TOKENS",
                        "MOA_REFERENCE_MAX_TOKENS",
                    ])
                })
                .unwrap_or(DEFAULT_REFERENCE_MAX_TOKENS),
            aggregator_max_tokens: u32_param(params, "aggregator_max_tokens")
                .or_else(|| {
                    env_u32(&[
                        "HERMES_MOA_AGGREGATOR_MAX_TOKENS",
                        "MOA_AGGREGATOR_MAX_TOKENS",
                    ])
                })
                .unwrap_or(DEFAULT_AGGREGATOR_MAX_TOKENS),
            request_timeout: Duration::from_secs(
                u64_param(params, "timeout_secs")
                    .or_else(|| env_u64(&["HERMES_MOA_TIMEOUT_SECS", "MOA_TIMEOUT_SECS"]))
                    .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS),
            ),
        })
    }
}

async fn run_reference_model_safe(
    client: reqwest::Client,
    config: MoaRuntimeConfig,
    model: String,
    prompt: String,
) -> ReferenceOutcome {
    let mut last_error = String::new();
    for attempt in 0..config.max_retries {
        match chat_completion(
            &client,
            &config,
            &model,
            vec![json!({"role": "user", "content": prompt})],
            config.reference_temperature,
            config.reference_max_tokens,
        )
        .await
        {
            Ok(content) => return ReferenceOutcome::success(model, content),
            Err(error) => {
                last_error = error;
                if attempt + 1 < config.max_retries {
                    let backoff_secs = 2_u64.pow(attempt.min(4) as u32);
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                }
            }
        }
    }
    ReferenceOutcome::failure(model, last_error)
}

async fn chat_completion(
    client: &reqwest::Client,
    config: &MoaRuntimeConfig,
    model: &str,
    messages: Vec<Value>,
    temperature: f64,
    max_tokens: u32,
) -> Result<String, String> {
    let url = format!("{}/chat/completions", config.base_url);
    let api_key = config.api_key.as_deref().unwrap_or_default();
    let body = json!({
        "model": model,
        "messages": messages,
        "temperature": temperature,
        "max_tokens": max_tokens,
    });

    let response = client
        .post(url)
        .bearer_auth(api_key.trim())
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| format!("failed to read response: {e}"))?;
    if !status.is_success() {
        return Err(format!("API error ({status}): {text}"));
    }
    let payload: Value = serde_json::from_str(&text)
        .map_err(|e| format!("failed to parse chat completion response: {e}"))?;
    chat_response_content(&payload)
        .ok_or_else(|| "chat completion response missing message.content".to_string())
}

fn chat_response_content(payload: &Value) -> Option<String> {
    let content = payload
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?;
    match content {
        Value::String(text) => nonempty(text),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part
                    .as_str()
                    .or_else(|| part.get("text").and_then(Value::as_str))
                {
                    out.push_str(text);
                }
            }
            nonempty(&out)
        }
        _ => None,
    }
}

fn construct_aggregator_prompt(responses: &[String]) -> String {
    let numbered = responses
        .iter()
        .enumerate()
        .map(|(idx, response)| format!("{}. {}", idx + 1, response))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{AGGREGATOR_SYSTEM_PROMPT}\n\n{numbered}")
}

fn required_prompt(params: &Value) -> Result<String, ToolError> {
    string_param(params, "prompt")
        .or_else(|| string_param(params, "user_prompt"))
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| ToolError::InvalidParams("Missing 'prompt'".into()))
}

fn moa_success_json(
    config: &MoaRuntimeConfig,
    started: Instant,
    response: String,
    reference_responses_count: usize,
    failed_models: Vec<Value>,
) -> Result<String, ToolError> {
    serde_json::to_string(&json!({
        "success": true,
        "status": "completed",
        "strategy": "mixture_of_agents",
        "response": response,
        "models_used": {
            "reference_models": config.reference_models,
            "aggregator_model": config.aggregator_model,
        },
        "reference_responses_count": reference_responses_count,
        "failed_models_count": failed_models.len(),
        "failed_models": failed_models,
        "processing_time": elapsed_seconds(started),
        "processing_time_seconds": elapsed_seconds(started),
    }))
    .map_err(|e| ToolError::ExecutionFailed(format!("serialize MoA response: {e}")))
}

fn moa_failure_json(
    config: &MoaRuntimeConfig,
    started: Instant,
    error: impl Into<String>,
    reference_responses_count: usize,
    failed_models: Vec<Value>,
) -> Result<String, ToolError> {
    serde_json::to_string(&json!({
        "success": false,
        "status": "failed",
        "strategy": "mixture_of_agents",
        "response": "MoA processing failed. Please try again or use a single model for this query.",
        "models_used": {
            "reference_models": config.reference_models,
            "aggregator_model": config.aggregator_model,
        },
        "reference_responses_count": reference_responses_count,
        "failed_models_count": failed_models.len(),
        "failed_models": failed_models,
        "processing_time": elapsed_seconds(started),
        "processing_time_seconds": elapsed_seconds(started),
        "error": error.into(),
    }))
    .map_err(|e| ToolError::ExecutionFailed(format!("serialize MoA failure: {e}")))
}

fn elapsed_seconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64()
}

fn string_param(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .and_then(nonempty)
}

fn model_list_from_params(params: &Value, key: &str) -> Option<Vec<String>> {
    let value = params.get(key)?;
    match value {
        Value::Array(items) => {
            let out = items
                .iter()
                .filter_map(Value::as_str)
                .filter_map(nonempty)
                .collect::<Vec<_>>();
            Some(out)
        }
        Value::String(raw) => Some(split_csv(raw)),
        _ => None,
    }
}

fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',').filter_map(nonempty).collect()
}

fn nonempty(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn f64_param(params: &Value, key: &str) -> Option<f64> {
    params.get(key).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    })
}

fn usize_param(params: &Value, key: &str) -> Option<usize> {
    params.get(key).and_then(|v| {
        v.as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<usize>().ok()))
    })
}

fn u32_param(params: &Value, key: &str) -> Option<u32> {
    params.get(key).and_then(|v| {
        v.as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u32>().ok()))
    })
}

fn u64_param(params: &Value, key: &str) -> Option<u64> {
    params.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
    })
}

fn env_nonempty(keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| std::env::var(key).ok().and_then(|v| nonempty(&v)))
}

fn env_csv(keys: &[&str]) -> Option<Vec<String>> {
    env_nonempty(keys)
        .map(|v| split_csv(&v))
        .filter(|v| !v.is_empty())
}

fn env_f64(keys: &[&str]) -> Option<f64> {
    env_nonempty(keys).and_then(|v| v.parse::<f64>().ok())
}

fn env_usize(keys: &[&str]) -> Option<usize> {
    env_nonempty(keys).and_then(|v| v.parse::<usize>().ok())
}

fn env_u32(keys: &[&str]) -> Option<u32> {
    env_nonempty(keys).and_then(|v| v.parse::<u32>().ok())
}

fn env_u64(keys: &[&str]) -> Option<u64> {
    env_nonempty(keys).and_then(|v| v.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old }
        }

        fn remove(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[tokio::test]
    async fn mixture_of_agents_schema_requires_prompt() {
        let handler = MixtureOfAgentsHandler;
        let schema = handler.schema();
        assert_eq!(schema.name, "mixture_of_agents");
        let required = schema
            .parameters
            .required
            .as_ref()
            .expect("required fields");
        assert!(required.contains(&"prompt".to_string()));
        let properties = schema.parameters.properties.as_ref().expect("properties");
        assert!(properties.contains_key("reference_models"));
        assert!(properties.contains_key("aggregator_model"));
    }

    #[tokio::test]
    async fn mixture_of_agents_rejects_missing_prompt() {
        let handler = MixtureOfAgentsHandler;
        let err = handler.execute(json!({"agents": ["planner"]})).await;
        assert!(matches!(err, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn mixture_of_agents_fails_cleanly_without_api_key() {
        let _env_lock = ENV_LOCK.lock().await;
        let _hermes = EnvGuard::remove("HERMES_MOA_API_KEY");
        let _moa = EnvGuard::remove("MOA_TOOLS_API_KEY");
        let _nous = EnvGuard::remove("NOUS_API_KEY");
        let handler = MixtureOfAgentsHandler;
        let output: Value = serde_json::from_str(
            &handler
                .execute(json!({"prompt": "compare plans"}))
                .await
                .expect("execute"),
        )
        .expect("json");

        assert_eq!(output["success"], false);
        assert_eq!(output["status"], "failed");
        assert!(output["error"].as_str().unwrap().contains("NOUS_API_KEY"));
    }

    #[tokio::test]
    async fn mixture_of_agents_executes_two_layer_openai_compatible_flow() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env_lock = ENV_LOCK.lock().await;
        let _key = EnvGuard::set("HERMES_MOA_API_KEY", "test-key");
        let _legacy = EnvGuard::remove("NOUS_API_KEY");
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_partial_json(json!({"model": "ref-a"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "ref-a answer"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({"model": "ref-b"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "ref-b answer"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({"model": "agg"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "aggregated answer"}}]
            })))
            .mount(&server)
            .await;

        let handler = MixtureOfAgentsHandler;
        let output: Value = serde_json::from_str(
            &handler
                .execute(json!({
                    "prompt": "compare plans",
                    "reference_models": ["ref-a", "ref-b"],
                    "aggregator_model": "agg",
                    "base_url": server.uri(),
                    "max_retries": 1,
                    "timeout_secs": 5
                }))
                .await
                .expect("execute"),
        )
        .expect("json");

        assert_eq!(output["success"], true);
        assert_eq!(output["status"], "completed");
        assert_eq!(output["strategy"], "mixture_of_agents");
        assert_eq!(output["response"], "aggregated answer");
        assert_eq!(output["reference_responses_count"], 2);
        assert_eq!(output["failed_models_count"], 0);

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 3);
        let aggregator_request = requests
            .iter()
            .find(|request| request.body_json::<Value>().unwrap()["model"] == "agg")
            .expect("aggregator request");
        let body = aggregator_request.body_json::<Value>().expect("body");
        let system_prompt = body["messages"][0]["content"].as_str().unwrap();
        assert!(system_prompt.contains("ref-a answer"));
        assert!(system_prompt.contains("ref-b answer"));
        assert_eq!(body["temperature"], DEFAULT_AGGREGATOR_TEMPERATURE);
    }

    #[tokio::test]
    async fn mixture_of_agents_tolerates_failed_reference_when_threshold_is_met() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env_lock = ENV_LOCK.lock().await;
        let _key = EnvGuard::set("HERMES_MOA_API_KEY", "test-key");
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({"model": "ref-ok"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "usable reference"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({"model": "ref-fail"})))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({"model": "agg"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "still aggregated"}}]
            })))
            .mount(&server)
            .await;

        let handler = MixtureOfAgentsHandler;
        let output: Value = serde_json::from_str(
            &handler
                .execute(json!({
                    "prompt": "hard problem",
                    "reference_models": ["ref-ok", "ref-fail"],
                    "aggregator_model": "agg",
                    "base_url": server.uri(),
                    "min_successful_references": 1,
                    "max_retries": 1,
                    "timeout_secs": 5
                }))
                .await
                .expect("execute"),
        )
        .expect("json");

        assert_eq!(output["success"], true);
        assert_eq!(output["response"], "still aggregated");
        assert_eq!(output["reference_responses_count"], 1);
        assert_eq!(output["failed_models_count"], 1);
        assert_eq!(output["failed_models"][0]["model"], "ref-fail");
    }
}
