use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use hermes_core::AgentError;
use hmac::KeyInit as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Provenance signature for an artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ProvenanceSignature {
    pub(crate) generated_at: String,
    pub(crate) algorithm: String,
    pub(crate) key_id: String,
    pub(crate) artifact_sha256: String,
    pub(crate) signature_hex: String,
}

/// Result of verifying a provenance signature.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ProvenanceVerification {
    pub(crate) ok: bool,
    pub(crate) code: String,
    pub(crate) key_id: Option<String>,
    pub(crate) artifact_sha256: Option<String>,
    pub(crate) reason: Option<String>,
}

/// Path to the provenance signing key file under a state root.
pub(crate) fn provenance_key_path_for_cli(state_root: &Path) -> PathBuf {
    state_root.join("auth").join("provenance.key")
}

/// Parse raw provenance key material (hex, base64, or raw bytes).
pub(crate) fn parse_provenance_key_material(raw: &str) -> Result<Vec<u8>, AgentError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(AgentError::Config(
            "empty provenance key material".to_string(),
        ));
    }
    let is_hex = s.len() % 2 == 0 && s.chars().all(|c| c.is_ascii_hexdigit());
    if is_hex {
        return hex::decode(s)
            .map_err(|e| AgentError::Config(format!("decode provenance hex key: {e}")));
    }
    if let Ok(bytes) = BASE64_STANDARD.decode(s.as_bytes()) {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }
    Ok(s.as_bytes().to_vec())
}

/// Load the provenance signing key from env or disk, optionally creating one.
pub(crate) fn load_or_create_provenance_key(
    state_root: &Path,
    allow_create: bool,
) -> Result<Vec<u8>, AgentError> {
    if let Ok(raw_env) = std::env::var("HERMES_PROVENANCE_SIGNING_KEY") {
        let bytes = parse_provenance_key_material(&raw_env)?;
        if bytes.len() < 16 {
            return Err(AgentError::Config(
                "HERMES_PROVENANCE_SIGNING_KEY must be at least 16 bytes".to_string(),
            ));
        }
        return Ok(bytes);
    }

    let path = provenance_key_path_for_cli(state_root);
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
        let bytes = parse_provenance_key_material(&raw)?;
        if bytes.len() < 16 {
            return Err(AgentError::Config(format!(
                "provenance key in {} must be at least 16 bytes",
                path.display()
            )));
        }
        return Ok(bytes);
    }

    if !allow_create {
        return Err(AgentError::Config(format!(
            "provenance key not found at {} (set HERMES_PROVENANCE_SIGNING_KEY or run doctor snapshot/bundle once)",
            path.display()
        )));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let mut key_bytes = [0u8; 32];
    {
        use rand::TryRng;
        rand::rngs::SysRng
            .try_fill_bytes(&mut key_bytes)
            .map_err(|e| AgentError::Config(e.to_string()))?;
    }
    let key_hex = hex::encode(key_bytes);
    std::fs::write(&path, format!("{key_hex}\n"))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)
            .map_err(|e| AgentError::Io(format!("metadata {}: {}", path.display(), e)))?
            .permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(&path, perms);
    }
    Ok(key_bytes.to_vec())
}

/// Sign bytes with the provenance key, returning a signature struct.
pub(crate) fn sign_artifact_bytes(
    state_root: &Path,
    bytes: &[u8],
    allow_create_key: bool,
) -> Result<ProvenanceSignature, AgentError> {
    use hmac::Mac as _;

    let key = load_or_create_provenance_key(state_root, allow_create_key)?;
    let artifact_hash_bytes = Sha256::digest(bytes);
    let artifact_sha256 = hex::encode(artifact_hash_bytes);
    let key_id = {
        let key_hash = Sha256::digest(&key);
        let full = hex::encode(key_hash);
        full.chars().take(16).collect::<String>()
    };
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&key)
        .map_err(|e| AgentError::Config(format!("init provenance hmac: {e}")))?;
    mac.update(artifact_sha256.as_bytes());
    let signature_hex = hex::encode(mac.finalize().into_bytes());
    Ok(ProvenanceSignature {
        generated_at: chrono::Utc::now().to_rfc3339(),
        algorithm: "hmac-sha256".to_string(),
        key_id,
        artifact_sha256,
        signature_hex,
    })
}

