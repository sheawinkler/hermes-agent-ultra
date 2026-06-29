//! Billing, quota, and feature gate for Terra.

pub mod consent_gate;
pub mod feature_gate;
pub mod lang_profile;
pub mod quota;
pub mod tier_mapping;
pub mod tool_budget;
pub mod tool_budget_engine;

pub use consent_gate::{ConsentGate, ConsentGateError};
pub use feature_gate::{FeatureGate, VerticalCap};
pub use lang_profile::{Language, ModelLanguageProfile, ProfileSource};
pub use quota::{QuotaEngine, QuotaError};
pub use tier_mapping::{GlobalTierMapping, ProviderTier, VerticalTierOverrides, resolve_model};
pub use tool_budget::{ToolBudget, ToolId, default_tool_budgets};
pub use tool_budget_engine::{BudgetError, ToolBudgetEngine};
