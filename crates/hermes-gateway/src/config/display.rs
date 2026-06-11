//! Per-platform display / verbosity settings (parity with Python `gateway.display_config`).

use serde_json::Value;

/// Built-in platform tier defaults: `(platform, key, value)`.
/// `streaming` uses empty string sentinel → [`None`] (follow global).
const PLATFORM_DEFAULTS: &[(&str, &str, &str)] = &[
    ("whatsapp", "tool_progress", "off"),
    ("whatsapp", "streaming", ""),
    ("telegram", "tool_progress", "all"),
    ("email", "tool_progress", "off"),
];

const GLOBAL_DEFAULTS: &[(&str, &str)] = &[("tool_progress", "all")];

/// Resolve a display setting for a platform.
///
/// Priority: `display.platforms.<plat>.<key>` → `display.<key>` → platform tier default
/// → global default → `fallback`.
pub fn resolve_display_setting(
    config: Option<&Value>,
    platform: &str,
    key: &str,
    fallback: Option<&str>,
) -> Option<String> {
    let plat = platform.trim().to_lowercase();
    let key = key.trim();

    if let Some(cfg) = config {
        if let Some(v) = cfg
            .get("display")
            .and_then(|d| d.get("platforms"))
            .and_then(|p| p.get(&plat))
            .and_then(|p| p.get(key))
            .and_then(value_as_display)
        {
            return Some(v);
        }
        if let Some(v) = cfg
            .get("display")
            .and_then(|d| d.get(key))
            .and_then(value_as_display)
        {
            return Some(v);
        }
    }

    for (p, k, v) in PLATFORM_DEFAULTS {
        if *p == plat && *k == key {
            if v.is_empty() {
                return None;
            }
            return Some((*v).to_string());
        }
    }

    for (k, v) in GLOBAL_DEFAULTS {
        if *k == key {
            return Some((*v).to_string());
        }
    }

    fallback.map(str::to_string)
}

fn value_as_display(v: &Value) -> Option<String> {
    if v.is_null() {
        return None;
    }
    v.as_str().map(str::to_string)
}
