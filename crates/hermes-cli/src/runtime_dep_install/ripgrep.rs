use std::path::{Path, PathBuf};

use hermes_bundled_rg;
use hermes_config::hermes_home;
use reqwest::Client;
use serde::Deserialize;
use tracing::info;

use super::download::{
    InstallError, download_with_mirrors, extract_binary, http_client, set_executable,
};

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

fn managed_rg_path() -> PathBuf {
    #[cfg(windows)]
    {
        hermes_home().join("bin").join("rg.exe")
    }
    #[cfg(not(windows))]
    {
        hermes_home().join("bin").join("rg")
    }
}

fn rg_binary_name() -> &'static str {
    if cfg!(windows) { "rg.exe" } else { "rg" }
}

fn materialize_bundled(dest: &Path) -> Result<(), InstallError> {
    hermes_bundled_rg::materialize(dest).map_err(|e| InstallError::Download(e.to_string()))
}

fn network_fallback_enabled() -> bool {
    std::env::var("HERMES_RG_NETWORK_FALLBACK")
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn ripgrep_target_suffix() -> Result<&'static str, InstallError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Ok("aarch64-pc-windows-msvc"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("linux", "arm") => Ok("arm-unknown-linux-gnueabihf"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        _ => Err(InstallError::NoMirrors),
    }
}

async fn resolve_ripgrep_asset(client: &Client) -> Result<(String, String), InstallError> {
    let suffix = ripgrep_target_suffix()?;
    let ext = archive_ext();

    if let Ok(version) = std::env::var("HERMES_RIPGREP_VERSION") {
        let tag = version.trim().trim_start_matches('v');
        if !tag.is_empty() {
            let archive = format!("ripgrep-{tag}-{suffix}.{ext}");
            let url =
                format!("https://github.com/BurntSushi/ripgrep/releases/download/{tag}/{archive}");
            return Ok((archive, url));
        }
    }

    let bundled = hermes_bundled_rg::version();
    let archive = format!("ripgrep-{bundled}-{suffix}.{ext}");
    let url =
        format!("https://github.com/BurntSushi/ripgrep/releases/download/{bundled}/{archive}");
    if network_fallback_enabled() {
        let resp = client
            .get("https://api.github.com/repos/BurntSushi/ripgrep/releases/latest")
            .header("User-Agent", "hermes-agent-ultra/dep-install")
            .send()
            .await
            .map_err(|e| InstallError::Version(e.to_string()))?;
        let release: GhRelease = resp
            .json()
            .await
            .map_err(|e| InstallError::Version(e.to_string()))?;
        let tag = release.tag_name.clone();
        let asset_name = format!("ripgrep-{}-{suffix}.{ext}", tag.trim_start_matches('v'));
        let direct = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .map(|asset| asset.browser_download_url.clone())
            .unwrap_or_else(|| {
                format!(
                    "https://github.com/BurntSushi/ripgrep/releases/download/{tag}/{asset_name}"
                )
            });
        return Ok((asset_name, direct));
    }
    Ok((archive, url))
}

fn archive_ext() -> &'static str {
    if cfg!(windows) { "zip" } else { "tar.gz" }
}

fn ripgrep_mirrors(primary: &str, tag: &str, archive: &str) -> Vec<String> {
    let gh = format!("https://github.com/BurntSushi/ripgrep/releases/download/{tag}/{archive}");
    if primary == gh {
        vec![
            gh.clone(),
            format!("https://ghfast.top/{gh}"),
            format!("https://mirror.ghproxy.com/{gh}"),
        ]
    } else {
        vec![primary.to_string(), gh]
    }
}

async fn ensure_ripgrep_network(quiet: bool) -> Result<PathBuf, InstallError> {
    let dest = managed_rg_path();
    let client = http_client()?;
    let (archive, primary_url) = resolve_ripgrep_asset(&client).await?;
    let tag = archive
        .strip_prefix("ripgrep-")
        .and_then(|rest| rest.find('-').map(|idx| &rest[..idx]))
        .unwrap_or("latest");
    let urls = ripgrep_mirrors(&primary_url, tag, &archive);

    if !quiet {
        info!(archive = %archive, "probing ripgrep mirrors");
    }

    let home = hermes_home();
    std::fs::create_dir_all(home.join("bin"))?;

    let temp_dir = std::env::temp_dir().join(format!("hermes-rg-{}", std::process::id()));
    tokio::fs::create_dir_all(&temp_dir).await?;

    let (archive_path, format) = download_with_mirrors(&client, &urls, &temp_dir, quiet).await?;
    extract_binary(&archive_path, format, &dest, rg_binary_name())?;
    set_executable(&dest);

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    if !quiet {
        info!(path = %dest.display(), "ripgrep installed from network");
    }
    Ok(dest)
}

pub async fn ensure_ripgrep(quiet: bool) -> Result<PathBuf, InstallError> {
    let dest = managed_rg_path();
    if dest.is_file() {
        return Ok(dest);
    }

    std::fs::create_dir_all(hermes_home().join("bin"))?;
    let dest_for_bundle = dest.clone();
    match tokio::task::spawn_blocking(move || materialize_bundled(&dest_for_bundle)).await {
        Ok(Ok(())) if dest.is_file() => {
            if !quiet {
                info!(
                    path = %dest.display(),
                    version = hermes_bundled_rg::version(),
                    "ripgrep materialized from build-time bundle"
                );
            }
            return Ok(dest);
        }
        Ok(Err(e)) if !quiet => {
            tracing::debug!(error = %e, "bundled ripgrep materialize failed");
        }
        Err(e) if !quiet => {
            tracing::debug!(error = %e, "bundled ripgrep task join failed");
        }
        _ => {}
    }

    if network_fallback_enabled() {
        return ensure_ripgrep_network(quiet).await;
    }

    Err(InstallError::Download(
        "bundled ripgrep unavailable; set HERMES_RG_NETWORK_FALLBACK=1 to download at runtime"
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_target_suffix() {
        if std::env::consts::OS == "windows" && std::env::consts::ARCH == "x86_64" {
            assert_eq!(ripgrep_target_suffix().unwrap(), "x86_64-pc-windows-msvc");
        }
    }
}