/// Compute the sidecar file path for a given artifact path.
pub(crate) fn provenance_sidecar_path_for_artifact(path: &Path) -> PathBuf {
    let filename = path
        .file_name()
        .map(|f| format!("{}.sig.json", f.to_string_lossy()))
        .unwrap_or_else(|| "artifact.sig.json".to_string());
    path.parent()
        .map(|p| p.join(&filename))
        .unwrap_or_else(|| PathBuf::from(filename))
}

/// Write a provenance sidecar JSON file next to the artifact.
pub(crate) fn write_provenance_sidecar(
    path: &Path,
    sig: &ProvenanceSignature,
) -> Result<PathBuf, AgentError> {
    let sidecar = provenance_sidecar_path_for_artifact(path);
    let body = serde_json::to_string_pretty(sig)
        .map_err(|e| AgentError::Config(format!("serialize provenance sidecar: {e}")))?;
    std::fs::write(&sidecar, body)
        .map_err(|e| AgentError::Io(format!("write {}: {}", sidecar.display(), e)))?;
    Ok(sidecar)
}

/// Verify an artifact's provenance signature.
pub(crate) fn verify_artifact_provenance(
    state_root: &Path,
    artifact_path: &Path,
    signature_path: Option<&Path>,
) -> Result<ProvenanceVerification, AgentError> {
    use hmac::Mac as _;

    let bytes = match std::fs::read(artifact_path) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "artifact_read_error".to_string(),
                key_id: None,
                artifact_sha256: None,
                reason: Some(format!("read {}: {}", artifact_path.display(), err)),
            });
        }
    };
    let sidecar_path = signature_path
        .map(PathBuf::from)
        .unwrap_or_else(|| provenance_sidecar_path_for_artifact(artifact_path));
    let sidecar_raw = match std::fs::read_to_string(&sidecar_path) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "signature_read_error".to_string(),
                key_id: None,
                artifact_sha256: None,
                reason: Some(format!("read {}: {}", sidecar_path.display(), err)),
            });
        }
    };
    let sig: ProvenanceSignature = match serde_json::from_str(&sidecar_raw) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "signature_parse_error".to_string(),
                key_id: None,
                artifact_sha256: None,
                reason: Some(format!("parse {}: {}", sidecar_path.display(), err)),
            });
        }
    };
    let key = match load_or_create_provenance_key(state_root, false) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "key_unavailable".to_string(),
                key_id: Some(sig.key_id),
                artifact_sha256: Some(sig.artifact_sha256),
                reason: Some(err.to_string()),
            });
        }
    };
    let artifact_sha = hex::encode(Sha256::digest(&bytes));
    if artifact_sha != sig.artifact_sha256 {
        return Ok(ProvenanceVerification {
            ok: false,
            code: "artifact_sha256_mismatch".to_string(),
            key_id: Some(sig.key_id),
            artifact_sha256: Some(artifact_sha),
            reason: Some("artifact_sha256 mismatch".to_string()),
        });
    }
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&key)
        .map_err(|e| AgentError::Config(format!("init provenance hmac: {e}")))?;
    mac.update(sig.artifact_sha256.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());
    if expected != sig.signature_hex {
        return Ok(ProvenanceVerification {
            ok: false,
            code: "signature_mismatch".to_string(),
            key_id: Some(sig.key_id),
            artifact_sha256: Some(sig.artifact_sha256),
            reason: Some("signature mismatch".to_string()),
        });
    }
    Ok(ProvenanceVerification {
        ok: true,
        code: "ok".to_string(),
        key_id: Some(sig.key_id),
        artifact_sha256: Some(sig.artifact_sha256),
        reason: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use hermes_cli::cli::Cli;
    use hermes_config::state_dir;

    #[test]
    fn provenance_sign_and_verify_round_trip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);
        let state_root = state_dir(cli.config_dir.as_deref().map(Path::new));

        let artifact = tmp.path().join("doctor-snapshot.json");
        let body = b"{\"ok\":true}";
        std::fs::write(&artifact, body).expect("write artifact");

        let sig = sign_artifact_bytes(&state_root, body, true).expect("sign");
        let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");
        let verified = verify_artifact_provenance(&state_root, &artifact, Some(sidecar.as_path()))
            .expect("verify");
        assert!(verified.ok, "verification should pass");
        assert_eq!(verified.code, "ok");
        assert!(verified.reason.is_none(), "no reason on success");
    }

    #[test]
    fn provenance_verify_detects_tampered_artifact() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);
        let state_root = state_dir(cli.config_dir.as_deref().map(Path::new));

        let artifact = tmp.path().join("doctor-snapshot.json");
        let body = b"{\"ok\":true}";
        std::fs::write(&artifact, body).expect("write artifact");
        let sig = sign_artifact_bytes(&state_root, body, true).expect("sign");
        let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");

        std::fs::write(&artifact, b"{\"ok\":false}").expect("tamper artifact");

        let verified = verify_artifact_provenance(&state_root, &artifact, Some(sidecar.as_path()))
            .expect("verify");
        assert!(!verified.ok, "tamper must fail");
        assert_eq!(verified.code, "artifact_sha256_mismatch");
        assert_eq!(verified.reason.as_deref(), Some("artifact_sha256 mismatch"));
    }

    #[test]
    fn provenance_verify_detects_signature_mismatch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);
        let state_root = state_dir(cli.config_dir.as_deref().map(Path::new));

        let artifact = tmp.path().join("doctor-snapshot.json");
        let body = b"{\"ok\":true}";
        std::fs::write(&artifact, body).expect("write artifact");
        let sig = sign_artifact_bytes(&state_root, body, true).expect("sign");
        let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");

        let mut parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).expect("read sidecar"))
                .expect("parse sidecar");
        parsed["signature_hex"] = serde_json::json!("deadbeef");
        std::fs::write(
            &sidecar,
            serde_json::to_string_pretty(&parsed).expect("serialize sidecar"),
        )
        .expect("write tampered sidecar");

        let verified = verify_artifact_provenance(&state_root, &artifact, Some(sidecar.as_path()))
            .expect("verify");
        assert!(!verified.ok, "signature mismatch must fail");
        assert_eq!(verified.code, "signature_mismatch");
        assert_eq!(verified.reason.as_deref(), Some("signature mismatch"));
    }

    #[test]
    fn provenance_verify_detects_missing_sidecar_with_code() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);
        let state_root = state_dir(cli.config_dir.as_deref().map(Path::new));

        let artifact = tmp.path().join("doctor-snapshot.json");
        std::fs::write(&artifact, b"{\"ok\":true}").expect("write artifact");

        let verified = verify_artifact_provenance(&state_root, &artifact, None).expect("verify");
        assert!(!verified.ok, "missing sidecar must fail");
        assert_eq!(verified.code, "signature_read_error");
        assert!(
            verified
                .reason
                .as_deref()
                .unwrap_or("")
                .contains(".sig.json")
        );
    }

    #[tokio::test]
    async fn rotate_provenance_key_archives_previous_key_and_rekeys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);
        let state_root = state_dir(cli.config_dir.as_deref().map(Path::new));

        let old_key = load_or_create_provenance_key(&state_root, true).expect("create key");
        crate::misc_main::run_rotate_provenance_key(cli.clone(), true)
            .await
            .expect("rotate key");
        let new_key = load_or_create_provenance_key(&state_root, false).expect("load rotated key");
        assert_ne!(old_key, new_key, "rotation must change active key bytes");

        let auth_dir = provenance_key_path_for_cli(&state_root)
            .parent()
            .expect("key path parent")
            .to_path_buf();
        let archived_count = std::fs::read_dir(auth_dir)
            .expect("read auth dir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("provenance.key.")
                    && entry.file_name().to_string_lossy().ends_with(".bak")
            })
            .count();
        assert!(archived_count >= 1, "rotation should archive previous key");
    }
}
