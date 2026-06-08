//! OSV malware check for MCP extension packages.
//!
//! Before launching an MCP server via npx/uvx, queries the OSV API to check if
//! the package has any known malware advisories (MAL-* IDs). Regular CVEs are
//! ignored — only confirmed malware is blocked.
//!
//! Corresponds to `hermes-agent/tools/osv_check.py`.

use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Value, json};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

const DEFAULT_OSV_ENDPOINT: &str = "https://api.osv.dev/v1/query";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Check if an MCP server package has known malware advisories.
///
/// Returns an error message if malware found, or `None` if clean/unknown.
/// Fail-open: network errors return `None` (allow).
pub async fn check_package_for_malware(command: &str, args: &[String]) -> Option<String> {
    let ecosystem = infer_ecosystem(command)?;
    let (package, version) = parse_package_from_args(args, ecosystem)?;

    let malware = match query_osv(&package, ecosystem, version.as_deref()).await {
        Ok(vulns) => vulns,
        Err(_) => return None, // fail-open
    };

    if malware.is_empty() {
        return None;
    }

    let ids: Vec<&str> = malware.iter().map(|m| m.id.as_str()).take(3).collect();
    let summary_strings: Vec<String> = malware
        .iter()
        .take(3)
        .map(|m| {
            let s = m.summary.as_deref().unwrap_or(&m.id);
            if s.len() > 100 { s[..100].to_string() } else { s.to_string() }
        })
        .collect();

    Some(format!(
        "BLOCKED: Package '{package}' ({ecosystem}) has known malware advisories: {}. Details: {}",
        ids.join(", "),
        summary_strings.join("; "),
    ))
}

#[derive(Debug)]
struct MalwareVuln {
    id: String,
    summary: Option<String>,
}

fn infer_ecosystem(command: &str) -> Option<&'static str> {
    let base = std::path::Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(command)
        .to_lowercase();

    match base.as_str() {
        "npx" | "npx.cmd" => Some("npm"),
        "uvx" | "uvx.cmd" | "pipx" => Some("PyPI"),
        _ => None,
    }
}

fn parse_package_from_args(args: &[String], ecosystem: &str) -> Option<(String, Option<String>)> {
    let package_token = args.iter().find(|a| !a.starts_with('-'))?;

    match ecosystem {
        "npm" => parse_npm_package(package_token),
        "PyPI" => parse_pypi_package(package_token),
        _ => Some((package_token.clone(), None)),
    }
}

fn parse_npm_package(token: &str) -> Option<(String, Option<String>)> {
    if token.starts_with('@') {
        // Scoped: @scope/name@version
        let re = Regex::new(r"^(@[^/]+/[^@]+)(?:@(.+))?$").unwrap();
        let caps = re.captures(token)?;
        let name = caps.get(1)?.as_str().to_string();
        let version = caps.get(2).map(|m| m.as_str().to_string());
        return Some((name, version));
    }
    if token.contains('@') {
        // Unscoped: name@version
        let parts: Vec<&str> = token.rsplitn(2, '@').collect();
        if parts.len() == 2 {
            let name = parts[1].to_string();
            let ver = parts[0].to_string();
            if ver != "latest" {
                return Some((name, Some(ver)));
            }
            return Some((name, None));
        }
    }
    Some((token.to_string(), None))
}

fn parse_pypi_package(token: &str) -> Option<(String, Option<String>)> {
    // Strip extras: name[extra1,extra2]==version
    let re = Regex::new(r"^([a-zA-Z0-9._-]+)(?:\[[^\]]*\])?(?:==(.+))?$").unwrap();
    let caps = re.captures(token)?;
    let name = caps.get(1)?.as_str().to_string();
    let version = caps.get(2).map(|m| m.as_str().to_string());
    Some((name, version))
}

