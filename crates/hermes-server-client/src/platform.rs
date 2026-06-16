//! Client platform string for Flowy API payloads.

pub fn client_platform() -> String {
    if cfg!(target_os = "windows") {
        "windows".to_string()
    } else if cfg!(target_os = "macos") {
        "mac".to_string()
    } else if cfg!(target_os = "linux") {
        "linux".to_string()
    } else {
        "unknown".to_string()
    }
}

pub fn os_version_string() -> String {
    if cfg!(target_os = "windows") {
        format!("Windows_NT {}", windows_release_hint())
    } else if cfg!(target_os = "macos") {
        format!("Darwin {}", std::env::consts::ARCH)
    } else if cfg!(target_os = "linux") {
        format!("Linux {}", std::env::consts::ARCH)
    } else {
        std::env::consts::OS.to_string()
    }
}

#[cfg(target_os = "windows")]
fn windows_release_hint() -> String {
    use std::process::Command;
    Command::new("cmd")
        .args(["/C", "ver"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(not(target_os = "windows"))]
fn windows_release_hint() -> String {
    String::new()
}
