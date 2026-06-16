//! Device fingerprint collection for activation reporting.

use std::process::Command;

use sha2::{Digest, Sha256};
use tracing::warn;

use crate::error::ServerClientError;
use crate::flowy::DeviceActivateRequest;
use crate::platform;

#[derive(Debug, Clone)]
pub struct DeviceFingerprint {
    pub mac: String,
    pub sn: String,
    pub cpu_chip_id: String,
}

pub fn collect_fingerprint(
    persisted_sn: Option<&str>,
) -> Result<DeviceFingerprint, ServerClientError> {
    let mac = read_mac_address().unwrap_or_else(|| {
        warn!("could not read MAC address; using generated placeholder");
        "00:00:00:00:00:01".to_string()
    });
    let sn = if let Some(sn) = persisted_sn {
        sn.to_string()
    } else {
        read_serial_number().unwrap_or_else(generate_serial_number)
    };
    let cpu_chip_id = read_cpu_chip_id().unwrap_or_else(|| {
        warn!("could not read CPU chip id; using hashed fallback");
        hash_cpu_fallback("unknown-cpu")
    });
    Ok(DeviceFingerprint {
        mac: normalize_mac(&mac),
        sn,
        cpu_chip_id,
    })
}

pub fn build_activate_request(
    channel: &str,
    fingerprint: &DeviceFingerprint,
) -> DeviceActivateRequest {
    DeviceActivateRequest {
        channel: channel.to_string(),
        mac: fingerprint.mac.clone(),
        sn: fingerprint.sn.clone(),
        activate_timestamp: chrono::Utc::now().timestamp_millis(),
        cpu_chip_id: fingerprint.cpu_chip_id.clone(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        os_version: platform::os_version_string(),
        xpu_brand: None,
        public_ip: String::new(),
        country_code: String::new(),
        postal: "0".to_string(),
        latitude: "0".to_string(),
        longitude: "0".to_string(),
        isp: String::new(),
        timezone: String::new(),
        currency: String::new(),
    }
}

fn normalize_mac(raw: &str) -> String {
    raw.trim().replace('-', ":").to_ascii_uppercase()
}

fn generate_serial_number() -> String {
    let suffix = uuid::Uuid::new_v4().to_string().replace('-', "");
    format!(
        "CLAWSN{}{}",
        chrono::Utc::now().timestamp_millis(),
        &suffix[..8.min(suffix.len())]
    )
}

fn hash_cpu_fallback(model: &str) -> String {
    let digest = Sha256::digest(model.as_bytes());
    format!("CPU{}", hex::encode(&digest[..8]).to_ascii_uppercase())
}

fn read_mac_address() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        run_powershell(
            "Get-NetAdapter | Where-Object { $_.Status -eq 'Up' -and $_.MacAddress -ne $null } | Select-Object -First 1 -ExpandProperty MacAddress",
        )
    }
    #[cfg(target_os = "linux")]
    {
        read_file_trim("/sys/class/net/eth0/address")
            .or_else(|| read_file_trim("/sys/class/net/en0/address"))
    }
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("ifconfig").arg("en0").output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("ether ") {
                return Some(rest.split_whitespace().next()?.to_string());
            }
        }
        None
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn read_serial_number() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        run_powershell("(Get-CimInstance Win32_BIOS).SerialNumber")
    }
    #[cfg(target_os = "linux")]
    {
        read_file_trim("/sys/class/dmi/id/product_serial")
    }
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("system_profiler")
            .args(["SPHardwareDataType"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("Serial Number") {
                return line.split(':').nth(1).map(|s| s.trim().to_string());
            }
        }
        None
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn read_cpu_chip_id() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        run_powershell("(Get-CimInstance Win32_Processor | Select-Object -First 1).ProcessorId")
    }
    #[cfg(target_os = "linux")]
    {
        let model = std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|s| s.trim().to_string())
            })?;
        return Some(hash_cpu_fallback(&model));
    }
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()?;
        let model = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if model.is_empty() {
            None
        } else {
            Some(hash_cpu_fallback(&model))
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "windows")]
fn run_powershell(script: &str) -> Option<String> {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(not(target_os = "windows"))]
fn run_powershell(_script: &str) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn read_file_trim(path: &str) -> Option<String> {
    let value = std::fs::read_to_string(path).ok()?.trim().to_string();
    if value.is_empty() || value.eq_ignore_ascii_case("none") {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_mac_replaces_dashes() {
        assert_eq!(normalize_mac("aa-bb-cc-dd-ee-ff"), "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn collect_fingerprint_with_persisted_sn() {
        let fp = collect_fingerprint(Some("SN123")).expect("fingerprint");
        assert_eq!(fp.sn, "SN123");
        assert!(!fp.mac.is_empty());
        assert!(!fp.cpu_chip_id.is_empty());
    }
}
