use std::path::PathBuf;

use hermes_config::hermes_home;
use tokio::process::Command;
use tracing::info;

use super::download::InstallError;
use super::node::ensure_node;

fn managed_agent_browser() -> PathBuf {
    if cfg!(windows) {
        hermes_home()
            .join("node")
            .join("bin")
            .join("agent-browser.cmd")
    } else {
        hermes_home().join("node").join("bin").join("agent-browser")
    }
}

fn npm_binary(prefix: &PathBuf) -> PathBuf {
    if cfg!(windows) {
        prefix.join("bin").join("npm.cmd")
    } else {
        prefix.join("bin").join("npm")
    }
}

fn npm_registry() -> Option<String> {
    std::env::var("HERMES_NPM_REGISTRY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("NPM_CONFIG_REGISTRY")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
}

/// Install `agent-browser` under `$HERMES_HOME/node` and fetch browser deps.
pub async fn ensure_browser(quiet: bool) -> Result<PathBuf, InstallError> {
    let dest = managed_agent_browser();
    if dest.is_file() {
        return Ok(dest);
    }

    ensure_node(quiet).await?;

    let home = hermes_home();
    let prefix = home.join("node");
    let npm = npm_binary(&prefix);
    if !npm.is_file() {
        return Err(InstallError::Command(format!(
            "npm not found at {}",
            npm.display()
        )));
    }

    if !quiet {
        info!("installing agent-browser via npm");
    }

    let mut install = Command::new(&npm);
    install.args([
        "install",
        "agent-browser",
        "--prefix",
        &prefix.to_string_lossy(),
        "--no-fund",
        "--no-audit",
        "--loglevel=error",
    ]);
    if let Some(registry) = npm_registry() {
        install.env("npm_config_registry", registry);
    }
    let status = install
        .status()
        .await
        .map_err(|e| InstallError::Command(e.to_string()))?;
    if !status.success() {
        return Err(InstallError::Command(
            "npm install agent-browser failed".into(),
        ));
    }

    if !dest.is_file() {
        return Err(InstallError::Command(format!(
            "agent-browser missing at {}",
            dest.display()
        )));
    }

    if !quiet {
        info!("installing agent-browser runtime dependencies");
    }
    let mut setup = Command::new(&dest);
    setup.arg("install").arg("--with-deps");
    if let Some(registry) = npm_registry() {
        setup.env("npm_config_registry", registry);
    }
    let setup_status = setup
        .status()
        .await
        .map_err(|e| InstallError::Command(e.to_string()))?;
    if !setup_status.success() {
        return Err(InstallError::Command(
            "agent-browser install --with-deps failed".into(),
        ));
    }

    if !quiet {
        info!(path = %dest.display(), "agent-browser installed");
    }
    Ok(dest)
}
