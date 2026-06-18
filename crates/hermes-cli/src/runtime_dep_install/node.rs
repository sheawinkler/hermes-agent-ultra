use std::path::PathBuf;

use hermes_config::hermes_home;
use reqwest::Client;
use serde::Deserialize;
use tracing::info;

use super::download::{
    ArchiveFormat, InstallError, download_with_mirrors, extract_tree, http_client, set_executable,
};
use super::probe::pick_fastest_url;

#[derive(Debug, Deserialize)]
struct NodeDistEntry {
    version: String,
    lts: serde_json::Value,
}

fn node_binary_name() -> &'static str {
    if cfg!(windows) { "node.exe" } else { "node" }
}

fn managed_node_bin() -> PathBuf {
    hermes_home()
        .join("node")
        .join("bin")
        .join(node_binary_name())
}

fn node_platform_slug() -> Result<(&'static str, &'static str, ArchiveFormat), InstallError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(("win", "x64", ArchiveFormat::Zip)),
        ("windows", "x86") => Ok(("win", "x86", ArchiveFormat::Zip)),
        ("windows", "aarch64") => Ok(("win", "arm64", ArchiveFormat::Zip)),
        ("linux", "x86_64") => Ok(("linux", "x64", ArchiveFormat::TarXz)),
        ("linux", "x86") => Ok(("linux", "x86", ArchiveFormat::TarXz)),
        ("linux", "aarch64") => Ok(("linux", "arm64", ArchiveFormat::TarXz)),
        ("linux", "arm") => Ok(("linux", "armv7l", ArchiveFormat::TarXz)),
        ("macos", "x86_64") => Ok(("darwin", "x64", ArchiveFormat::TarGz)),
        ("macos", "aarch64") => Ok(("darwin", "arm64", ArchiveFormat::TarGz)),
        _ => Err(InstallError::NoMirrors),
    }
}

async fn resolve_node_version(client: &Client) -> Result<String, InstallError> {
    if let Ok(version) = std::env::var("HERMES_NODE_VERSION") {
        let trimmed = version.trim();
        if !trimmed.is_empty() {
            return Ok(if trimmed.starts_with('v') {
                trimmed.to_string()
            } else {
                format!("v{trimmed}")
            });
        }
    }

    let resp = client
        .get("https://nodejs.org/dist/index.json")
        .send()
        .await
        .map_err(|e| InstallError::Version(e.to_string()))?;
    let entries: Vec<NodeDistEntry> = resp
        .json()
        .await
        .map_err(|e| InstallError::Version(e.to_string()))?;
    for entry in entries {
        let is_lts = match &entry.lts {
            serde_json::Value::Bool(b) => *b,
            serde_json::Value::String(s) => !s.is_empty() && s != "false",
            _ => false,
        };
        if is_lts {
            return Ok(entry.version);
        }
    }
    Err(InstallError::Version("no Node.js LTS entry found".into()))
}

fn node_archive_name(version: &str, os: &str, arch: &str, format: ArchiveFormat) -> String {
    let ext = match format {
        ArchiveFormat::Zip => "zip",
        ArchiveFormat::TarXz => "tar.xz",
        ArchiveFormat::TarGz => "tar.gz",
        ArchiveFormat::SevenZip => "7z",
    };
    format!("node-{version}-{os}-{arch}.{ext}")
}

fn node_mirror_urls(version: &str, archive: &str) -> Vec<String> {
    vec![
        format!("https://nodejs.org/dist/{version}/{archive}"),
        format!("https://npmmirror.com/mirrors/node/{version}/{archive}"),
    ]
}

pub async fn ensure_node(quiet: bool) -> Result<PathBuf, InstallError> {
    let dest = managed_node_bin();
    if dest.is_file() {
        return Ok(dest);
    }

    let client = http_client()?;
    let version = resolve_node_version(&client).await?;
    let (os, arch, format) = node_platform_slug()?;
    let archive = node_archive_name(&version, os, arch, format);
    let urls = node_mirror_urls(&version, &archive);

    if !quiet {
        info!(%version, os, arch, "probing Node.js mirrors");
    }
    let url_refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    let _ = pick_fastest_url(&client, &url_refs).await;

    let home = hermes_home();
    let node_root = home.join("node");
    std::fs::create_dir_all(&node_root)?;

    let temp_dir = std::env::temp_dir().join(format!("hermes-node-{}", std::process::id()));
    tokio::fs::create_dir_all(&temp_dir).await?;

    let (archive_path, detected_format) =
        download_with_mirrors(&client, &urls, &temp_dir, quiet).await?;

    extract_tree(&archive_path, detected_format, &node_root)?;
    set_executable(&dest);

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    if !dest.is_file() {
        return Err(InstallError::Extract(format!(
            "node binary missing at {}",
            dest.display()
        )));
    }
    if !quiet {
        info!(path = %dest.display(), %version, "Node.js installed");
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_archive_name_windows() {
        let name = node_archive_name("v22.16.0", "win", "x64", ArchiveFormat::Zip);
        assert_eq!(name, "node-v22.16.0-win-x64.zip");
    }
}
