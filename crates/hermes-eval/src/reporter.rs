//! Serialize [`RunRecord`](crate::result::RunRecord) to disk for CI artifacts and baselines.
//!
//! Parquet / historical diff (`hermes bench compare`) can build on the same JSON.

use std::fs;
use std::path::Path;

use crate::error::EvalResult;
use crate::result::RunRecord;

/// Persists evaluation results (implementations may add compression or uploads).
pub trait Reporter: Send + Sync {
    fn write_run(&self, record: &RunRecord, path: &Path) -> EvalResult<()>;
}

/// Pretty JSON, one file per run (e.g. `evals/run-2026-04-17.json`).
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonReporter;

impl Reporter for JsonReporter {
    fn write_run(&self, record: &RunRecord, path: &Path) -> EvalResult<()> {
        let bytes = serde_json::to_vec_pretty(record)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::BenchmarkMetadata;
    use crate::result::AggregateMetrics;
    use chrono::Utc;

    #[test]
    fn json_reporter_writes_file() {
        let path = std::env::temp_dir().join(format!(
            "hermes_eval_reporter_{}.json",
            uuid::Uuid::new_v4()
        ));
        let record = RunRecord {
            run_id: "test".into(),
            benchmark: BenchmarkMetadata {
                id: "x".into(),
                name: "x".into(),
                source: "x".into(),
                version: "0".into(),
            },
            started_at: Utc::now(),
            finished_at: None,
            seed: 0,
            concurrency: 1,
            model: "m".into(),
            tasks: vec![],
            metrics: AggregateMetrics::default(),
        };
        JsonReporter.write_run(&record, &path).expect("write");
        assert!(path.exists());
        let s = fs::read_to_string(&path).expect("read");
        assert!(s.contains("\"run_id\""));
        let _ = fs::remove_file(&path);
    }
}
