//! Background parallel runtime dependency installation and tool-time waiting.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use hermes_config::dep_check::{RuntimeDep, is_available};
use hermes_config::dep_gate::{self, NotifyFn};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use super::{auto_ensure_enabled, ensure_runtime_dep};

struct Coordinator {
    running: Mutex<HashSet<RuntimeDep>>,
    failed: Mutex<HashSet<RuntimeDep>>,
    notify: Mutex<HashMap<RuntimeDep, Arc<Notify>>>,
}

impl Coordinator {
    fn new() -> Self {
        Self {
            running: Mutex::new(HashSet::new()),
            failed: Mutex::new(HashSet::new()),
            notify: Mutex::new(HashMap::new()),
        }
    }

    fn notify_for(&self, dep: RuntimeDep) -> Arc<Notify> {
        let mut map = self.notify.lock().expect("notify lock");
        map.entry(dep)
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }

    fn start_install(&self, dep: RuntimeDep) {
        if is_available(dep) {
            return;
        }
        {
            let failed = self.failed.lock().expect("failed lock");
            if failed.contains(&dep) {
                return;
            }
        }
        {
            let mut running = self.running.lock().expect("running lock");
            if running.contains(&dep) {
                return;
            }
            running.insert(dep);
        }

        if !auto_ensure_enabled() {
            debug!(%dep, "HERMES_AUTO_ENSURE_DEPS disabled; not starting background install");
            self.running.lock().expect("running lock").remove(&dep);
            self.failed.lock().expect("failed lock").insert(dep);
            self.notify_for(dep).notify_waiters();
            return;
        }

        let notify = self.notify_for(dep);
        let coordinator = coordinator();
        info!(%dep, "starting background runtime dependency install");
        tokio::spawn(async move {
            let ok = ensure_runtime_dep(dep, true).await;
            {
                let mut running = coordinator.running.lock().expect("running lock");
                running.remove(&dep);
            }
            if !ok {
                warn!(%dep, "background runtime dependency install failed");
                coordinator.failed.lock().expect("failed lock").insert(dep);
            } else {
                info!(%dep, "background runtime dependency install finished");
            }
            notify.notify_waiters();
        });
    }
}

fn coordinator() -> &'static Arc<Coordinator> {
    static COORD: OnceLock<Arc<Coordinator>> = OnceLock::new();
    COORD.get_or_init(|| Arc::new(Coordinator::new()))
}

fn spawn_background_install(deps: Vec<RuntimeDep>) {
    let mut need_browser = false;
    let mut parallel = Vec::new();
    for dep in deps {
        if is_available(dep) {
            continue;
        }
        if dep == RuntimeDep::Browser {
            need_browser = true;
        } else {
            parallel.push(dep);
        }
    }
    let coord = coordinator();
    for dep in parallel {
        coord.start_install(dep);
    }
    if need_browser {
        let coord = Arc::clone(coord);
        tokio::spawn(async move {
            if !is_available(RuntimeDep::Node) {
                coord.start_install(RuntimeDep::Node);
                if !wait_dep_ready(RuntimeDep::Node, None).await {
                    return;
                }
            }
            coord.start_install(RuntimeDep::Browser);
        });
    }
}

async fn wait_dep_ready(dep: RuntimeDep, notify: Option<&NotifyFn>) -> bool {
    if is_available(dep) {
        return true;
    }
    coordinator().start_install(dep);

    let coord = coordinator();
    let dep_notify = coord.notify_for(dep);
    loop {
        if is_available(dep) {
            return true;
        }
        if coord.failed.lock().expect("failed lock").contains(&dep) {
            return false;
        }
        if let Some(cb) = notify {
            let labels = dep_gate::missing_dep_labels(&[dep]);
            cb(format!("仍在等待运行时依赖: {labels}…"));
        }
        tokio::select! {
            _ = dep_notify.notified() => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
        }
    }
}

async fn wait_tool_deps(tool_name: &str, notify: NotifyFn) -> bool {
    let deps = dep_gate::deps_for_tool(tool_name);
    let missing: Vec<RuntimeDep> = deps
        .iter()
        .copied()
        .filter(|dep| !is_available(*dep))
        .collect();
    if missing.is_empty() {
        return true;
    }

    let labels = dep_gate::missing_dep_labels(&missing);
    notify(format!(
        "缺少运行时依赖: {labels}；后台正在安装，完成后继续执行 `{tool_name}`…"
    ));

    spawn_background_install(missing.clone());

    for dep in missing {
        if !wait_dep_ready(dep, Some(&notify)).await {
            return false;
        }
    }
    true
}

/// Register dep-gate hooks so gateway/agent can background-install and wait at tool time.
pub fn register_dep_gate_hooks() {
    dep_gate::register_hooks(
        Box::new(spawn_background_install),
        Arc::new(
            |tool: &str, notify: NotifyFn| -> Pin<Box<dyn Future<Output = bool> + Send>> {
                let tool = tool.to_string();
                Box::pin(async move { wait_tool_deps(&tool, notify).await })
            },
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinator_singleton() {
        assert!(Arc::ptr_eq(coordinator(), coordinator()));
    }
}
