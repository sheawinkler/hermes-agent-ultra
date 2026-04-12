//! Hermes Intelligence Crate
//!
//! Smart model router, error classifier, usage/pricing tracker,
//! title generator, insights, prompt builder, and redaction.

pub mod error_classifier;
pub mod insights;
pub mod prompt;
pub mod redact;
pub mod router;
pub mod title;
pub mod usage;

#[cfg(feature = "rl")]
pub mod rl;

pub use error_classifier::{ErrorCategory, ErrorClassifier, RetryStrategy};
pub use insights::Insights;
pub use prompt::PromptBuilder;
pub use redact::{RedactionPattern, Redactor};
pub use router::{ModelCapability, ModelInfo, ModelRequirements, RouterError, SmartModelRouter};
pub use title::{TitleError, TitleGenerator};
pub use usage::{ModelPricing, ModelUsage, UsageRecord, UsageSummary, UsageTracker};

#[cfg(feature = "rl")]
pub use rl::*;