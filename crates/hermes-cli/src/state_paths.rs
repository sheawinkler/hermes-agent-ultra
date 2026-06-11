//! Centralized Hermes state-root and well-known path helpers for the CLI binary.

use std::path::{Path, PathBuf};

use hermes_cli::cli::Cli;
use hermes_config::state_dir;

/// Config/state root shared by CLI, `hermes gateway`, cron, and `webhooks.json`.
pub(crate) fn hermes_state_root(cli: &Cli) -> PathBuf {
    state_dir(cli.config_dir.as_deref().map(Path::new))
}

/// Log when `HERMES_HOME` was remapped to the ultra home for this process.
pub(crate) fn log_legacy_home_env_hint(prior_home: Option<&str>, migrated_home: &Path) {
    let migrated = migrated_home.to_string_lossy();
    let Some(prior) = prior_home.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    if prior != migrated.as_ref() {
        tracing::info!(
            prior_hermes_home = prior,
            effective_hermes_home = migrated.as_ref(),
            "HERMES_HOME was remapped to the fresh ultra home for this process; legacy data is not copied — update your user environment variable if you want new shells to match"
        );
    }
}
