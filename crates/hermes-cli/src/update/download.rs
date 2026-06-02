use std::path::PathBuf;
use std::process::Command;
use hermes_core::errors::AgentError;
use crate::update::platform::Platform;

/// 下载 artifact 并解压出 binary
/// 返回 (archive_path, extracted_binary_path)，archive 由调用者负责清理
pub async fn download_and_extract(
    url: &str,
    platform: &Platform,
    show_progress: bool,
) -> Result<(PathBuf, PathBuf), AgentError> {
    let temp_dir = std::env::temp_dir();
    let archive_path = temp_dir.join(platform.artifact_name());

    if show_progress {
        println!("Downloading {} ...", url);
    }

    let mut cmd = Command::new("curl");
    cmd.args([
        "-sSL",
        "-o", &archive_path.to_string_lossy(),
        "-H", "User-Agent: hermes-agent-ultra",
    ]);
    // Only attach GitHub token for GitHub URLs
    if url.contains("github.com") || url.contains("githubusercontent.com") {
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
            cmd.args(["-H", "Accept: application/octet-stream"]);
        }
    }
    cmd.arg(url);

    let output = cmd.output()
        .map_err(|e| AgentError::Io(format!("Failed to run curl: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AgentError::Io(format!("Download failed: {stderr}")));
    }

    if show_progress {
        println!("Download complete.");
    }

    // Extract binary from archive
    let binary_name = platform.binary_name();
    let extracted_path = temp_dir.join(format!("hermes-update-{}", binary_name));

    if platform.artifact_name().ends_with(".zip") {
        extract_zip(&archive_path, binary_name, &extracted_path)?;
    } else {
        extract_tar_gz(&archive_path, binary_name, &extracted_path)?;
    }

    // Archive kept for checksum verification; caller cleans up
    Ok((archive_path, extracted_path))
}

fn extract_tar_gz(
    archive_path: &std::path::Path,
    binary_name: &str,
    output_path: &std::path::Path,
) -> Result<(), AgentError> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let file = std::fs::File::open(archive_path)
        .map_err(|e| AgentError::Io(format!("Failed to open archive: {e}")))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    for entry in archive.entries().map_err(|e| AgentError::Io(format!("Failed to read archive: {e}")))? {
        let mut entry = entry.map_err(|e| AgentError::Io(format!("Failed to read entry: {e}")))?;
        let path = entry.path().map_err(|e| AgentError::Io(format!("Invalid entry path: {e}")))?;

        // Match binary by filename (may be nested in a directory)
        if path.file_name().and_then(|n| n.to_str()) == Some(binary_name) {
            let mut out = std::fs::File::create(output_path)
                .map_err(|e| AgentError::Io(format!("Failed to create output file: {e}")))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| AgentError::Io(format!("Failed to extract binary: {e}")))?;
            return Ok(());
        }
    }

    Err(AgentError::Io(format!("Binary '{}' not found in archive", binary_name)))
}

fn extract_zip(
    archive_path: &std::path::Path,
    binary_name: &str,
    output_path: &std::path::Path,
) -> Result<(), AgentError> {
    let file = std::fs::File::open(archive_path)
        .map_err(|e| AgentError::Io(format!("Failed to open zip archive: {e}")))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AgentError::Io(format!("Failed to read zip archive: {e}")))?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)
            .map_err(|e| AgentError::Io(format!("Failed to read zip entry: {e}")))?;

        let entry_name = entry.name().to_string();
        // Match by filename (might be in a subdirectory)
        if entry_name.ends_with(binary_name) || entry_name == binary_name {
            let mut out = std::fs::File::create(output_path)
                .map_err(|e| AgentError::Io(format!("Failed to create output file: {e}")))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| AgentError::Io(format!("Failed to extract binary: {e}")))?;
            return Ok(());
        }
    }

    Err(AgentError::Io(format!("Binary '{}' not found in zip archive", binary_name)))
}
