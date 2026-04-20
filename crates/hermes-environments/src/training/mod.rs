//! Training / benchmark loops (PARITY_PLAN Week 3).
//!
//! This module is a **skeleton**: wire up SWE-bench, TBLite, Terminal-Bench 2, and
//! tool-call parsers incrementally. Prefer:
//! - Rust for the env loop, trajectory recording, and Hermes tool dispatch
//! - Optional `python3 -c ...` subprocess for HuggingFace `datasets` when needed
//!
//! Next steps (see root `PARITY_PLAN.md`):
//! - `HermesBaseEnv` impl with config compatible with Python `default.yaml`
//! - `training/benchmarks/*` as submodules (tblite, terminalbench, yc_bench)
//! - Parsers under `training/parsers/` mirroring `tool_call_parsers` in Python

/// Placeholder for a training or benchmark episode (one task + trajectory).
pub trait HermesEpisode: Send {
    /// Stable task id for logging.
    fn task_id(&self) -> &str;
}

/// Long-term: environment that loads tasks, runs the agent loop, emits trajectories.
///
/// Implementations may wrap Docker, local shell, or remote runners (`daytona`, `modal`, …).
pub trait HermesBaseEnv: Send + Sync {
    /// Dataset / benchmark identifier (e.g. HuggingFace slug).
    fn dataset_id(&self) -> &str;
}