async fn query_osv(
    package: &str,
    ecosystem: &str,
    version: Option<&str>,
) -> Result<Vec<MalwareVuln>, String> {
    let endpoint = std::env::var("OSV_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_OSV_ENDPOINT.to_string());

    let mut payload = serde_json::Map::new();
    let mut pkg = serde_json::Map::new();
    pkg.insert("name".into(), json!(package));
    pkg.insert("ecosystem".into(), json!(ecosystem));
    payload.insert("package".into(), json!(pkg));
    if let Some(v) = version {
        payload.insert("version".into(), json!(v));
    }

    let client = reqwest::Client::builder()
        .user_agent("hermes-agent-osv-check/1.0")
        .timeout(DEFAULT_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .post(&endpoint)
        .json(&payload)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    let vulns = body.get("vulns").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let malware: Vec<MalwareVuln> = vulns
        .iter()
        .filter_map(|v| {
            let id = v.get("id")?.as_str()?.to_string();
            if !id.starts_with("MAL-") {
                return None;
            }
            let summary = v.get("summary").and_then(|s| s.as_str()).map(|s| s.to_string());
            Some(MalwareVuln { id, summary })
        })
        .collect();

    Ok(malware)
}

// ---------------------------------------------------------------------------
// Tool Handler (existing)
// ---------------------------------------------------------------------------

pub struct OsvCheckHandler;

#[async_trait]
impl ToolHandler for OsvCheckHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let command = params.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let args: Vec<String> = params
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        if command.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'command'".into()));
        }

        let result = check_package_for_malware(command, &args).await;
        Ok(json!({
            "blocked": result.is_some(),
            "message": result,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("command".into(), json!({"type": "string", "description": "Command like npx or uvx"}));
        props.insert(
            "args".into(),
            json!({"type": "array", "items": {"type": "string"}, "description": "Command arguments"}),
        );
        tool_schema(
            "osv_check",
            "Check if an MCP server package has known malware advisories via the OSV API.",
            JsonSchema::object(props, vec!["command".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_ecosystem_npx() {
        assert_eq!(infer_ecosystem("npx"), Some("npm"));
        assert_eq!(infer_ecosystem("/usr/bin/npx"), Some("npm"));
        assert_eq!(infer_ecosystem("npx.cmd"), Some("npm"));
    }

    #[test]
    fn test_infer_ecosystem_uvx() {
        assert_eq!(infer_ecosystem("uvx"), Some("PyPI"));
        assert_eq!(infer_ecosystem("/usr/bin/uvx"), Some("PyPI"));
        assert_eq!(infer_ecosystem("pipx"), Some("PyPI"));
    }

    #[test]
    fn test_infer_ecosystem_unknown() {
        assert_eq!(infer_ecosystem("python"), None);
        assert_eq!(infer_ecosystem("node"), None);
    }

    #[test]
    fn test_parse_npm_unscoped() {
        let (name, ver) = parse_npm_package("express@4.18.0").unwrap();
        assert_eq!(name, "express");
        assert_eq!(ver, Some("4.18.0".into()));
    }

    #[test]
    fn test_parse_npm_scoped() {
        let (name, ver) = parse_npm_package("@anthropic/mcp-server@1.0.0").unwrap();
        assert_eq!(name, "@anthropic/mcp-server");
        assert_eq!(ver, Some("1.0.0".into()));
    }

    #[test]
    fn test_parse_npm_no_version() {
        let (name, ver) = parse_npm_package("express").unwrap();
        assert_eq!(name, "express");
        assert_eq!(ver, None);
    }

    #[test]
    fn test_parse_pypi_with_version() {
        let (name, ver) = parse_pypi_package("requests==2.31.0").unwrap();
        assert_eq!(name, "requests");
        assert_eq!(ver, Some("2.31.0".into()));
    }

    #[test]
    fn test_parse_pypi_no_version() {
        let (name, ver) = parse_pypi_package("requests").unwrap();
        assert_eq!(name, "requests");
        assert_eq!(ver, None);
    }
}
