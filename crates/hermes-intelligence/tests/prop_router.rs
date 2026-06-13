//! Bounded invariant coverage: smart model router satisfies constraints
//! **Validates: Requirement 16.1**
//!
//! For registered model sets and requirements, if route() returns Ok, the
//! selected model satisfies all capability, context window, and cost constraints.

use hermes_intelligence::{
    ModelCapability, ModelRequirements, RouterModelInfo as ModelInfo, SmartModelRouter,
};

fn model(
    name: &str,
    provider: &str,
    context_window: usize,
    cost_per_input_token: f64,
    capabilities: Vec<ModelCapability>,
) -> ModelInfo {
    ModelInfo {
        name: name.to_string(),
        provider: provider.to_string(),
        context_window,
        cost_per_input_token,
        cost_per_output_token: cost_per_input_token * 2.0,
        capabilities,
    }
}

fn model_sets() -> Vec<Vec<ModelInfo>> {
    vec![
        vec![model(
            "gpt-4o-mini",
            "openai",
            128_000,
            0.00000015,
            vec![
                ModelCapability::Chat,
                ModelCapability::Code,
                ModelCapability::FunctionCalling,
                ModelCapability::Streaming,
            ],
        )],
        vec![
            model(
                "claude-sonnet",
                "anthropic",
                200_000,
                0.000003,
                vec![
                    ModelCapability::Chat,
                    ModelCapability::Code,
                    ModelCapability::Reasoning,
                    ModelCapability::FunctionCalling,
                ],
            ),
            model(
                "gemini-flash",
                "google",
                1_000_000,
                0.00000035,
                vec![
                    ModelCapability::Chat,
                    ModelCapability::Vision,
                    ModelCapability::Streaming,
                    ModelCapability::Embedding,
                ],
            ),
        ],
        vec![
            model(
                "text-embed",
                "openai",
                8_192,
                0.00000002,
                vec![ModelCapability::Embedding],
            ),
            model(
                "vision-pro",
                "google",
                65_536,
                0.000001,
                vec![ModelCapability::Chat, ModelCapability::Vision],
            ),
            model(
                "reasoning-large",
                "anthropic",
                200_000,
                0.000004,
                vec![ModelCapability::Chat, ModelCapability::Reasoning],
            ),
        ],
    ]
}

fn requirement_cases() -> Vec<ModelRequirements> {
    vec![
        ModelRequirements::default(),
        ModelRequirements {
            capabilities: vec![ModelCapability::Chat],
            max_context: Some(1_000),
            max_cost: Some(10.0),
            prefer_fast: false,
        },
        ModelRequirements {
            capabilities: vec![ModelCapability::Vision],
            max_context: Some(64_000),
            max_cost: Some(10.0),
            prefer_fast: true,
        },
        ModelRequirements {
            capabilities: vec![ModelCapability::Reasoning],
            max_context: Some(200_000),
            max_cost: Some(10.0),
            prefer_fast: false,
        },
        ModelRequirements {
            capabilities: vec![ModelCapability::Embedding],
            max_context: Some(8_000),
            max_cost: Some(10.0),
            prefer_fast: true,
        },
        ModelRequirements {
            capabilities: vec![ModelCapability::FunctionCalling, ModelCapability::Code],
            max_context: Some(128_000),
            max_cost: Some(0.00001),
            prefer_fast: false,
        },
    ]
}

#[test]
fn router_selected_model_satisfies_constraints() {
    let prompt = "Test prompt for routing";

    for models in model_sets() {
        for requirements in requirement_cases() {
            let mut router = SmartModelRouter::new();
            for model in &models {
                router.register(model.clone());
            }

            if let Ok(selected_name) = router.route(prompt, &requirements) {
                let model = router.get_model(&selected_name).unwrap();

                for cap in &requirements.capabilities {
                    assert!(
                        model.capabilities.contains(cap),
                        "Selected model '{}' missing capability {:?}",
                        selected_name,
                        cap
                    );
                }

                if let Some(max_ctx) = requirements.max_context {
                    assert!(
                        model.context_window >= max_ctx,
                        "Selected model '{}' context {} < required {}",
                        selected_name,
                        model.context_window,
                        max_ctx
                    );
                }

                if let Some(max_cost) = requirements.max_cost {
                    let prompt_tokens = (prompt.len() / 4).max(1);
                    let estimated = model.cost_per_input_token * prompt_tokens as f64;
                    assert!(
                        estimated <= max_cost,
                        "Selected model '{}' estimated cost {} > max {}",
                        selected_name,
                        estimated,
                        max_cost
                    );
                }
            }
        }
    }
}
