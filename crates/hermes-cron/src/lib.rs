//! # hermes-cron
//!
//! Cron job scheduler for Hermes Agent (Requirement 13).
//!
//! Provides a cron-based scheduler that can create, manage, persist, and
//! execute recurring agent tasks. Jobs are defined by a cron expression
//! schedule, an agent prompt, and optional skill/model/deliver configurations.

pub mod job;
pub mod persistence;
pub mod runner;
pub mod scheduler;

// Re-export primary types
pub use job::{CronJob, DeliverConfig, DeliverTarget, JobStatus, ModelConfig};
pub use persistence::{FileJobPersistence, JobPersistence, SqliteJobPersistence};
pub use runner::CronRunner;
pub use scheduler::{CronError, CronScheduler};