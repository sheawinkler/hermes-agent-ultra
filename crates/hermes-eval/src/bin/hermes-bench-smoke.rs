//! Minimal TBLite smoke run: `cargo run -p hermes-eval --bin hermes-bench-smoke`
//!
//! Environment (optional):
//! - `HERMES_EVAL_MODEL` — recorded on the run (default: `tblite-smoke`)
//! - `HERMES_EVAL_MAX_TASKS` — cap tasks (e.g. `1` for fastest check)

use std::sync::Arc;

use hermes_eval::runner::{Runner, RunnerConfig};
use hermes_eval::{NoopRollout, TbliteSmokeAdapter};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let max_tasks = std::env::var("HERMES_EVAL_MAX_TASKS")
        .ok()
        .and_then(|s| s.parse().ok());

    let config = RunnerConfig {
        model: std::env::var("HERMES_EVAL_MODEL").unwrap_or_else(|_| "tblite-smoke".into()),
        max_tasks,
        ..Default::default()
    };

    let runner = Runner::new(config);
    match runner
        .run(
            Arc::new(TbliteSmokeAdapter::default()),
            Arc::new(NoopRollout),
        )
        .await
    {
        Ok(record) => match serde_json::to_string_pretty(&record) {
            Ok(s) => {
                println!("{}", s);
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("serialize run record: {}", e);
                std::process::ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("{}", e);
            std::process::ExitCode::FAILURE
        }
    }
}
