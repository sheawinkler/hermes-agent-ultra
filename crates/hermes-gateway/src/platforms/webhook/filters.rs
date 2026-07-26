//! Declarative route filters and bounded script transforms for webhooks.
//!
//! Route scripts are intentionally constrained to `$HERMES_HOME/scripts` and
//! inherit Hermes' credential-sanitized subprocess environment. A malformed,
//! failed, or timed-out script rejects its webhook rather than delivering an
//! unreviewed payload.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, warn};

pub(crate) const DEFAULT_SCRIPT_TIMEOUT_SECONDS: u64 = 30;

pub(crate) const fn default_script_timeout_seconds() -> u64 {
    DEFAULT_SCRIPT_TIMEOUT_SECONDS
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RoutePayloadDecision {
    Continue(Value),
    IgnoredFilter,
    IgnoredScript,
}

#[derive(Debug, Clone)]
pub(crate) struct WebhookRouteProcessor {
    home: PathBuf,
    script_timeout: Duration,
}

impl WebhookRouteProcessor {
    pub(crate) fn for_config(script_timeout_seconds: u64) -> Self {
        Self::with_home(hermes_config::hermes_home(), script_timeout_seconds)
    }

    fn with_home(home: PathBuf, script_timeout_seconds: u64) -> Self {
        Self {
            home,
            script_timeout: Duration::from_secs(script_timeout_seconds.max(1)),
        }
    }

    pub(crate) async fn apply(
        &self,
        filters: &Value,
        script: Option<&str>,
        payload: Value,
        event_type: &str,
        headers: &HashMap<String, String>,
    ) -> RoutePayloadDecision {
        if !self.route_filters_match(filters, &payload, event_type, headers) {
            return RoutePayloadDecision::IgnoredFilter;
        }

        let Some(script) = script.map(str::trim).filter(|script| !script.is_empty()) else {
            return RoutePayloadDecision::Continue(payload);
        };

        match self.run_route_script(script, &payload).await {
            Some(transformed) => RoutePayloadDecision::Continue(transformed),
            None => RoutePayloadDecision::IgnoredScript,
        }
    }

    fn route_filters_match(
        &self,
        filters: &Value,
        payload: &Value,
        event_type: &str,
        headers: &HashMap<String, String>,
    ) -> bool {
        match filters {
            Value::Null | Value::Bool(false) => true,
            Value::Array(items) if items.is_empty() => true,
            Value::Object(items) if items.is_empty() => true,
            Value::String(value) if value.is_empty() => true,
            Value::Number(value) if value.as_i64() == Some(0) || value.as_u64() == Some(0) => {
                true
            }
            Value::Object(_) => self.filter_matches(filters, payload, event_type, headers),
            Value::Array(items) => items
                .iter()
                .all(|filter| self.filter_matches(filter, payload, event_type, headers)),
            _ => {
                warn!("webhook filters must be a list or object");
                false
            }
        }
    }

    fn filter_matches(
        &self,
        spec: &Value,
        payload: &Value,
        event_type: &str,
        headers: &HashMap<String, String>,
    ) -> bool {
        let Some(spec) = spec.as_object() else {
            warn!(filter = ?spec, "ignoring invalid webhook filter specification");
            return false;
        };

        if let Some(items) = spec.get("all") {
            return items.as_array().is_some_and(|items| {
                items
                    .iter()
                    .all(|item| self.filter_matches(item, payload, event_type, headers))
            });
        }
        if let Some(items) = spec.get("any") {
            return items.as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| self.filter_matches(item, payload, event_type, headers))
            });
        }
        if let Some(item) = spec.get("not") {
            return !self.filter_matches(item, payload, event_type, headers);
        }

        let value = resolve_filter_field(spec.get("field"), payload, event_type, headers);
        if let Some(expected) = spec.get("exists") {
            return value.is_present() == json_truthy(expected);
        }
        if spec.get("missing") == Some(&Value::Bool(true)) {
            return !value.is_present();
        }
        if let Some(expected) = spec.get("equals") {
            return value.equals(expected);
        }
        if let Some(expected) = spec.get("not_equals") {
            return !value.is_present() || !value.equals(expected);
        }
        if let Some(needle) = spec.get("contains") {
            return value.contains(needle);
        }
        if let Some(haystack) = spec.get("in") {
            return haystack
                .as_array()
                .is_some_and(|items| items.iter().any(|item| value.equals(item)));
        }
        if let Some(path) = spec.get("in_file") {
            return load_filter_file_values(&self.home, path)
                .iter()
                .any(|item| value.equals(item));
        }
        if let Some(pattern) = spec.get("regex") {
            let Some(rendered) = value.stringify() else {
                return false;
            };
            let Some(pattern) = pattern.as_str() else {
                warn!("invalid webhook filter regex");
                return false;
            };
            return match regex::Regex::new(pattern) {
                Ok(regex) => regex.is_match(&rendered),
                Err(error) => {
                    warn!(%error, "invalid webhook filter regex");
                    false
                }
            };
        }

        warn!(filter = ?spec, "webhook filter has no supported operator");
        false
    }

    async fn run_route_script(&self, script_value: &str, payload: &Value) -> Option<Value> {
        let path = match self.resolve_script_path(script_value) {
            Ok(path) => path,
            Err(error) => {
                warn!(%error, "webhook script ignored payload");
                return None;
            }
        };
        let input = match serde_json::to_vec(payload) {
            Ok(input) => input,
            Err(error) => {
                warn!(%error, path = %path.display(), "unable to encode webhook script input");
                return None;
            }
        };

        let mut command = script_command(&path);
        command
            .current_dir(path.parent().unwrap_or(&self.home))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear()
            .envs(hermes_core::subprocess_env::hermes_subprocess_env(false));
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                warn!(%error, path = %path.display(), "webhook script could not start");
                return None;
            }
        };
        let Some(mut stdin) = child.stdin.take() else {
            warn!(path = %path.display(), "webhook script has no stdin");
            return None;
        };
        if let Err(error) = stdin.write_all(&input).await {
            warn!(%error, path = %path.display(), "unable to write webhook script input");
            return None;
        }
        if let Err(error) = stdin.shutdown().await {
            warn!(%error, path = %path.display(), "unable to close webhook script stdin");
            return None;
        }
        drop(stdin);

        let output = match tokio::time::timeout(self.script_timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                warn!(%error, path = %path.display(), "webhook script wait failed");
                return None;
            }
            Err(_) => {
                warn!(timeout_secs = self.script_timeout.as_secs(), path = %path.display(), "webhook script timed out");
                return None;
            }
        };
        if !output.status.success() {
            debug!(
                path = %path.display(),
                code = ?output.status.code(),
                "webhook script ignored payload after nonzero exit"
            );
            return None;
        }

        let stdout = hermes_intelligence::redact_sensitive_text(
            String::from_utf8_lossy(&output.stdout).trim(),
        );
        if stdout.is_empty() || stdout == "[SILENT]" {
            return None;
        }

        let transformed = match serde_json::from_str::<Value>(&stdout) {
            Ok(Value::Object(value)) => Value::Object(value),
            Ok(_) => {
                warn!(path = %path.display(), "webhook script stdout must be a JSON object or text");
                return None;
            }
            Err(_) => {
                let Value::Object(mut transformed) = payload.clone() else {
                    warn!(path = %path.display(), "webhook script text output requires an object payload");
                    return None;
                };
                transformed.insert("script_output".to_string(), Value::String(stdout));
                Value::Object(transformed)
            }
        };
        if transformed
            .get("[SILENT]")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || transformed
                .get("__hermes_ignore__")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return None;
        }
        Some(transformed)
    }

    fn resolve_script_path(&self, script_value: &str) -> Result<PathBuf, String> {
        let raw = expand_environment_variables(script_value.trim());
        if raw.is_empty() {
            return Err("script path is empty".to_string());
        }
        let scripts_root = self.home.join("scripts");
        let root = std::fs::canonicalize(&scripts_root)
            .map_err(|_| format!("script directory is unavailable: {}", scripts_root.display()))?;
        let candidate = if raw == "~/.hermes" || raw.starts_with("~/.hermes/") {
            resolve_profile_path(&self.home, &raw).unwrap_or_else(|| scripts_root.clone())
        } else {
            let raw_path = PathBuf::from(raw);
            if raw_path.is_absolute() {
                raw_path
            } else {
                scripts_root.join(raw_path)
            }
        };
        let candidate = std::fs::canonicalize(&candidate)
            .map_err(|_| format!("script not found: {}", candidate.display()))?;
        if !candidate.starts_with(&root) {
            return Err(format!("script path resolves outside {}", root.display()));
        }
        let metadata = std::fs::metadata(&candidate)
            .map_err(|_| format!("script not found: {}", candidate.display()))?;
        if !metadata.is_file() {
            return Err(format!("script path is not a file: {}", candidate.display()));
        }
        Ok(candidate)
    }
}

