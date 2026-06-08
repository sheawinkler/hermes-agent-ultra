use std::collections::BTreeMap;
use std::path::Path;

use hermes_core::AgentError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpTransportKind {
    Http,
    Stdio,
}

impl McpTransportKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::Stdio => "stdio",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub supports_parallel_tool_calls: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl McpServerEntry {
    pub fn transport_kind(&self) -> McpTransportKind {
        if self.url.is_some() {
            McpTransportKind::Http
        } else {
            McpTransportKind::Stdio
        }
    }

    pub fn transport_display(&self) -> String {
        if let Some(url) = self.url.as_deref() {
            return url.to_string();
        }
        let mut parts = Vec::new();
        if let Some(command) = self.command.as_deref() {
            parts.push(command.to_string());
        }
        parts.extend(self.args.iter().take(2).cloned());
        if parts.is_empty() {
            "(unconfigured)".to_string()
        } else {
            parts.join(" ")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct McpConfig {
    pub servers: Vec<McpServerEntry>,
}

impl McpConfig {
    pub fn get(&self, name: &str) -> Option<&McpServerEntry> {
        self.servers.iter().find(|entry| entry.name == name)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &str> {
        self.servers
            .iter()
            .flat_map(|entry| entry.warnings.iter().map(String::as_str))
    }
}

#[derive(Debug, Deserialize)]
struct RawMcpServerEntry {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    supports_parallel_tool_calls: bool,
}

fn default_enabled() -> bool {
    true
}

fn normalized_nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn parse_entry(name: &str, raw: RawMcpServerEntry) -> Result<McpServerEntry, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("MCP server name must not be empty".to_string());
    }
    let command = normalized_nonempty(raw.command);
    let url = normalized_nonempty(raw.url);
    let mut warnings = Vec::new();
    if command.is_some() && url.is_some() {
        warnings.push(format!(
            "MCP server '{name}' has both 'url' and 'command' in config. Using HTTP transport ('url'). Remove 'command' to silence this warning."
        ));
    }
    match (&command, &url) {
        (None, None) => {
            return Err(format!(
                "MCP server '{name}' must configure either 'url' or 'command'"
            ));
        }
        (_, Some(url)) => {
            let parsed = reqwest::Url::parse(url)
                .map_err(|err| format!("MCP server '{name}' has invalid url '{url}': {err}"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(format!(
                    "MCP server '{name}' url must use http or https, got '{}'",
                    parsed.scheme()
                ));
            }
        }
        _ => {}
    }

    Ok(McpServerEntry {
        name: name.to_string(),
        command,
        args: raw.args,
        env: raw.env,
        url,
        enabled: raw.enabled.unwrap_or(true),
        supports_parallel_tool_calls: raw.supports_parallel_tool_calls,
        warnings,
    })
}

pub fn parse_mcp_config_json(raw: &str) -> Result<McpConfig, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|err| format!("invalid MCP JSON: {err}"))?;
    if value.is_null() {
        return Ok(McpConfig::default());
    }

    if let Some(servers_value) = value.get("servers") {
        let array = servers_value
            .as_array()
            .ok_or_else(|| "MCP config 'servers' must be an array".to_string())?;
        let mut servers = Vec::new();
        for item in array {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "MCP server array entries require a string 'name'".to_string())?;
            let raw_entry: RawMcpServerEntry = serde_json::from_value(item.clone())
                .map_err(|err| format!("invalid MCP server '{name}': {err}"))?;
            servers.push(parse_entry(name, raw_entry)?);
        }
        servers.sort_by(|a, b| a.name.cmp(&b.name));
        return Ok(McpConfig { servers });
    }

    let object = value
        .as_object()
        .ok_or_else(|| "MCP config must be a JSON object".to_string())?;
    let mut servers = Vec::new();
    for (name, entry_value) in object {
        let raw_entry: RawMcpServerEntry = serde_json::from_value(entry_value.clone())
            .map_err(|err| format!("invalid MCP server '{name}': {err}"))?;
        servers.push(parse_entry(name, raw_entry)?);
    }
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(McpConfig { servers })
}

pub fn load_mcp_config(path: &Path) -> Result<McpConfig, AgentError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| AgentError::Io(format!("failed to read {}: {err}", path.display())))?;
    parse_mcp_config_json(&raw)
        .map_err(|err| AgentError::Config(format!("invalid MCP config {}: {err}", path.display())))
}

pub fn load_mcp_config_if_exists(path: &Path) -> Result<Option<McpConfig>, AgentError> {
    if !path.exists() {
        return Ok(None);
    }
    load_mcp_config(path).map(Some)
}

#[cfg(test)]
mod tests {
    use super::{load_mcp_config, parse_mcp_config_json, McpTransportKind};

    #[test]
    fn parses_object_config_and_warns_when_url_and_command_conflict() {
        let cfg = parse_mcp_config_json(
            r#"{
              "remote": {
                "url": "https://example.com/mcp",
                "command": "npx",
                "args": ["-y", "server"],
                "supports_parallel_tool_calls": true
              }
            }"#,
        )
        .expect("valid config");
        let entry = cfg.get("remote").expect("remote entry");
        assert_eq!(entry.transport_kind(), McpTransportKind::Http);
        assert_eq!(entry.transport_display(), "https://example.com/mcp");
        assert!(entry.supports_parallel_tool_calls);
        assert_eq!(cfg.warnings().count(), 1);
        assert!(cfg
            .warnings()
            .next()
            .expect("warning")
            .contains("Using HTTP transport"));
    }

    #[test]
    fn rejects_missing_transport() {
        let err = parse_mcp_config_json(r#"{"broken": {"enabled": true}}"#)
            .expect_err("missing transport should fail");
        assert!(err.contains("either 'url' or 'command'"));
    }

    #[test]
    fn rejects_non_http_urls() {
        let err = parse_mcp_config_json(r#"{"bad": {"url": "ftp://example.com/mcp"}}"#)
            .expect_err("unsupported scheme should fail");
        assert!(err.contains("http or https"));
    }

    #[test]
    fn parses_servers_array_compat_shape() {
        let cfg = parse_mcp_config_json(
            r#"{"servers": [{"name": "local", "command": "hermes", "args": ["mcp", "serve"]}]}"#,
        )
        .expect("array config");
        let entry = cfg.get("local").expect("local entry");
        assert_eq!(entry.transport_kind(), McpTransportKind::Stdio);
        assert_eq!(entry.transport_display(), "hermes mcp serve");
        assert!(entry.enabled);
    }

    #[test]
    fn load_mcp_config_reports_path_for_invalid_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("mcp_servers.json");
        std::fs::write(&path, r#"{"bad": {"url": "not a url"}}"#).expect("write");
        let err = load_mcp_config(&path).expect_err("invalid url");
        let msg = err.to_string();
        assert!(msg.contains("mcp_servers.json"));
        assert!(msg.contains("invalid url"));
    }
}
