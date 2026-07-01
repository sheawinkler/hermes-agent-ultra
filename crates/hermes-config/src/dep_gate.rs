//! Tool → runtime dependency mapping and install coordination hooks.
//!
//! [`hermes-cli`] registers background install + wait implementations at startup;
//! [`hermes-agent`] calls [`await_tool_deps`] before executing tools that need them.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use crate::dep_check::{RuntimeDep, description, is_available};

pub type NotifyFn = Arc<dyn Fn(String) + Send + Sync>;
pub type SpawnInstallFn = Box<dyn Fn(Vec<RuntimeDep>) + Send + Sync>;
pub type WaitToolDepsFn =
    Arc<dyn Fn(&str, NotifyFn) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

static SPAWN_INSTALL: OnceLock<SpawnInstallFn> = OnceLock::new();
static WAIT_TOOL_DEPS: OnceLock<WaitToolDepsFn> = OnceLock::new();

/// Register hooks from the CLI runtime (background parallel install + blocking wait).
pub fn register_hooks(spawn: SpawnInstallFn, wait: WaitToolDepsFn) {
    let _ = SPAWN_INSTALL.set(spawn);
    let _ = WAIT_TOOL_DEPS.set(wait);
}

/// Start background installation for the given deps (no-op when hooks are unset).
pub fn spawn_background_install(deps: Vec<RuntimeDep>) {
    if let Some(spawn) = SPAWN_INSTALL.get() {
        spawn(deps);
    }
}

/// Runtime deps required before executing `tool_name`.
pub fn deps_for_tool(tool_name: &str) -> &'static [RuntimeDep] {
    match tool_name {
        "search_files" => &[RuntimeDep::Ripgrep],
        name if name.starts_with("browser_") => &[RuntimeDep::Browser],
        "computer_use" => &[RuntimeDep::Browser],
        "tts" | "tts_premium" | "video_analyze" | "media_long_video" => &[RuntimeDep::Ffmpeg],
        _ => &[],
    }
}

/// Wait until deps for `tool_name` are ready; notify user while waiting.
pub async fn await_tool_deps(tool_name: &str, notify: NotifyFn) -> bool {
    let deps = deps_for_tool(tool_name);
    if deps.is_empty() || deps.iter().all(|dep| is_available(*dep)) {
        return true;
    }
    if let Some(wait) = WAIT_TOOL_DEPS.get() {
        return wait(tool_name, notify).await;
    }
    deps.iter().all(|dep| is_available(*dep))
}

/// User-facing label for missing deps (used by CLI coordinator).
pub fn missing_dep_labels(deps: &[RuntimeDep]) -> String {
    deps.iter()
        .filter(|dep| !is_available(**dep))
        .map(|dep| format!("{} ({})", dep, description(*dep)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_files_needs_ripgrep() {
        assert_eq!(deps_for_tool("search_files"), &[RuntimeDep::Ripgrep]);
    }

    #[test]
    fn browser_tools_need_browser_dep() {
        assert_eq!(deps_for_tool("browser_navigate"), &[RuntimeDep::Browser]);
    }

    #[test]
    fn media_long_video_needs_ffmpeg() {
        assert_eq!(deps_for_tool("media_long_video"), &[RuntimeDep::Ffmpeg]);
    }

    #[test]
    fn unknown_tool_has_no_deps() {
        assert!(deps_for_tool("terminal").is_empty());
    }
}
