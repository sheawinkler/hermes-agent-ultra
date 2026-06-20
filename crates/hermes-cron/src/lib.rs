//! # hermes-cron
//!
//! Cron job scheduler for Hermes Agent (Requirement 13).
//!
//! Provides a cron-based scheduler that can create, manage, persist, and
//! execute recurring agent tasks. Jobs are defined by a cron expression
//! schedule, an agent prompt, and optional skill/model/deliver configurations.

pub mod backend;
pub mod blueprints;
pub mod chronos;
pub mod cli_support;
pub mod completion;
pub mod job;
pub mod persistence;
pub mod runner;
pub mod scheduler;
pub mod suggestions;

// Re-export primary types
pub use backend::ScheduledCronjobBackend;
pub use blueprints::{
    catalog as blueprint_catalog, fill_blueprint, get_blueprint, resolve_blueprint_command,
    AutomationBlueprint, BlueprintCommandAction, BlueprintFillError, BlueprintJobSpec,
    BlueprintSlot, BlueprintSlotKind,
};
pub use chronos::{
    verify_nas_fire_token, ChronosConfig, ChronosFireClaims, ChronosNasCronProvider,
};
pub use cli_support::{cron_scheduler_for_data_dir, MinimalCronLlm};
pub use completion::CronCompletionEvent;
pub use job::{CronJob, DeliverConfig, DeliverTarget, JobStatus, ModelConfig};
pub use persistence::{FileJobPersistence, JobPersistence, SqliteJobPersistence};
pub use runner::CronRunner;
pub use scheduler::{CronError, CronScheduler, ManagedCronProvider};
pub use suggestions::{
    SuggestionError, SuggestionJobSpec, SuggestionRecord, SuggestionStatus, SuggestionStore,
    MAX_PENDING_SUGGESTIONS,
};
