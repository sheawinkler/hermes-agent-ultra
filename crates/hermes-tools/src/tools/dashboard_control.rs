use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_config::{
    hermes_home, load_user_config_file, save_config_yaml, validate_config, PlatformConfig,
};
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

fn normalize_host(input: Option<&Value>) -> String {
    let raw = input
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("127.0.0.1");
    raw.to_string()
}

fn normalize_port(input: Option<&Value>) -> Result<u16, ToolError> {
    let Some(v) = input else {
        return Ok(8080);
    };
    if let Some(p) = v.as_u64() {
        return u16::try_from(p).map_err(|_| ToolError::InvalidParams("port out of range".into()));
    }
    if let Some(s) = v.as_str() {
        let parsed = s
            .trim()
            .parse::<u16>()
            .map_err(|_| ToolError::InvalidParams("port must be a valid integer".into()))?;
        return Ok(parsed);
    }
    Err(ToolError::InvalidParams(
        "port must be an integer or numeric string".into(),
    ))
}

fn parse_bool(input: Option<&Value>, default: bool) -> bool {
    input.and_then(Value::as_bool).unwrap_or(default)
}

fn is_local_host(host: &str) -> bool {
    matches!(host.trim(), "127.0.0.1" | "localhost" | "::1")
}

fn dashboard_url(host: &str, port: u16) -> String {
    let display_host = if host.trim() == "0.0.0.0" {
        "127.0.0.1"
    } else {
        host.trim()
    };
    format!("http://{}:{}/", display_host, port)
}

fn read_dashboard_platform() -> Result<(std::path::PathBuf, PlatformConfig), ToolError> {
    let cfg_path = hermes_home().join("config.yaml");
    let cfg = load_user_config_file(&cfg_path)
        .map_err(|e| ToolError::ExecutionFailed(format!("load config: {e}")))?;
    let platform = cfg.platforms.get("api_server").cloned().unwrap_or_default();
    Ok((cfg_path, platform))
}

fn current_host_and_port(platform: &PlatformConfig) -> (String, u16) {
    let host = platform
        .extra
        .get("host")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("127.0.0.1")
        .to_string();
    let port = platform
        .extra
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(8080);
    (host, port)
}

fn emit_status_payload(cfg_path: &std::path::Path, platform: &PlatformConfig) -> String {
    let (host, port) = current_host_and_port(platform);
    json!({
        "status": "ok",
        "enabled": platform.enabled,
        "host": host,
        "port": port,
        "url": dashboard_url(&host, port),
        "config_path": cfg_path.display().to_string(),
    })
    .to_string()
}

fn persist_dashboard_platform(
    enabled: bool,
    host: String,
    port: u16,
    insecure: bool,
) -> Result<String, ToolError> {
    if enabled && !insecure && !is_local_host(&host) {
        return Err(ToolError::InvalidParams(
            "refusing non-localhost bind without insecure=true".into(),
        ));
    }

    let cfg_path = hermes_home().join("config.yaml");
    let mut cfg = load_user_config_file(&cfg_path)
        .map_err(|e| ToolError::ExecutionFailed(format!("load config: {e}")))?;

    let platform = cfg
        .platforms
        .entry("api_server".to_string())
        .or_insert_with(PlatformConfig::default);
    platform.enabled = enabled;
    platform
        .extra
        .insert("host".to_string(), Value::String(host.clone()));
    platform.extra.insert("port".to_string(), json!(port));

    validate_config(&cfg)
        .map_err(|e| ToolError::ExecutionFailed(format!("validate config: {e}")))?;
    save_config_yaml(&cfg_path, &cfg)
        .map_err(|e| ToolError::ExecutionFailed(format!("save config: {e}")))?;

    Ok(json!({
        "status": "ok",
        "enabled": enabled,
        "host": host,
        "port": port,
        "url": dashboard_url(&host, port),
        "config_path": cfg_path.display().to_string(),
    })
    .to_string())
}

#[derive(Default)]
pub struct DashboardControlHandler;

#[async_trait]
impl ToolHandler for DashboardControlHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
            .trim()
            .to_ascii_lowercase();

        match action.as_str() {
            "status" => {
                let (cfg_path, platform) = read_dashboard_platform()?;
                Ok(emit_status_payload(&cfg_path, &platform))
            }
            "enable" | "on" => {
                let host = normalize_host(params.get("host"));
                let port = normalize_port(params.get("port"))?;
                let insecure = parse_bool(params.get("insecure"), false);
                persist_dashboard_platform(true, host, port, insecure)
            }
            "disable" | "off" => {
                let (cfg_path, platform) = read_dashboard_platform()?;
                let (host, port) = current_host_and_port(&platform);
                persist_dashboard_platform(false, host, port, true).map(|raw| {
                    let parsed: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
                    json!({
                        "status": "ok",
                        "enabled": false,
                        "host": parsed.get("host").and_then(Value::as_str).unwrap_or("127.0.0.1"),
                        "port": parsed.get("port").and_then(Value::as_u64).unwrap_or(8080),
                        "url": parsed.get("url").and_then(Value::as_str).unwrap_or(""),
                        "config_path": cfg_path.display().to_string(),
                    })
                    .to_string()
                })
            }
            "url" => {
                let (cfg_path, platform) = read_dashboard_platform()?;
                let (host, port) = current_host_and_port(&platform);
                Ok(json!({
                    "status": "ok",
                    "enabled": platform.enabled,
                    "url": dashboard_url(&host, port),
                    "host": host,
                    "port": port,
                    "config_path": cfg_path.display().to_string(),
                })
                .to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "unsupported action '{other}' (expected status|enable|disable|url)"
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({"type":"string","enum":["status","enable","on","disable","off","url"]}),
        );
        props.insert("host".into(), json!({"type":"string"}));
        props.insert(
            "port".into(),
            json!({"type":"integer","minimum":1,"maximum":65535}),
        );
        props.insert(
            "insecure".into(),
            json!({
                "type":"boolean",
                "description":"Allow non-localhost bind when enabling dashboard."
            }),
        );
        tool_schema(
            "dashboard_control",
            "Inspect and configure dashboard/api_server host/port enablement in Hermes config.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    #[tokio::test]
    async fn enable_status_disable_roundtrip() {
        let _guard = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", temp.path());

        let handler = DashboardControlHandler;
        let enabled = handler
            .execute(json!({"action":"enable","host":"127.0.0.1","port":9191}))
            .await
            .expect("enable");
        let enabled_json: Value = serde_json::from_str(&enabled).expect("json");
        assert_eq!(enabled_json["enabled"], true);
        assert_eq!(enabled_json["port"], 9191);

        let status = handler
            .execute(json!({"action":"status"}))
            .await
            .expect("status");
        let status_json: Value = serde_json::from_str(&status).expect("json");
        assert_eq!(status_json["enabled"], true);
        assert_eq!(status_json["host"], "127.0.0.1");
        assert_eq!(status_json["port"], 9191);

        let disabled = handler
            .execute(json!({"action":"disable"}))
            .await
            .expect("disable");
        let disabled_json: Value = serde_json::from_str(&disabled).expect("json");
        assert_eq!(disabled_json["enabled"], false);
    }

    #[tokio::test]
    async fn rejects_non_local_without_insecure() {
        let _guard = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", temp.path());

        let handler = DashboardControlHandler;
        let err = handler
            .execute(json!({"action":"enable","host":"0.0.0.0","port":8080}))
            .await
            .expect_err("non-local should fail");
        match err {
            ToolError::InvalidParams(msg) => assert!(msg.contains("non-localhost")),
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