fn script_command(path: &Path) -> Command {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());
    let executable = match extension.as_deref() {
        Some("sh" | "bash") => "bash",
        _ if cfg!(windows) => "python",
        _ => "python3",
    };
    let mut command = Command::new(executable);
    command.arg(path);
    command
}

#[derive(Clone, Copy)]
enum FilterValue<'a> {
    Missing,
    Json(&'a Value),
    Text(&'a str),
}

impl FilterValue<'_> {
    fn is_present(self) -> bool {
        !matches!(self, Self::Missing)
    }

    fn equals(self, expected: &Value) -> bool {
        match self {
            Self::Missing => false,
            Self::Json(value) => value == expected,
            Self::Text(value) => expected.as_str() == Some(value),
        }
    }

    fn contains(self, needle: &Value) -> bool {
        match self {
            Self::Missing => false,
            Self::Text(value) => needle
                .as_str()
                .is_some_and(|needle| value.contains(needle)),
            Self::Json(Value::Array(items)) => items.iter().any(|item| item == needle),
            Self::Json(Value::Object(items)) => needle
                .as_str()
                .is_some_and(|needle| items.contains_key(needle)),
            Self::Json(value) => value
                .as_str()
                .map(|value| stringify_filter_value(&Value::String(value.to_string())))
                .is_some_and(|value| {
                    needle
                        .as_str()
                        .map_or_else(|| value.contains(&stringify_filter_value(needle)), |needle| value.contains(needle))
                }),
        }
    }

    fn stringify(self) -> Option<String> {
        match self {
            Self::Missing => None,
            Self::Text(value) => Some(value.to_string()),
            Self::Json(value) => Some(stringify_filter_value(value)),
        }
    }
}

