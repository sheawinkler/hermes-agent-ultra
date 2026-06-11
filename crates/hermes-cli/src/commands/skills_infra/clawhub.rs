//! ClawHub skill source.

use bytes::Bytes;
use hermes_core::AgentError;

use super::constants::CLAWHUB_API_BASE;
use super::parse::ensure_safe_relative_path;
use super::types::{ClawHubLatestVersion, ClawHubSkillDetailResponse};

pub(crate) fn detect_archive_format(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 4
        && bytes[0] == 0x50
        && bytes[1] == 0x4B
        && bytes[2] == 0x03
        && bytes[3] == 0x04
    {
        return "zip";
    }
    if bytes.len() >= 2 && bytes[0] == 0x1F && bytes[1] == 0x8B {
        return "tar.gz";
    }
    "unknown"
}

pub(crate) fn extract_clawhub_archive(bytes: &[u8]) -> Result<Vec<(String, Bytes)>, AgentError> {
    match detect_archive_format(bytes) {
        "zip" => {
            let cursor = std::io::Cursor::new(bytes);
            let mut zip = zip::ZipArchive::new(cursor).map_err(|e| {
                AgentError::Config(format!("Failed to parse ClawHub zip payload: {}", e))
            })?;
            let mut out = Vec::new();
            for i in 0..zip.len() {
                let mut file = zip.by_index(i).map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub zip entry: {}", e))
                })?;
                if file.is_dir() {
                    continue;
                }
                let raw_name = file.name().replace('\\', "/");
                let segments: Vec<&str> = raw_name.split('/').filter(|s| !s.is_empty()).collect();
                let normalized = if segments.is_empty() {
                    file.name().to_string()
                } else if segments.len() == 1 {
                    segments[0].to_string()
                } else {
                    // Drop top-level archive folder if present.
                    segments[1..].join("/")
                };
                ensure_safe_relative_path(&normalized)?;
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut file, &mut buf).map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub file payload: {}", e))
                })?;
                out.push((normalized, Bytes::from(buf)));
            }
            Ok(out)
        }
        "tar.gz" => {
            let decoder = flate2::read::GzDecoder::new(bytes);
            let mut archive = tar::Archive::new(decoder);
            let mut out = Vec::new();
            let entries = archive.entries().map_err(|e| {
                AgentError::Config(format!("Failed to parse ClawHub tar payload: {}", e))
            })?;
            for entry in entries {
                let mut entry = entry.map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub tar entry: {}", e))
                })?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let path = entry
                    .path()
                    .map_err(|e| AgentError::Config(format!("Invalid tar entry path: {}", e)))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let normalized = path.split('/').skip(1).collect::<Vec<_>>().join("/");
                if normalized.is_empty() {
                    continue;
                }
                ensure_safe_relative_path(&normalized)?;
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut buf).map_err(|e| {
                    AgentError::Config(format!("Failed to read ClawHub tar payload: {}", e))
                })?;
                out.push((normalized, Bytes::from(buf)));
            }
            Ok(out)
        }
        _ => Err(AgentError::Config(
            "Unsupported ClawHub archive format (expected zip or tar.gz).".to_string(),
        )),
    }
}

pub(crate) async fn fetch_clawhub_skill_files(
    client: &reqwest::Client,
    slug: &str,
    version_hint: Option<&str>,
) -> Result<Vec<(String, Bytes)>, AgentError> {
    let detail_url = format!("{}/skills/{}", CLAWHUB_API_BASE, slug);
    let detail = client
        .get(&detail_url)
        .header("Accept", "application/json")
        .header("User-Agent", "Mozilla/5.0 hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("ClawHub detail request failed: {}", e)))?;
    if !detail.status().is_success() {
        let status = detail.status();
        let body = detail.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "ClawHub detail lookup failed for '{}' ({}): {}",
            slug, status, body
        )));
    }
    let payload = detail
        .json::<ClawHubSkillDetailResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid ClawHub detail payload: {}", e)))?;
    let version = version_hint
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.to_string())
        .or_else(|| {
            let v = payload.latest_version.version.trim();
            if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            }
        })
        .ok_or_else(|| {
            AgentError::Config(format!("No ClawHub version available for '{}'.", slug))
        })?;

    let download_url = format!(
        "{}/download?slug={}&version={}",
        CLAWHUB_API_BASE,
        urlencoding::encode(slug),
        urlencoding::encode(&version)
    );
    let mut last_err = String::new();
    for attempt in 1..=4 {
        let resp = client
            .get(&download_url)
            .header("Accept", "*/*")
            .header("User-Agent", "Mozilla/5.0 hermes-agent-ultra")
            .timeout(std::time::Duration::from_secs(40))
            .send()
            .await
            .map_err(|e| AgentError::Config(format!("ClawHub download request failed: {}", e)))?;
        if resp.status().is_success() {
            let bytes = resp.bytes().await.map_err(|e| {
                AgentError::Config(format!("Invalid ClawHub download payload: {}", e))
            })?;
            return extract_clawhub_archive(&bytes);
        }
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait_secs = attempt * 2;
            tokio::time::sleep(std::time::Duration::from_secs(wait_secs as u64)).await;
            last_err = "rate limited (429)".to_string();
            continue;
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "ClawHub download failed for '{}@{}' ({}): {}",
            slug, version, status, body
        )));
    }
    Err(AgentError::Config(format!(
        "ClawHub download for '{}@{}' failed after retries: {}",
        slug, version, last_err
    )))
}
