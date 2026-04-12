//! Smart model router — selects the best model for a given prompt and requirements.
//!
//! Requirement 16.1

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ModelCapability
// ---------------------------------------------------------------------------

/// Capabilities that a model may support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    Chat,
    Vision,
    Code,
    FunctionCalling,
    Streaming,
    Reasoning,
    Embedding,
}

// ---------------------------------------------------------------------------
// ModelInfo
// ---------------------------------------------------------------------------

/// Metadata about a registered model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub provider: String,
    pub context_window: usize,
    pub cost_per_input_token: f64,
    pub cost_per_output_token: f64,
    pub capabilities: Vec<ModelCapability>,
}

// ---------------------------------------------------------------------------
// ModelRequirements
// ---------------------------------------------------------------------------

/// Requirements that the router uses to select a model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelRequirements {
    /// Required capabilities (all must be present).
    pub capabilities: Vec<ModelCapability>,
    /// Maximum context window size the model must support.
    pub max_context: Option<usize>,
    /// Maximum estimated cost per request (in USD).
    pub max_cost: Option<f64>,
    /// Prefer faster / lower-latency models.
    pub prefer_fast: bool,
}

// ---------------------------------------------------------------------------
// RouterError
// ---------------------------------------------------------------------------

/// Errors that can occur during model routing.
#[derive(Debug, Clone, thiserror::Error)]
pub enum RouterError {
    #[error("no models registered")]
    NoModelsRegistered,

    #[error("no model matches requirements")]
    NoModelMatched,

    #[error("model not found: {0}")]
    ModelNotFound(String),
}

// ---------------------------------------------------------------------------
// SmartModelRouter
// ---------------------------------------------------------------------------

/// Router that selects the best model for a prompt based on requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartModelRouter {
    pub models: HashMap<String, ModelInfo>,
}

