//! # hermes-parity-tests
//!
//! Week 0 **parity proof chain**: JSON fixtures (recorded from or verified against
//! Python `research/hermes-agent`) plus a small harness that runs the **Rust**
//! implementation and compares outputs.
//!
//! - Fixture layout: `fixtures/<logical_module>/*.json`
//! - Schema: see [`harness::ParityFixtureFile`]
//! - Recording: `scripts/record_fixtures.py` (requires local Python checkout)

pub mod harness;
pub mod recorder;

pub use harness::{
    checkpoint_parse_shortstat, checkpoint_shadow_dir_id, checkpoint_validate_commit_hash,
    dispatch_case, load_fixture_file, run_all_active_fixtures, run_fixture_file,
    run_fixtures_in_dir, CheckpointShortstat, ParityCase, ParityError, ParityFixtureFile,
};
