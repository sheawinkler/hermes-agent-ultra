//! Configurable tool-output truncation limits.
//!
//! Corresponds to `hermes-agent/tools/tool_output_limits.py`.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde_yaml::Value as YamlValue;

pub const DEFAULT_MAX_BYTES: usize = 50_000;
pub const DEFAULT_MAX_LINES: usize = 2_000;
pub const DEFAULT_MAX_LINE_LENGTH: usize = 2_000;

static CACHED_LIMITS: LazyLock<Mutex<Option<ToolOutputLimits>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolOutputLimits {
    pub max_bytes: usize,
    pub max_lines: usize,
    pub max_line_length: usize,
}

impl Default for ToolOutputLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_lines: DEFAULT_MAX_LINES,
            max_line_length: DEFAULT_MAX_LINE_LENGTH,
        }
    }
}

pub fn coerce_positive_int(value: Option<&YamlValue>, default: usize) -> usize {
    let Some(value) = value else {
        return default;
    };

    let parsed = match value {
        YamlValue::Number(n) => n.as_i64(),
        YamlValue::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    };

    match parsed {
        Some(n) if n > 0 => n as usize,
        _ => default,
    }
}

pub fn limits_from_config_root(root: &YamlValue) -> ToolOutputLimits {
    let Some(section) = root.get("tool_output").and_then(YamlValue::as_mapping) else {
        return ToolOutputLimits::default();
    };

    let get = |key: &str| section.get(YamlValue::String(key.to_string()));
    ToolOutputLimits {
        max_bytes: coerce_positive_int(get("max_bytes"), DEFAULT_MAX_BYTES),
        max_lines: coerce_positive_int(get("max_lines"), DEFAULT_MAX_LINES),
        max_line_length: coerce_positive_int(get("max_line_length"), DEFAULT_MAX_LINE_LENGTH),
    }
}

pub fn get_tool_output_limits() -> ToolOutputLimits {
    let mut cached = CACHED_LIMITS
        .lock()
        .expect("tool output limits cache lock poisoned");
    if let Some(limits) = *cached {
        return limits;
    }

    let limits = read_config_root()
        .as_ref()
        .map(limits_from_config_root)
        .unwrap_or_default();
    *cached = Some(limits);
    limits
}

pub fn reset_tool_output_limits_cache() {
    let mut cached = CACHED_LIMITS
        .lock()
        .expect("tool output limits cache lock poisoned");
    *cached = None;
}

pub fn get_max_bytes() -> usize {
    get_tool_output_limits().max_bytes
}

pub fn get_max_lines() -> usize {
    get_tool_output_limits().max_lines
}

pub fn get_max_line_length() -> usize {
    get_tool_output_limits().max_line_length
}

fn read_config_root() -> Option<YamlValue> {
    read_config_root_from_home(&hermes_home())
}

fn read_config_root_from_home(home: &Path) -> Option<YamlValue> {
    let path = home.join("config.yaml");
    let raw = std::fs::read_to_string(path).ok()?;
    serde_yaml::from_str(&raw).ok()
}

fn hermes_home() -> PathBuf {
    std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".hermes")))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".hermes")))
        .unwrap_or_else(|| PathBuf::from(".hermes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_match_python() {
        assert_eq!(ToolOutputLimits::default().max_bytes, 50_000);
        assert_eq!(ToolOutputLimits::default().max_lines, 2_000);
        assert_eq!(ToolOutputLimits::default().max_line_length, 2_000);
    }

    #[test]
    fn coerce_positive_int_matches_python_fallbacks() {
        assert_eq!(
            coerce_positive_int(Some(&YamlValue::Number(123.into())), 10),
            123
        );
        assert_eq!(
            coerce_positive_int(Some(&YamlValue::String("456".to_string())), 10),
            456
        );
        assert_eq!(
            coerce_positive_int(Some(&YamlValue::String("nope".to_string())), 10),
            10
        );
        assert_eq!(
            coerce_positive_int(Some(&YamlValue::Number(0.into())), 10),
            10
        );
        assert_eq!(
            coerce_positive_int(Some(&YamlValue::Number((-1).into())), 10),
            10
        );
    }

    #[test]
    fn limits_from_config_root_reads_tool_output_section() {
        let root: YamlValue = serde_yaml::from_str(
            r#"
tool_output:
  max_bytes: 100000
  max_lines: "5000"
  max_line_length: -1
"#,
        )
        .expect("yaml");

        let limits = limits_from_config_root(&root);
        assert_eq!(limits.max_bytes, 100_000);
        assert_eq!(limits.max_lines, 5_000);
        assert_eq!(limits.max_line_length, DEFAULT_MAX_LINE_LENGTH);
    }

    #[test]
    fn read_config_root_from_home_reads_config_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("config.yaml"),
            "tool_output:\n  max_bytes: 777\n  max_lines: 888\n  max_line_length: 999\n",
        )
        .expect("write config");

        let root = read_config_root_from_home(temp.path()).expect("config root");
        let limits = limits_from_config_root(&root);
        assert_eq!(
            limits,
            ToolOutputLimits {
                max_bytes: 777,
                max_lines: 888,
                max_line_length: 999,
            }
        );
    }
}
