//! Run manager that tracks all training runs.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::TrainingConfig;
use crate::training::{TrainingMetrics, TrainingStatus};

/// A single training run with full metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingRun {
    pub id: String,
    pub environment: String,
    pub status: TrainingStatus,
    pub config: TrainingConfig,
    pub metrics: TrainingMetrics,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
}

/// Manages training runs on disk.
pub struct RunManager {
    pub data_dir: PathBuf,
    runs: HashMap<String, TrainingRun>,
}

impl RunManager {
    /// Create a new run manager storing data under `data_dir`.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            runs: HashMap::new(),
        }
    }

    /// Create and register a new training run.
    pub fn create_run(&mut self, environment: &str, config: TrainingConfig) -> String {
        let id = format!("run-{}", uuid_v4_stub());
        let run = TrainingRun {
            id: id.clone(),
            environment: environment.to_string(),
            status: TrainingStatus::Pending,
            config: config.clone(),
            metrics: TrainingMetrics {
                total_steps: config.max_steps,
                ..Default::default()
            },
            started_at: Utc::now(),
            finished_at: None,
        };
        self.runs.insert(id.clone(), run);
        id
    }

    /// Get a run by ID.
    pub fn get_run(&self, id: &str) -> Option<&TrainingRun> {
        self.runs.get(id)
    }

    /// Get a mutable reference to a run by ID.
    pub fn get_run_mut(&mut self, id: &str) -> Option<&mut TrainingRun> {
        self.runs.get_mut(id)
    }

    /// List all runs.
    pub fn list_runs(&self) -> Vec<&TrainingRun> {
        let mut runs: Vec<_> = self.runs.values().collect();
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        runs
    }

    /// Update run status.
    pub fn set_status(&mut self, id: &str, status: TrainingStatus) -> bool {
        if let Some(run) = self.runs.get_mut(id) {
            if matches!(
                status,
                TrainingStatus::Completed | TrainingStatus::Failed | TrainingStatus::Stopped
            ) {
                run.finished_at = Some(Utc::now());
            }
            run.status = status;
            true
        } else {
            false
        }
    }

    /// Update metrics for a run.
    pub fn update_metrics(&mut self, id: &str, metrics: TrainingMetrics) -> bool {
        if let Some(run) = self.runs.get_mut(id) {
            run.metrics = metrics;
            true
        } else {
            false
        }
    }
}

fn uuid_v4_stub() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:016x}", nanos)
}
