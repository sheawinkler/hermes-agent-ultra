use std::path::Path;
use std::process::Command;
use hermes_core::errors::AgentError;
use sha2::{Sha256, Digest};

/// 从 checksums 文本中解析指定文件的哈希值（内部辅助函数）
fn parse_checksum_for_file(checksums_text: &str, filename: &str) -> Option<String> {
    checksums_text
        .lines()
        .find_map(|line| {
            let parts: Vec<&str> = line.splitn(2, |c: char| c.is_whitespace()).collect();
            if parts.len() == 2 {
                let entry_filename = parts[1].trim().trim_start_matches('*');
                if entry_filename == filename {
                    return Some(parts[0].to_string())
                }
            }
            None
        })
}

/// 计算数据的 SHA256 哈希
fn compute_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

/// 验证下载的 archive 的 SHA256 校验和
/// archive_path: 下载的 zip/tar.gz 文件路径
/// checksum_url: checksums.sha256 文件 URL
/// expected_filename: archive 文件名（如 hermes-windows-x86_64.zip）
pub async fn verify_checksum(
    archive_path: &Path,
    checksum_url: &str,
    expected_filename: &str,
) -> Result<(), AgentError> {
    // Download checksums file using curl (system TLS)
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sSfL",
        "-H", "User-Agent: hermes-agent-ultra",
    ]);
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
    }
    cmd.arg(checksum_url);

    let output = cmd.output()
        .map_err(|e| AgentError::Io(format!("Failed to run curl for checksums: {e}")))?;

    if !output.status.success() {
        tracing::warn!("Checksums file not available, skipping verification");
        return Ok(());
    }

    let checksums_text = String::from_utf8(output.stdout)
        .map_err(|e| AgentError::Io(format!("Invalid UTF-8 in checksums: {e}")))?;

    let expected_hash = match parse_checksum_for_file(&checksums_text, expected_filename) {
        Some(h) => h,
        None => {
            tracing::warn!("No checksum entry for '{}' in checksums file, skipping verification", expected_filename);
            return Ok(());
        }
    };

    // Compute actual hash of the archive
    let archive_data = std::fs::read(archive_path)
        .map_err(|e| AgentError::Io(format!("Failed to read archive for checksum: {e}")))?;

    let actual_hash = compute_sha256(&archive_data);

    if actual_hash != expected_hash.to_lowercase() {
        let _ = std::fs::remove_file(archive_path);
        return Err(AgentError::Io(format!(
            "SHA256 checksum mismatch!\n  Expected: {}\n  Actual:   {}\nThe downloaded file has been removed for safety.",
            expected_hash, actual_hash
        )));
    }

    tracing::info!("SHA256 checksum verified successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_checksum_standard_format() {
        let checksums = "abc123def456  hermes-linux-x86_64.tar.gz\n789abc  hermes-windows-x86_64.zip\n";
        assert_eq!(
            parse_checksum_for_file(checksums, "hermes-linux-x86_64.tar.gz"),
            Some("abc123def456".to_string())
        );
        assert_eq!(
            parse_checksum_for_file(checksums, "hermes-windows-x86_64.zip"),
            Some("789abc".to_string())
        );
    }

    #[test]
    fn test_parse_checksum_with_star_prefix() {
        let checksums = "abc123  *hermes-linux-x86_64.tar.gz\n";
        assert_eq!(
            parse_checksum_for_file(checksums, "hermes-linux-x86_64.tar.gz"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn test_parse_checksum_not_found() {
        let checksums = "abc123  hermes-linux-x86_64.tar.gz\n";
        assert_eq!(
            parse_checksum_for_file(checksums, "hermes-macos-aarch64.tar.gz"),
            None
        );
    }

    #[test]
    fn test_parse_checksum_empty_input() {
        assert_eq!(parse_checksum_for_file("", "hermes-linux-x86_64.tar.gz"), None);
    }

    #[test]
    fn test_compute_sha256_known_value() {
        // SHA256 of empty string
        let hash = compute_sha256(b"");
        assert_eq!(hash, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn test_compute_sha256_with_data() {
        let hash = compute_sha256(b"hello world");
        assert_eq!(hash, "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
    }

    #[test]
    fn test_verify_checksum_match() {
        let dir = std::env::temp_dir();
        let test_file = dir.join("test_verify_match.bin");
        let data = b"test binary content";
        std::fs::write(&test_file, data).unwrap();

        let expected_hash = compute_sha256(data);
        let checksums_text = format!("{}  test_verify_match.bin\n", expected_hash);

        let parsed = parse_checksum_for_file(&checksums_text, "test_verify_match.bin").unwrap();
        let actual = compute_sha256(&std::fs::read(&test_file).unwrap());
        assert_eq!(parsed, actual);

        let _ = std::fs::remove_file(test_file);
    }

    #[test]
    fn test_verify_checksum_mismatch() {
        let data = b"actual content";
        let actual_hash = compute_sha256(data);
        let fake_hash = "0000000000000000000000000000000000000000000000000000000000000000";
        assert_ne!(actual_hash, fake_hash);
    }

    #[test]
    fn test_sha256_modified_binary_differs() {
        let original_data = b"hermes binary v1.0.0";
        let original_hash = compute_sha256(original_data);

        let mut modified_data = original_data.to_vec();
        modified_data[0] = b'H'; // 修改第一个字节
        let modified_hash = compute_sha256(&modified_data);

        assert_ne!(original_hash, modified_hash);
        // 原始数据的哈希应保持不变
        assert_eq!(compute_sha256(original_data), original_hash);
    }
}
