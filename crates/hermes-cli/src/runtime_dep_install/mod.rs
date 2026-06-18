//! Silent runtime dependency installation.

mod browser;
mod download;
mod ffmpeg;
mod node;
mod probe;
mod ripgrep;

use hermes_config::dep_check::{RuntimeDep, is_available};
use tracing::{debug, info, warn};

pub use browser::ensure_browser;
pub use download::InstallError;
pub use ffmpeg::ensure_ffmpeg;
pub use node::ensure_node;
pub use ripgrep::ensure_ripgrep;

const AUTO_ENSURE_ENV: &str = "HERMES_AUTO_ENSURE_DEPS";

/// Whether gateway/CLI should attempt silent dependency installation.
pub fn auto_ensure_enabled() -> bool {
    std::env::var(AUTO_ENSURE_ENV)
        .ok()
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true)
}

/// Install a single runtime dependency when missing (`quiet` suppresses stdout).
pub async fn ensure_runtime_dep(dep: RuntimeDep, quiet: bool) -> bool {
    if is_available(dep) {
        debug!(%dep, "runtime dependency already available");
        return true;
    }

    let result: Result<(), InstallError> = match dep {
        RuntimeDep::Ffmpeg => ensure_ffmpeg(quiet)
            .await
            .map(|_| ())
            .map_err(|e| InstallError::Download(e.to_string())),
        RuntimeDep::Node => ensure_node(quiet).await.map(|_| ()),
        RuntimeDep::Ripgrep => ensure_ripgrep(quiet).await.map(|_| ()),
        RuntimeDep::Browser => ensure_browser(quiet).await.map(|_| ()),
    };

    let ok = result.is_ok();
    if let Err(e) = result {
        if quiet {
            warn!(%dep, error = %e, "runtime dependency auto-install failed");
        } else {
            eprintln!("Failed to install {dep}: {e}");
        }
    } else if is_available(dep) {
        if !quiet {
            info!(%dep, "runtime dependency installed");
        }
    } else {
        warn!(%dep, "install finished but dependency still not detected");
        return false;
    }

    ok && is_available(dep)
}

/// Ensure all missing deps when [`auto_ensure_enabled`] is true.
pub async fn ensure_missing_runtime_deps(
    deps: &[RuntimeDep],
    quiet: bool,
) -> Vec<(RuntimeDep, bool)> {
    let mut results = Vec::new();
    for &dep in deps {
        if is_available(dep) {
            results.push((dep, true));
            continue;
        }
        if !auto_ensure_enabled() {
            debug!(%dep, "HERMES_AUTO_ENSURE_DEPS disabled; skipping auto install");
            results.push((dep, false));
            continue;
        }
        let ok = ensure_runtime_dep(dep, quiet).await;
        results.push((dep, ok));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_ensure_defaults_on() {
        let prior = std::env::var(AUTO_ENSURE_ENV).ok();
        unsafe { std::env::remove_var(AUTO_ENSURE_ENV) };
        assert!(auto_ensure_enabled());
        unsafe { std::env::remove_var(AUTO_ENSURE_ENV) };
        if let Some(v) = prior {
            unsafe { std::env::set_var(AUTO_ENSURE_ENV, v) };
        }
    }
}
