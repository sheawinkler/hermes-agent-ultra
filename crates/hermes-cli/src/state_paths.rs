//! Binary-crate re-exports for Hermes state-root path helpers.

use std::path::PathBuf;

use hermes_cli::cli::Cli;
use hermes_cli::paths::CliStateRoot;

/// Config/state root shared by CLI, `hermes gateway`, cron, and `webhooks.json`.
pub(crate) fn hermes_state_root(cli: &Cli) -> PathBuf {
    CliStateRoot::from_config_dir(cli.config_dir.as_deref().map(std::path::Path::new))
        .root()
        .to_path_buf()
}

/// Log when `HERMES_HOME` was remapped to the ultra home for this process.
pub(crate) fn log_legacy_home_env_hint(
    prior_home: Option<&str>,
    migrated_home: &std::path::Path,
) {
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
