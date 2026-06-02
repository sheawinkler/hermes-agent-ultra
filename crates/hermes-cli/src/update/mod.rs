pub mod platform;
pub mod github;
pub mod download;
pub mod verify;
pub mod replace;
pub mod modelscope;
pub mod probe;

use hermes_core::errors::AgentError;
use crate::update::platform::Platform;

/// 更新选项
pub struct UpdateOptions {
    pub yes: bool,
    pub force: bool,
    pub source: Option<String>,
}

/// 检查是否有更新可用（兼容旧接口）
pub async fn check_for_updates() -> Result<String, AgentError> {
    let platform = Platform::detect()?;
    let source = probe::select_fastest_source(None).await;

    let info = source.fetch_latest(&platform).await?;
    let current = env!("CARGO_PKG_VERSION");
    let current_normalized = current.trim_start_matches('v');

    if info.version == current_normalized {
        Ok(format!("Already up to date (v{current_normalized})."))
    } else {
        let mut msg = format!(
            "New version available: v{} (current: v{current_normalized})\nRun `hermes update` to upgrade.",
            info.version
        );
        if let Some(notes) = &info.release_notes {
            // Show first 5 lines of release notes
            let preview: String = notes.lines().take(5).collect::<Vec<_>>().join("\n");
            msg.push_str(&format!("\n\nRelease notes:\n{preview}"));
        }
        Ok(msg)
    }
}

/// 执行完整的 OTA 更新流程
pub async fn perform_update(opts: UpdateOptions) -> Result<(), AgentError> {
    // 1. Detect platform
    let platform = Platform::detect()?;
    println!("Platform: {}-{}", platform.os, platform.arch);

    // 2. Fetch latest release info
    let source = probe::select_fastest_source(opts.source.as_deref()).await;
    println!("Checking for updates from {}...", source.name());
    let info = source.fetch_latest(&platform).await?;

    // 3. Version comparison
    let current = env!("CARGO_PKG_VERSION").trim_start_matches('v');
    if !opts.force && info.version == current {
        println!("Already up to date (v{current}).");
        return Ok(());
    }

    println!("Current version: v{current}");
    println!("Latest version:  v{}", info.version);

    if let Some(ref notes) = info.release_notes {
        let preview: String = notes.lines().take(10).collect::<Vec<_>>().join("\n");
        println!("\nRelease notes:\n{preview}\n");
    }

    // 4. Confirm (unless -y)
    if !opts.yes {
        println!("Proceed with update? [y/N] ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)
            .map_err(|e| AgentError::Io(format!("Failed to read input: {e}")))?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Update cancelled.");
            return Ok(());
        }
    }

    // 5. Download and extract
    let (archive_path, new_binary) = download::download_and_extract(
        &info.artifact_url,
        &platform,
        true, // show progress
    ).await?;

    // 6. Verify checksum (on archive, not extracted binary)
    if let Some(ref checksum_url) = info.checksum_url {
        verify::verify_checksum(&archive_path, checksum_url, &platform.artifact_name()).await?;
    } else {
        tracing::warn!("No checksums.sha256 available for this release, skipping verification");
    }

    // Cleanup archive
    let _ = std::fs::remove_file(&archive_path);

    // 7. Self-replace
    replace::self_replace(&new_binary)?;

    // Cleanup temp file
    let _ = std::fs::remove_file(&new_binary);

    // 8. Success message
    println!("\nSuccessfully updated to v{}!", info.version);
    if cfg!(windows) {
        println!("Please restart hermes for the update to take effect.");
    }

    Ok(())
}
