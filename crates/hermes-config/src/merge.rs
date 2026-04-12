//! Deep merge logic for configuration values.
//!
//! Priority order (highest wins):
//!   1. Environment variables
//!   2. config.yaml
//!   3. gateway.json
//!   4. Built-in defaults

use serde_json::Value;

use crate::config::GatewayConfig;

// ---------------------------------------------------------------------------
// deep_merge
// ---------------------------------------------------------------------------

/// Recursively merge `overlay` into `base`.
///
/// - Objects are merged key-by-key; keys present in `overlay` replace those in `base`.
/// - All other types (arrays, strings, numbers, etc.) are overwritten by `overlay`.
/// - `null` values in `overlay` cause the corresponding key to be **removed** from `base`.
pub fn deep_merge(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            for (key, overlay_val) in overlay_map {
                if overlay_val.is_null() {
                    // Remove key on null overlay
                    base_map.remove(key);
                } else if let Some(base_val) = base_map.get_mut(key) {
                    deep_merge(base_val, overlay_val);
                } else {
                    base_map.insert(key.clone(), overlay_val.clone());
                }
            }
        }
        // All non-object overlays simply replace the base value
        (base, overlay) => {
            *base = overlay.clone();
        }
    }
}

// ---------------------------------------------------------------------------
// merge_configs
// ---------------------------------------------------------------------------

/// Merge a YAML-sourced config with a JSON-sourced config.
///
/// The `yaml_config` takes priority over `json_config`. The result
/// is a new GatewayConfig where yaml values override json values
/// at every level.
///
/// After the merge, validation is applied (e.g. DailyReset at_hour clamping).
pub fn merge_configs(yaml_config: &GatewayConfig, json_config: &GatewayConfig) -> GatewayConfig {
    // Serialize both to JSON values, deep-merge yaml over json, then deserialize.
    let mut base = serde_json::to_value(json_config).unwrap_or(Value::Null);
    let overlay = serde_json::to_value(yaml_config).unwrap_or(Value::Null);

    if !overlay.is_null() {
        deep_merge(&mut base, &overlay);
    }

    // Deserialize back; fall back to yaml_config on error
    let merged: GatewayConfig = match serde_json::from_value(base) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to deserialize merged config: {e}; falling back to yaml");
            yaml_config.clone()
        }
    };

    // Validate session reset policies
    let mut merged = merged;
    merged.session.reset_policy = merged.session.reset_policy.validate();
    for (_platform, policy) in merged.session.platform_overrides.iter_mut() {
        *policy = policy.validate();
    }
    for (_stype, policy) in merged.session.session_type_overrides.iter_mut() {
        *policy = policy.validate();
    }

    merged
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_merge_objects() {
        let mut base = serde_json::json!({
            "a": 1,
            "b": 2,
            "c": { "x": 10, "y": 20 }
        });
        let overlay = serde_json::json!({
            "b": 99,
            "c": { "y": 99, "z": 30 }
        });
        deep_merge(&mut base, &overlay);
        assert_eq!(base["a"], 1); // kept
        assert_eq!(base["b"], 99); // overridden
        assert_eq!(base["c"]["x"], 10); // kept
        assert_eq!(base["c"]["y"], 99); // overridden
        assert_eq!(base["c"]["z"], 30); // added
    }

    #[test]
    fn deep_merge_null_removes_key() {
        let mut base = serde_json::json!({
            "a": 1,
            "b": 2
        });
        let overlay = serde_json::json!({
            "b": serde_json::Value::Null
        });
        deep_merge(&mut base, &overlay);
        assert_eq!(base["a"], 1);
        assert!(base.get("b").is_none());
    }

    #[test]
    fn deep_merge_array_replaces() {
        let mut base = serde_json::json!({
            "items": [1, 2, 3]
        });
        let overlay = serde_json::json!({
            "items": [4, 5]
        });
        deep_merge(&mut base, &overlay);
        assert_eq!(base["items"], serde_json::json!([4, 5]));
    }

    #[test]
    fn merge_configs_combines() {
        let mut json_cfg = GatewayConfig::default();
        json_cfg.model = Some("gpt-3.5-turbo".into());
        json_cfg.max_turns = 10;

        let mut yaml_cfg = GatewayConfig::default();
        yaml_cfg.model = Some("gpt-4o".into());

        let merged = merge_configs(&yaml_cfg, &json_cfg);
        // yaml wins for model
        assert_eq!(merged.model.as_deref(), Some("gpt-4o"));
        // yaml's default max_turns (30) overrides json's 10 since yaml is primary
        assert_eq!(merged.max_turns, 30);
    }
}
