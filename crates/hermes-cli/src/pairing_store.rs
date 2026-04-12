//! Pairing store — manages `~/.hermes/pairing.json`.
//!
//! Tracks paired devices with pending/approved/revoked status and
//! generates shared secrets on approval.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Persistent store backed by `pairing.json`.
pub struct PairingStore {
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedDevice {
    pub device_id: String,
    pub name: Option<String>,
    pub status: PairingStatus,
    pub created_at: String,
    pub last_seen: Option<String>,
    pub shared_secret: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingStatus {
    Pending,
    Approved,
    Revoked,
}

impl std::fmt::Display for PairingStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PairingStatus::Pending => write!(f, "pending"),
            PairingStatus::Approved => write!(f, "approved"),
            PairingStatus::Revoked => write!(f, "revoked"),
        }
    }
}

impl PairingStore {
    /// Open (or create) the store at the given path.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Default location: `~/.hermes/pairing.json`.
    pub fn default_path() -> PathBuf {
        hermes_config::hermes_home().join("pairing.json")
    }

    /// Open the store at the default location.
    pub fn open_default() -> Self {
        Self::new(Self::default_path())
    }

    /// Load all devices from disk. Returns an empty vec if the file is missing.
    pub fn load(&self) -> Result<Vec<PairedDevice>, String> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let data = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("Failed to read {}: {}", self.path.display(), e))?;
        serde_json::from_str(&data)
            .map_err(|e| format!("Failed to parse {}: {}", self.path.display(), e))
    }

    /// Persist the full device list to disk.
    pub fn save(&self, devices: &[PairedDevice]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create dir: {}", e))?;
        }
        let json = serde_json::to_string_pretty(devices)
            .map_err(|e| format!("Serialization error: {}", e))?;
        std::fs::write(&self.path, json)
            .map_err(|e| format!("Failed to write {}: {}", self.path.display(), e))
    }

    /// List all devices.
    pub fn list(&self) -> Result<Vec<PairedDevice>, String> {
        self.load()
    }

    /// Approve a pending device: set status to `Approved` and generate a shared secret.
    pub fn approve(&self, device_id: &str) -> Result<PairedDevice, String> {
        let mut devices = self.load()?;
        let dev = devices
            .iter_mut()
            .find(|d| d.device_id == device_id)
            .ok_or_else(|| format!("Device '{}' not found", device_id))?;

        if dev.status != PairingStatus::Pending {
            return Err(format!(
                "Device '{}' is not pending (current status: {})",
                device_id, dev.status
            ));
        }

        dev.status = PairingStatus::Approved;
        dev.shared_secret = Some(generate_shared_secret());
        dev.last_seen = Some(chrono::Utc::now().to_rfc3339());

        let result = dev.clone();
        self.save(&devices)?;
        Ok(result)
    }

    /// Revoke an approved device.
    pub fn revoke(&self, device_id: &str) -> Result<PairedDevice, String> {
        let mut devices = self.load()?;
        let dev = devices
            .iter_mut()
            .find(|d| d.device_id == device_id)
            .ok_or_else(|| format!("Device '{}' not found", device_id))?;

        dev.status = PairingStatus::Revoked;
        dev.shared_secret = None;
        dev.last_seen = Some(chrono::Utc::now().to_rfc3339());

        let result = dev.clone();
        self.save(&devices)?;
        Ok(result)
    }

    /// Remove all pending pairing requests. Returns the count removed.
    pub fn clear_pending(&self) -> Result<usize, String> {
        let mut devices = self.load()?;
        let before = devices.len();
        devices.retain(|d| d.status != PairingStatus::Pending);
        let removed = before - devices.len();
        self.save(&devices)?;
        Ok(removed)
    }
}

/// Generate a 32-byte hex shared secret using random bytes from the OS.
fn generate_shared_secret() -> String {
    use std::fmt::Write;
    let mut buf = [0u8; 32];
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut buf);
        }
    }
    #[cfg(not(unix))]
    {
        // Fallback: use timestamp + pid as entropy (not cryptographically strong)
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id() as u128;
        let combined = ts ^ pid;
        buf[..16].copy_from_slice(&combined.to_le_bytes());
        buf[16..].copy_from_slice(&combined.wrapping_mul(6364136223846793005).to_le_bytes());
    }
    let mut hex = String::with_capacity(64);
    for b in &buf {
        let _ = write!(hex, "{:02x}", b);
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_empty() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = PairingStore::new(tmp.path().to_path_buf());
        store.save(&[]).unwrap();
        let loaded = store.load().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_approve_pending_device() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = PairingStore::new(tmp.path().to_path_buf());

        let devices = vec![PairedDevice {
            device_id: "dev-1".into(),
            name: Some("Test Phone".into()),
            status: PairingStatus::Pending,
            created_at: "2025-01-01T00:00:00Z".into(),
            last_seen: None,
            shared_secret: None,
        }];
        store.save(&devices).unwrap();

        let approved = store.approve("dev-1").unwrap();
        assert_eq!(approved.status, PairingStatus::Approved);
        assert!(approved.shared_secret.is_some());
        assert_eq!(approved.shared_secret.unwrap().len(), 64);
    }

    #[test]
    fn test_approve_nonexistent_fails() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = PairingStore::new(tmp.path().to_path_buf());
        store.save(&[]).unwrap();
        assert!(store.approve("no-such-device").is_err());
    }

    #[test]
    fn test_revoke_device() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = PairingStore::new(tmp.path().to_path_buf());

        let devices = vec![PairedDevice {
            device_id: "dev-2".into(),
            name: None,
            status: PairingStatus::Approved,
            created_at: "2025-01-01T00:00:00Z".into(),
            last_seen: None,
            shared_secret: Some("abc123".into()),
        }];
        store.save(&devices).unwrap();

        let revoked = store.revoke("dev-2").unwrap();
        assert_eq!(revoked.status, PairingStatus::Revoked);
        assert!(revoked.shared_secret.is_none());
    }

    #[test]
    fn test_clear_pending() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = PairingStore::new(tmp.path().to_path_buf());

        let devices = vec![
            PairedDevice {
                device_id: "pending-1".into(),
                name: None,
                status: PairingStatus::Pending,
                created_at: "2025-01-01T00:00:00Z".into(),
                last_seen: None,
                shared_secret: None,
            },
            PairedDevice {
                device_id: "approved-1".into(),
                name: None,
                status: PairingStatus::Approved,
                created_at: "2025-01-01T00:00:00Z".into(),
                last_seen: None,
                shared_secret: Some("secret".into()),
            },
        ];
        store.save(&devices).unwrap();

        let removed = store.clear_pending().unwrap();
        assert_eq!(removed, 1);

        let remaining = store.load().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].device_id, "approved-1");
    }

    #[test]
    fn test_generate_shared_secret_length() {
        let s = generate_shared_secret();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
