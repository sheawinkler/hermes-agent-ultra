//! Property 9: Smart model router satisfies constraints
//! **Validates: Requirement 16.1**
//!
//! For any set of registered models and requirements, if route() returns Ok,
//! the selected model satisfies all capability, context window, and cost constraints.

use proptest::prelude::*;

use hermes_intelligence::{
    ModelCapability, ModelRequirements, RouterModelInfo as ModelInfo, SmartModelRouter,
};

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

fn arb_capability() -> impl Strategy<Value = ModelCapability> {
    prop_oneof![
        Just(ModelCapability::Chat),
        Just(ModelCapability::Vision),
        Just(ModelCapability::Code),
        Just(ModelCapability::FunctionCalling),
        Just(ModelCapability::Streaming),
        Just(ModelCapability::Reasoning),
    ]
}

fn arb_model_info() -> impl Strategy<Value = ModelInfo> {
    (
        "[a-z]{3,10}",
        prop_oneof![Just("openai"), Just("anthropic"), Just("google")],
        1000usize..500_000,
        1e-8f64..1e-4,
        1e-8f64..1e-4,
        proptest::collection::hash_set(arb_capability(), 1..5),
    )
        .prop_map(|(name, provider, ctx, cost_in, cost_out, caps)| ModelInfo {
            name,
            provider: provider.to_string(),
            context_window: ctx,
            cost_per_input_token: cost_in,
            cost_per_output_token: cost_out,
            capabilities: caps.into_iter().collect(),
        })
}

fn arb_requirements() -> impl Strategy<Value = ModelRequirements> {
    (
        proptest::collection::vec(arb_capability(), 0..3),
        proptest::option::of(1000usize..200_000),
        proptest::option::of(1e-4f64..10.0),
        proptest::bool::ANY,
    )
        .prop_map(|(caps, max_ctx, max_cost, prefer_fast)| {
            // Deduplicate capabilities
            let mut unique_caps = caps;
            unique_caps.sort_by_key(|c| format!("{:?}", c));
            unique_caps.dedup();
            ModelRequirements {
                capabilities: unique_caps,
                max_context: max_ctx,
                max_cost,
                prefer_fast,
            }
        })
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_router_selected_model_satisfies_constraints(
        models in proptest::collection::vec(arb_model_info(), 1..6),
        requirements in arb_requirements(),
    ) {
        let mut router = SmartModelRouter::new();
        for model in &models {
            router.register(model.clone());
        }

        let prompt = "Test prompt for routing";
        match router.route(prompt, &requirements) {
            Ok(selected_name) => {
                let model = router.get_model(&selected_name).unwrap();

                // All required capabilities must be present
                for cap in &requirements.capabilities {
                    prop_assert!(
                        model.capabilities.contains(cap),
                        "Selected model '{}' missing capability {:?}",
                        selected_name, cap
                    );
                }

                // Context window must be >= required
                if let Some(max_ctx) = requirements.max_context {
                    prop_assert!(
                        model.context_window >= max_ctx,
                        "Selected model '{}' context {} < required {}",
                        selected_name, model.context_window, max_ctx
                    );
                }

                // Estimated cost must be <= max_cost
                if let Some(max_cost) = requirements.max_cost {
                    let prompt_tokens = (prompt.len() / 4).max(1);
                    let estimated = model.cost_per_input_token * prompt_tokens as f64;
                    prop_assert!(
                        estimated <= max_cost,
                        "Selected model '{}' estimated cost {} > max {}",
                        selected_name, estimated, max_cost
                    );
                }
            }
            Err(_) => {
                // If routing failed, verify no model could satisfy the requirements
                // (this is acceptable — the property only constrains successful routes)
            }
        }
    }
}