fn resolve_filter_field<'a>(
    field: Option<&Value>,
    payload: &'a Value,
    event_type: &'a str,
    headers: &'a HashMap<String, String>,
) -> FilterValue<'a> {
    let Some(field) = field.and_then(Value::as_str).map(str::trim).filter(|field| !field.is_empty()) else {
        return FilterValue::Missing;
    };
    let mut parts = field.split('.').filter(|part| !part.is_empty());
    let Some(first) = parts.next() else {
        return FilterValue::Missing;
    };
    let remaining: Vec<&str> = parts.collect();

    match first {
        "event" | "event_type" => {
            if remaining.is_empty() {
                FilterValue::Text(event_type)
            } else {
                FilterValue::Missing
            }
        }
        "headers" => {
            let Some(header) = remaining.first() else {
                return FilterValue::Missing;
            };
            if remaining.len() != 1 {
                return FilterValue::Missing;
            }
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(header))
                .map(|(_, value)| FilterValue::Text(value))
                .unwrap_or(FilterValue::Missing)
        }
        "payload" => traverse_filter_value(payload.get("payload").unwrap_or(payload), &remaining),
        _ => {
            let mut all_parts = Vec::with_capacity(remaining.len() + 1);
            all_parts.push(first);
            all_parts.extend(remaining);
            traverse_filter_value(payload, &all_parts)
        }
    }
}

fn traverse_filter_value<'a>(mut value: &'a Value, parts: &[&str]) -> FilterValue<'a> {
    for part in parts {
        value = match value {
            Value::Object(values) => match values.get(*part) {
                Some(value) => value,
                None => return FilterValue::Missing,
            },
            Value::Array(values) => match part.parse::<usize>().ok().and_then(|index| values.get(index)) {
                Some(value) => value,
                None => return FilterValue::Missing,
            },
            _ => return FilterValue::Missing,
        };
    }
    FilterValue::Json(value)
}

fn json_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn stringify_filter_value(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let mut output = String::from("[");
            for (index, item) in items.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str(&stringify_filter_value(item));
            }
            output.push(']');
            output
        }
        Value::Object(items) => stringify_sorted_object(items),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn stringify_sorted_object(items: &Map<String, Value>) -> String {
    let mut keys: Vec<_> = items.keys().collect();
    keys.sort_unstable();
    let mut output = String::from("{");
    for (index, key) in keys.into_iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        let _ = write!(output, "{}:{}", serde_json::to_string(key).unwrap_or_default(), stringify_filter_value(&items[key]));
    }
    output.push('}');
    output
}

fn expand_environment_variables(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            let ch = input[index..].chars().next().expect("valid UTF-8");
            output.push(ch);
            index += ch.len_utf8();
            continue;
        }
        let start = index;
        index += 1;
        let (name, end) = if bytes.get(index) == Some(&b'{') {
            let name_start = index + 1;
            match input[name_start..].find('}') {
                Some(offset) => (&input[name_start..name_start + offset], name_start + offset + 1),
                None => {
                    output.push('$');
                    continue;
                }
            }
        } else {
            let name_start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            (&input[name_start..index], index)
        };
        if name.is_empty() {
            output.push('$');
            continue;
        }
        match std::env::var(name) {
            Ok(value) => output.push_str(&value),
            Err(_) => output.push_str(&input[start..end]),
        }
        index = end;
    }
    output
}