impl SmartModelRouter {
    /// Create an empty router.
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
        }
    }

    /// Register a model.
    pub fn register(&mut self, info: ModelInfo) {
        self.models.insert(info.name.clone(), info);
    }

    /// Select the best model for the given prompt and requirements.
    ///
    /// Scoring strategy:
    /// 1. Filter out models that don't match **all** required capabilities.
    /// 2. Filter out models whose context window is smaller than `max_context`.
    /// 3. Estimate cost for the prompt and filter out models that exceed `max_cost`.
    /// 4. Among remaining candidates, pick the one with the lowest cost
    ///    (or the fastest, when `prefer_fast` is set).
    pub fn route(
        &self,
        prompt: &str,
        requirements: &ModelRequirements,
    ) -> Result<String, RouterError> {
        if self.models.is_empty() {
            return Err(RouterError::NoModelsRegistered);
        }

        let prompt_tokens = estimate_tokens(prompt);

        // 1. Filter by capabilities
        let candidates: Vec<&ModelInfo> = self
            .models
            .values()
            .filter(|m| {
                requirements
                    .capabilities
                    .iter()
                    .all(|cap| m.capabilities.contains(cap))
            })
            .collect();

        if candidates.is_empty() {
            return Err(RouterError::NoModelMatched);
        }

        // 2. Filter by context window
        let candidates: Vec<&ModelInfo> = candidates
            .into_iter()
            .filter(|m| {
                requirements
                    .max_context
                    .map_or(true, |mc| m.context_window >= mc)
            })
            .collect();

        if candidates.is_empty() {
            return Err(RouterError::NoModelMatched);
        }

        // 3. Filter by max cost
        let candidates: Vec<&ModelInfo> = candidates
            .into_iter()
            .filter(|m| {
                let estimated = m.cost_per_input_token * prompt_tokens as f64;
                requirements.max_cost.map_or(true, |mc| estimated <= mc)
            })
            .collect();

        if candidates.is_empty() {
            return Err(RouterError::NoModelMatched);
        }

        // 4. Rank — lowest cost wins; if prefer_fast, break ties by provider
        //    heuristics (prefer "gpt" over "claude" as a proxy for speed).
        let best = if requirements.prefer_fast {
            candidates.into_iter().min_by(|a, b| {
                let cost_a = a.cost_per_input_token * prompt_tokens as f64;
                let cost_b = b.cost_per_input_token * prompt_tokens as f64;
                cost_a
                    .partial_cmp(&cost_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(fastness_score(a).cmp(&fastness_score(b)))
            })
        } else {
            candidates.into_iter().min_by(|a, b| {
                let cost_a = a.cost_per_input_token * prompt_tokens as f64;
                let cost_b = b.cost_per_input_token * prompt_tokens as f64;
                cost_a
                    .partial_cmp(&cost_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        };

        Ok(best
            .map(|m| m.name.clone())
            .ok_or(RouterError::NoModelMatched)?)
    }

    /// Get a model's info by name.
    pub fn get_model(&self, name: &str) -> Option<&ModelInfo> {
        self.models.get(name)
    }

    /// List all registered model names.
    pub fn list_models(&self) -> Vec<String> {
        self.models.keys().cloned().collect()
    }
}

impl Default for SmartModelRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Rough token estimate: ~4 chars per token for English text.
fn estimate_tokens(text: &str) -> usize {
    (text.len() / 4).max(1)
}

/// Fastness heuristic: smaller number = faster.
/// Models with "mini" or "flash" in their name are considered fast.
fn fastness_score(model: &ModelInfo) -> u32 {
    let name_lower = model.name.to_lowercase();
    if name_lower.contains("mini") || name_lower.contains("flash") {
        0
    } else if name_lower.contains("gpt-3") || name_lower.contains("haiku") {
        1
    } else if name_lower.contains("sonnet") || name_lower.contains("gpt-4o-mini") {
        2
    } else {
        3
    }
}

// ---------------------------------------------------------------------------
// Default models
// ---------------------------------------------------------------------------

/// Create a router pre-loaded with common models.
impl SmartModelRouter {
    pub fn with_default_models() -> Self {
        let mut router = Self::new();

        router.register(ModelInfo {
            name: "gpt-4o".into(),
            provider: "openai".into(),
            context_window: 128_000,
            cost_per_input_token: 2.5e-6,
            cost_per_output_token: 1.0e-5,
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Vision,
                ModelCapability::Code,
                ModelCapability::FunctionCalling,
                ModelCapability::Streaming,
                ModelCapability::Reasoning,
            ],
        });

        router.register(ModelInfo {
            name: "gpt-4o-mini".into(),
            provider: "openai".into(),
            context_window: 128_000,
            cost_per_input_token: 1.5e-7,
            cost_per_output_token: 6.0e-7,
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Vision,
                ModelCapability::Code,
                ModelCapability::FunctionCalling,
                ModelCapability::Streaming,
            ],
        });

        router.register(ModelInfo {
            name: "claude-sonnet-4-20250514".into(),
            provider: "anthropic".into(),
            context_window: 200_000,
            cost_per_input_token: 3.0e-6,
            cost_per_output_token: 1.5e-5,
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Vision,
                ModelCapability::Code,
                ModelCapability::FunctionCalling,
                ModelCapability::Streaming,
                ModelCapability::Reasoning,
            ],
        });

        router.register(ModelInfo {
            name: "claude-haiku-3-5-20241022".into(),
            provider: "anthropic".into(),
            context_window: 200_000,
            cost_per_input_token: 8.0e-7,
            cost_per_output_token: 4.0e-6,
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Vision,
                ModelCapability::Code,
                ModelCapability::FunctionCalling,
                ModelCapability::Streaming,
            ],
        });

        router.register(ModelInfo {
            name: "gemini-2.0-flash".into(),
            provider: "google".into(),
            context_window: 1_048_576,
            cost_per_input_token: 1.0e-7,
            cost_per_output_token: 4.0e-7,
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Vision,
                ModelCapability::Code,
                ModelCapability::FunctionCalling,
                ModelCapability::Streaming,
            ],
        });

        router.register(ModelInfo {
            name: "o3".into(),
            provider: "openai".into(),
            context_window: 200_000,
            cost_per_input_token: 1.0e-5,
            cost_per_output_token: 4.0e-5,
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Code,
                ModelCapability::FunctionCalling,
                ModelCapability::Streaming,
                ModelCapability::Reasoning,
            ],
        });

        router
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_by_capability() {
        let router = SmartModelRouter::with_default_models();
        let req = ModelRequirements {
            capabilities: vec![ModelCapability::Reasoning],
            max_context: None,
            max_cost: None,
            prefer_fast: false,
        };
        let model = router.route("Explain quantum computing", &req).unwrap();
        assert!(model == "gpt-4o" || model == "claude-sonnet-4-20250514" || model == "o3");
    }

    #[test]
    fn test_route_by_cost() {
        let router = SmartModelRouter::with_default_models();
        let req = ModelRequirements {
            capabilities: vec![ModelCapability::Chat],
            max_context: None,
            max_cost: Some(1e-4),
            prefer_fast: false,
        };
        let model = router.route("Hello", &req).unwrap();
        // Should pick cheapest chat model
        assert!(model.contains("flash") || model.contains("mini") || model.contains("haiku"));
    }

    #[test]
    fn test_route_no_match() {
        let router = SmartModelRouter::with_default_models();
        let req = ModelRequirements {
            // Use a capability that no default model has
            capabilities: vec![ModelCapability::Embedding],
            max_context: None,
            max_cost: None,
            prefer_fast: false,
        };
        assert!(router.route("test", &req).is_err());
    }

    #[test]
    fn test_empty_router() {
        let router = SmartModelRouter::new();
        let req = ModelRequirements::default();
        assert!(matches!(
            router.route("test", &req),
            Err(RouterError::NoModelsRegistered)
        ));
    }
}
