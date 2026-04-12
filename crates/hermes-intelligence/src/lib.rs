//! Hermes Intelligence Crate
//!
//! Smart model router, error classifier, usage/pricing tracker,
//! title generator, insights, prompt builder, redaction,
//! Anthropic adapter, display formatting, credential pool,
//! model metadata, usage pricing, and context engine.

pub mod anthropic_adapter;
pub mod context_engine;
pub mod credential_pool;
pub mod display;
pub mod error_classifier;
pub mod insights;
pub mod model_metadata;
pub mod prompt;
pub mod redact;
pub mod router;
pub mod self_evolution;
pub mod title;
pub mod usage;
pub mod usage_pricing;

#[cfg(feature = "rl")]
pub mod rl;

pub use error_classifier::{ErrorCategory, ErrorClassifier, RetryStrategy};
pub use insights::Insights;
pub use prompt::PromptBuilder;
pub use redact::{RedactionPattern, Redactor};
pub use router::{
    ModelCapability, ModelInfo as RouterModelInfo, ModelRequirements, RouterError, SmartModelRouter,
};
pub use self_evolution::{
    AdaptivePolicyEngine, EvolutionConfig, HardGateConfig, LongTaskPlan, OutcomeSignals,
    PolicyStore, PolicyVersion,
};
pub use title::{TitleError, TitleGenerator};
pub use usage::{ModelPricing, ModelUsage, UsageRecord, UsageSummary, UsageTracker};

pub use anthropic_adapter::{
    AnthropicContent, AnthropicContentBlock, AnthropicMessage, AnthropicTool,
    NormalizedAssistantMessage, NormalizedToolCall, ReasoningConfig, ReasoningEffort,
};
pub use context_engine::{
    ContextEngine, ContextError, DefaultContextEngine, ImportanceBasedEngine,
};
pub use credential_pool::{CredentialPool, PoolManager, PoolStrategy, PooledCredential};
pub use display::{
    build_tool_preview, format_context_pressure, format_cost, format_duration_compact,
    format_model_response, format_progress_bar, format_token_count, format_tool_call,
    format_tool_result, format_usage_stats, get_cute_tool_message, render_inline_unified_diff,
};
pub use model_metadata::{
    estimate_messages_tokens_rough, estimate_request_tokens_rough, estimate_tokens_rough,
    get_model_context_length, get_model_info, get_next_probe_tier, infer_provider_from_url,
    is_local_endpoint, known_models, max_output_tokens, supports_tools, supports_vision,
    ModelMetadataEntry,
};
pub use usage_pricing::{
    calculate_cost, format_token_count_compact, get_pricing, get_pricing_entry, has_known_pricing,
    normalize_usage, resolve_billing_route, BillingMode, BillingRoute, CanonicalUsage, CostResult,
    CostSource, CostStatus, PricingEntry,
};

#[cfg(feature = "rl")]
pub use rl::*;