fn resolve_profile_path(home: &Path, path_value: &str) -> Option<PathBuf> {
    let raw = expand_environment_variables(path_value.trim());
    if raw.is_empty() {
        return None;
    }
    if raw == "~/.hermes" {
        return Some(home.to_path_buf());
    }
    if let Some(rest) = raw.strip_prefix("~/.hermes/") {
        return Some(home.join(rest));
    }
    let raw = PathBuf::from(&raw);
    if raw.is_absolute() {
        return Some(raw);
    }
    if let Some(rest) = raw.to_string_lossy().strip_prefix("~/") {
        let user_home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)?;
        return Some(user_home.join(rest));
    }
    Some(home.join(raw))
}

fn load_filter_file_values(home: &Path, path_value: &Value) -> Vec<Value> {
    let Some(path) = path_value
        .as_str()
        .and_then(|path| resolve_profile_path(home, path))
    else {
        return Vec::new();
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) => {
            warn!(%error, path = %path.display(), "webhook filter in_file read failed");
            return Vec::new();
        }
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(items)) => items,
        Ok(Value::Object(items)) => items.into_iter().map(|(key, _)| Value::String(key)).collect(),
        Ok(value) => vec![value],
        Err(_) => raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| Value::String(line.to_string()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::{json, Value};
    use tempfile::tempdir;

    use super::{RoutePayloadDecision, WebhookRouteProcessor};

    #[tokio::test]
    async fn nested_any_and_in_file_filters_match_profile_relative_paths() {
        let home = tempdir().expect("home");
        let data = home.path().join("data");
        std::fs::create_dir_all(&data).expect("data");
        std::fs::write(data.join("watchlist.json"), "[\"chat-1\", \"chat-2\"]")
            .expect("watchlist");
        let processor = WebhookRouteProcessor::with_home(home.path().to_path_buf(), 30);
        let decision = processor
            .apply(
                &json!([
                    {"field": "payload.fromMe", "equals": false},
                    {"any": [
                        {"field": "payload.chatId", "in_file": "~/.hermes/data/watchlist.json"},
                        {"field": "payload.id.remote", "in_file": "~/.hermes/data/watchlist.json"}
                    ]}
                ]),
                None,
                json!({"payload": {"fromMe": false, "chatId": "chat-2"}}),
                "message",
                &HashMap::new(),
            )
            .await;
        assert!(matches!(decision, RoutePayloadDecision::Continue(_)));
    }

    #[tokio::test]
    async fn non_matching_filter_is_rejected_before_script_execution() {
        let home = tempdir().expect("home");
        let scripts = home.path().join("scripts");
        std::fs::create_dir_all(&scripts).expect("scripts");
        std::fs::write(scripts.join("must-not-run.sh"), "exit 99\n").expect("script");
        let processor = WebhookRouteProcessor::with_home(home.path().to_path_buf(), 30);
        let decision = processor
            .apply(
                &json!([{"field": "payload.label", "equals": "urgent"}]),
                Some("must-not-run.sh"),
                json!({"payload": {"label": "later"}}),
                "task",
                &HashMap::new(),
            )
            .await;
        assert_eq!(decision, RoutePayloadDecision::IgnoredFilter);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn script_transforms_payload_inside_hermes_scripts_root() {
        let home = tempdir().expect("home");
        let scripts = home.path().join("scripts");
        std::fs::create_dir_all(&scripts).expect("scripts");
        std::fs::write(
            scripts.join("transform.sh"),
            "#!/usr/bin/env bash\nprintf '%s' '{\"body\":\"PROFILE-SAFE\"}'\n",
        )
        .expect("script");
        let processor = WebhookRouteProcessor::with_home(home.path().to_path_buf(), 30);
        let decision = processor
            .apply(
                &Value::Null,
                Some("~/.hermes/scripts/transform.sh"),
                json!({"body": "old"}),
                "task",
                &HashMap::new(),
            )
            .await;
        assert_eq!(
            decision,
            RoutePayloadDecision::Continue(json!({"body": "PROFILE-SAFE"}))
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn silent_script_rejects_payload() {
        let home = tempdir().expect("home");
        let scripts = home.path().join("scripts");
        std::fs::create_dir_all(&scripts).expect("scripts");
        std::fs::write(scripts.join("silent.sh"), "#!/usr/bin/env bash\nprintf '[SILENT]'\n")
            .expect("script");
        let processor = WebhookRouteProcessor::with_home(home.path().to_path_buf(), 30);
        let decision = processor
            .apply(
                &Value::Null,
                Some("silent.sh"),
                json!({"body": "ignore"}),
                "task",
                &HashMap::new(),
            )
            .await;
        assert_eq!(decision, RoutePayloadDecision::IgnoredScript);
    }
}
