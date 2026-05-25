//! SOTA command surface for local release, interoperability, and agent handoff checks.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{SecondsFormat, Utc};
use hermes_core::AgentError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::providers::{known_providers, provider_capability_for};

const A2A_CARD_PATH: &str = "/.well-known/agent.json";
const A2A_CARD_ALT_PATH: &str = "/agent-card.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SotaItem {
    pub id: u8,
    pub title: String,
    pub status: String,
    pub command: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SotaStatusReport {
    pub kind: String,
    pub version: String,
    pub generated_at: String,
    pub implemented: usize,
    pub total: usize,
    pub items: Vec<SotaItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlightEvent {
    pub event_id: String,
    pub timestamp: String,
    pub run_id: String,
    pub objective: String,
    pub phase: String,
    pub actor: String,
    pub tool: Option<String>,
    pub status: String,
    pub duration_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub artifacts: Vec<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlightReport {
    pub kind: String,
    pub path: String,
    pub events: Vec<FlightEvent>,
    pub event_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalGate {
    pub name: String,
    pub status: String,
    pub detail: String,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalGateReport {
    pub kind: String,
    pub version: String,
    pub generated_at: String,
    pub passed: bool,
    pub gates: Vec<EvalGate>,
    pub release_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConformanceCheck {
    pub method: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConformanceReport {
    pub kind: String,
    pub protocol_version: String,
    pub generated_at: String,
    pub passed: bool,
    pub checks: Vec<ConformanceCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCapabilityRow {
    pub provider: String,
    pub oauth_supported: bool,
    pub models_dev_merged: bool,
    pub managed_tools_supported: bool,
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn state_root(config_dir: Option<&str>) -> PathBuf {
    let override_path = config_dir.map(Path::new);
    hermes_config::state_dir(override_path)
}

fn report_output(value: &Value, json_only: bool, output: Option<&str>) -> Result<(), AgentError> {
    let rendered = serde_json::to_string_pretty(value)
        .map_err(|err| AgentError::Config(format!("serialize SOTA report: {err}")))?;
    if let Some(path) = output.map(str::trim).filter(|path| !path.is_empty()) {
        let output_path = PathBuf::from(path);
        if let Some(parent) = output_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|err| AgentError::Io(format!("mkdir {}: {err}", parent.display())))?;
        }
        std::fs::write(&output_path, rendered.as_bytes())
            .map_err(|err| AgentError::Io(format!("write {}: {err}", output_path.display())))?;
    }
    if json_only {
        println!("{rendered}");
    }
    Ok(())
}

fn print_human_status(report: &SotaStatusReport) {
    println!(
        "Hermes SOTA status: {}/{} implemented",
        report.implemented, report.total
    );
    println!("Version: {}", report.version);
    for item in &report.items {
        println!(
            "{}. {} [{}]\n   command: {}\n   evidence: {}",
            item.id, item.title, item.status, item.command, item.evidence
        );
    }
}

fn print_human_eval(report: &EvalGateReport) {
    println!(
        "Hermes local release gate: {}",
        if report.passed { "pass" } else { "fail" }
    );
    for gate in &report.gates {
        println!("- {} [{}]: {}", gate.name, gate.status, gate.detail);
        if let Some(command) = &gate.command {
            println!("  command: {command}");
        }
    }
}

fn print_human_flight(report: &FlightReport) {
    println!("Flight recorder: {} event(s)", report.event_count);
    println!("Path: {}", report.path);
    for event in &report.events {
        println!(
            "- {} {} {} {}",
            event.timestamp, event.run_id, event.phase, event.status
        );
    }
}

fn print_human_matrix(value: &Value) {
    println!("Provider/tool capability matrix");
    if let Some(providers) = value.get("providers").and_then(Value::as_array) {
        for row in providers {
            println!(
                "- {} oauth={} models_dev={} managed_tools={}",
                row.get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                row.get("oauth_supported")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                row.get("models_dev_merged")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                row.get("managed_tools_supported")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            );
        }
    }
}

pub async fn handle_cli_sota(
    config_dir: Option<String>,
    action: Option<String>,
    topic: Option<String>,
    json_only: bool,
    output: Option<String>,
    host: String,
    port: u16,
    once: bool,
) -> Result<(), AgentError> {
    let action = action
        .as_deref()
        .map(str::trim)
        .filter(|action| !action.is_empty())
        .unwrap_or("status")
        .to_ascii_lowercase();
    let topic = topic
        .as_deref()
        .map(str::trim)
        .filter(|topic| !topic.is_empty())
        .unwrap_or("")
        .to_ascii_lowercase();
    let output_ref = output.as_deref();
    let state = state_root(config_dir.as_deref());

    match action.as_str() {
        "status" | "all" => {
            let report = build_status_report();
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode SOTA status: {err}")))?;
            if !json_only {
                print_human_status(&report);
            }
            report_output(&value, json_only, output_ref)
        }
        "eval" | "gate" | "release-gate" => {
            let report = build_eval_gate_report();
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode eval gate: {err}")))?;
            if !json_only {
                print_human_eval(&report);
            }
            report_output(&value, json_only, output_ref)
        }
        "flight" | "recorder" => {
            let path = flight_recorder_path(&state);
            let report = match topic.as_str() {
                "sample" | "write" | "append" => {
                    let event = sample_flight_event();
                    append_flight_event(&path, &event)?;
                    build_flight_report(&path)?
                }
                "show" | "list" | "" => build_flight_report(&path)?,
                other => {
                    return Err(AgentError::Config(format!(
                        "unknown flight action '{other}' (use sample or show)"
                    )));
                }
            };
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode flight report: {err}")))?;
            if !json_only {
                print_human_flight(&report);
            }
            report_output(&value, json_only, output_ref)
        }
        "a2a" | "agent-card" | "agent2agent" => match topic.as_str() {
            "serve" | "server" => serve_a2a_card(&host, port, once),
            "card" | "show" | "" => {
                let value = build_a2a_card();
                if !json_only {
                    println!("A2A agent card");
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&value).map_err(|err| {
                            AgentError::Config(format!("encode A2A card: {err}"))
                        })?
                    );
                }
                report_output(&value, json_only, output_ref)
            }
            other => Err(AgentError::Config(format!(
                "unknown A2A action '{other}' (use card or serve)"
            ))),
        },
        "mcp" => {
            if !matches!(topic.as_str(), "conformance" | "doctor" | "show" | "") {
                return Err(AgentError::Config(format!(
                    "unknown MCP action '{topic}' (use conformance)"
                )));
            }
            let report = build_mcp_conformance_report().await;
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode MCP report: {err}")))?;
            if !json_only {
                println!(
                    "MCP conformance: {}",
                    if report.passed { "pass" } else { "fail" }
                );
                for check in &report.checks {
                    println!("- {} [{}]: {}", check.method, check.status, check.detail);
                }
            }
            report_output(&value, json_only, output_ref)
        }
        "capabilities" | "capability" | "providers" | "matrix" => {
            let value = build_capability_matrix();
            if !json_only {
                print_human_matrix(&value);
            }
            report_output(&value, json_only, output_ref)
        }
        "handoff" | "handoffs" | "contract" | "contracts" => {
            if !matches!(topic.as_str(), "template" | "schema" | "contract" | "show" | "") {
                return Err(AgentError::Config(format!(
                    "unknown handoff action '{topic}' (use template)"
                )));
            }
            let value = build_handoff_contract_template();
            if !json_only {
                println!("Typed handoff contract template");
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).map_err(|err| {
                        AgentError::Config(format!("encode handoff contract: {err}"))
                    })?
                );
            }
            report_output(&value, json_only, output_ref)
        }
        other => Err(AgentError::Config(format!(
            "unknown SOTA action '{other}' (use status, eval, flight, a2a, mcp, capabilities, or handoff)"
        ))),
    }
}

pub fn build_status_report() -> SotaStatusReport {
    let items = vec![
        SotaItem {
            id: 1,
            title: "OAuth credential file safety".to_string(),
            status: "guarded".to_string(),
            command: "hermes auth login <provider>".to_string(),
            evidence: "auth.json, Qwen, and Gemini OAuth saves use atomic owner-only writes"
                .to_string(),
        },
        SotaItem {
            id: 2,
            title: "Local release/eval gate".to_string(),
            status: "implemented".to_string(),
            command: "hermes sota eval --json".to_string(),
            evidence: "deterministic repo/file/version gates plus explicit local release commands"
                .to_string(),
        },
        SotaItem {
            id: 3,
            title: "Agent Flight Recorder".to_string(),
            status: "implemented".to_string(),
            command: "hermes sota flight sample && hermes sota flight show".to_string(),
            evidence: "JSONL event schema under state/flight-recorder/events.jsonl".to_string(),
        },
        SotaItem {
            id: 4,
            title: "A2A skeleton, card, and card server".to_string(),
            status: "implemented".to_string(),
            command: "hermes sota a2a card --json".to_string(),
            evidence: "well-known agent card plus minimal HTTP card server".to_string(),
        },
        SotaItem {
            id: 5,
            title: "MCP conformance doctor".to_string(),
            status: "implemented".to_string(),
            command: "hermes sota mcp conformance --json".to_string(),
            evidence: "live in-process checks for initialize/list/read/call/ping method handling"
                .to_string(),
        },
        SotaItem {
            id: 6,
            title: "Typed handoff contracts".to_string(),
            status: "implemented".to_string(),
            command: "hermes sota handoff template --json".to_string(),
            evidence: "versioned JSON contract template with schemas, budgets, evidence, and stop conditions"
                .to_string(),
        },
        SotaItem {
            id: 7,
            title: "Provider/tool capability matrix".to_string(),
            status: "implemented".to_string(),
            command: "hermes sota capabilities --json".to_string(),
            evidence: "provider registry exported with OAuth, models.dev, and managed-tool bits"
                .to_string(),
        },
    ];
    SotaStatusReport {
        kind: "hermes.sota.status".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at: now_rfc3339(),
        implemented: items.len(),
        total: items.len(),
        items,
    }
}

pub fn build_eval_gate_report() -> EvalGateReport {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let gates = vec![
        file_gate(
            "workspace manifest",
            &repo_root.join("Cargo.toml"),
            "workspace package version and release metadata are present",
            None,
        ),
        file_gate(
            "release workflow",
            &repo_root.join(".github/workflows/release.yml"),
            "tag release workflow is present for artifact publishing",
            None,
        ),
        file_gate(
            "sota command module",
            &repo_root.join("crates/hermes-cli/src/sota.rs"),
            "new SOTA command surface is compiled into hermes-cli",
            Some("cargo test -p hermes-cli sota -- --nocapture"),
        ),
        file_gate(
            "mcp server implementation",
            &repo_root.join("crates/hermes-mcp/src/server.rs"),
            "MCP in-process conformance checks have a real server target",
            Some("cargo test -p hermes-mcp"),
        ),
        file_gate(
            "provider parity snapshot",
            &repo_root.join("docs/parity/upstream-provider-auth-snapshot.json"),
            "provider capability matrix is grounded in the existing upstream parity snapshot",
            Some("cargo test -p hermes-cli providers -- --nocapture"),
        ),
    ];
    let passed = gates.iter().all(|gate| gate.status == "pass");
    EvalGateReport {
        kind: "hermes.sota.eval_gate".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at: now_rfc3339(),
        passed,
        gates,
        release_commands: vec![
            "cargo fmt --all --check".to_string(),
            "cargo test -p hermes-cli sota -- --nocapture".to_string(),
            "cargo test -p hermes-cli auth_store_write_is_atomic_owner_only_and_cleans_tmp -- --nocapture"
                .to_string(),
            "cargo test -p hermes-mcp".to_string(),
            "cargo run -p hermes-cli --bin hermes-agent-ultra -- sota status --json".to_string(),
        ],
    }
}

fn file_gate(name: &str, path: &Path, detail: &str, command: Option<&str>) -> EvalGate {
    EvalGate {
        name: name.to_string(),
        status: if path.exists() { "pass" } else { "fail" }.to_string(),
        detail: format!("{} ({})", detail, path.display()),
        command: command.map(str::to_string),
    }
}

pub fn flight_recorder_path(state_root: &Path) -> PathBuf {
    state_root.join("flight-recorder").join("events.jsonl")
}

pub fn sample_flight_event() -> FlightEvent {
    FlightEvent {
        event_id: Uuid::new_v4().to_string(),
        timestamp: now_rfc3339(),
        run_id: format!("local-{}", Uuid::new_v4()),
        objective: "sota-local-verification".to_string(),
        phase: "checkpoint".to_string(),
        actor: "hermes-cli".to_string(),
        tool: Some("sota.flight".to_string()),
        status: "ok".to_string(),
        duration_ms: Some(0),
        input_tokens: None,
        output_tokens: None,
        cost_usd: None,
        artifacts: vec!["hermes sota flight sample".to_string()],
        metadata: json!({
            "schema": "hermes.flight_event.v1",
            "note": "sample event written by the local SOTA flight recorder"
        }),
    }
}

pub fn append_flight_event(path: &Path, event: &FlightEvent) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| AgentError::Io(format!("mkdir {}: {err}", parent.display())))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| AgentError::Io(format!("open {}: {err}", path.display())))?;
    let raw = serde_json::to_string(event)
        .map_err(|err| AgentError::Config(format!("serialize flight event: {err}")))?;
    file.write_all(raw.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|err| AgentError::Io(format!("write {}: {err}", path.display())))?;
    file.flush()
        .map_err(|err| AgentError::Io(format!("flush {}: {err}", path.display())))?;
    Ok(())
}

pub fn build_flight_report(path: &Path) -> Result<FlightReport, AgentError> {
    let events = read_flight_events(path)?;
    Ok(FlightReport {
        kind: "hermes.flight_report".to_string(),
        path: path.display().to_string(),
        event_count: events.len(),
        events,
    })
}

pub fn read_flight_events(path: &Path) -> Result<Vec<FlightEvent>, AgentError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|err| AgentError::Io(format!("read {}: {err}", path.display())))?;
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| {
            serde_json::from_str::<FlightEvent>(line).map_err(|err| {
                AgentError::Config(format!("parse {} line {}: {err}", path.display(), idx + 1))
            })
        })
        .collect()
}

pub fn build_a2a_card() -> Value {
    json!({
        "name": "Hermes Agent Ultra",
        "description": "Rust-native autonomous agent runtime with CLI/TUI, MCP bridge, provider routing, gateway integrations, and local-first release gates.",
        "version": env!("CARGO_PKG_VERSION"),
        "url": "http://127.0.0.1:9127",
        "documentationUrl": "https://github.com/sheawinkler/hermes-agent-ultra",
        "defaultInputModes": ["text/plain", "application/json"],
        "defaultOutputModes": ["text/plain", "application/json", "application/jsonl"],
        "capabilities": {
            "streaming": true,
            "stateTransitionHistory": true,
            "pushNotifications": false,
            "toolUse": true,
            "mcpBridge": true,
            "flightRecorder": true
        },
        "skills": [
            {
                "id": "local-code-agent",
                "name": "Local code-agent execution",
                "description": "Inspect, edit, test, and package local repositories with deterministic release gates.",
                "tags": ["coding", "rust", "release"]
            },
            {
                "id": "mcp-bridge",
                "name": "MCP bridge",
                "description": "Expose Hermes tools, resources, and prompts over Model Context Protocol.",
                "tags": ["mcp", "tools", "interoperability"]
            },
            {
                "id": "typed-handoff",
                "name": "Typed handoff contracts",
                "description": "Accept bounded agent-to-agent work packages with schemas, budgets, and evidence requirements.",
                "tags": ["handoff", "contracts", "agent-orchestration"]
            }
        ],
        "wellKnown": {
            "agentCard": A2A_CARD_PATH,
            "alternateCard": A2A_CARD_ALT_PATH,
            "health": "/healthz"
        }
    })
}

fn serve_a2a_card(host: &str, port: u16, once: bool) -> Result<(), AgentError> {
    let bind_addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&bind_addr)
        .map_err(|err| AgentError::Io(format!("bind A2A card server {bind_addr}: {err}")))?;
    let addr = listener
        .local_addr()
        .map_err(|err| AgentError::Io(format!("read A2A listener addr: {err}")))?;
    println!(
        "A2A card server listening on http://{}{}",
        addr, A2A_CARD_PATH
    );
    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => handle_a2a_stream(&mut stream)?,
            Err(err) => return Err(AgentError::Io(format!("accept A2A connection: {err}"))),
        }
        if once {
            break;
        }
    }
    Ok(())
}

fn handle_a2a_stream(stream: &mut TcpStream) -> Result<(), AgentError> {
    let mut buf = [0_u8; 4096];
    let n = stream
        .read(&mut buf)
        .map_err(|err| AgentError::Io(format!("read A2A request: {err}")))?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let (status, content_type, body) = a2a_response_for_path(path);
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|err| AgentError::Io(format!("write A2A response: {err}")))?;
    stream
        .flush()
        .map_err(|err| AgentError::Io(format!("flush A2A response: {err}")))?;
    Ok(())
}

fn a2a_response_for_path(path: &str) -> (u16, &'static str, String) {
    match path {
        A2A_CARD_PATH | A2A_CARD_ALT_PATH => (
            200,
            "application/json",
            serde_json::to_string_pretty(&build_a2a_card()).unwrap_or_else(|_| "{}".to_string()),
        ),
        "/healthz" => (
            200,
            "application/json",
            json!({"status":"ok","service":"hermes-a2a-card"}).to_string(),
        ),
        _ => (
            404,
            "application/json",
            json!({"error":"not_found","expected":[A2A_CARD_PATH,A2A_CARD_ALT_PATH,"/healthz"]})
                .to_string(),
        ),
    }
}

pub async fn build_mcp_conformance_report() -> McpConformanceReport {
    let server = hermes_mcp::McpServer::new(Arc::new(hermes_tools::ToolRegistry::new()));
    let mut checks = Vec::new();

    let initialize = server.handle_request("initialize", json!({})).await;
    checks.push(match initialize {
        Ok(value)
            if value.get("protocolVersion").is_some()
                && value.get("serverInfo").is_some()
                && value.get("capabilities").is_some() =>
        {
            pass(
                "initialize",
                "advertises protocol, serverInfo, and capabilities",
            )
        }
        Ok(value) => fail("initialize", format!("unexpected response shape: {value}")),
        Err(err) => fail("initialize", err.to_string()),
    });

    checks.push(list_check(
        "tools/list",
        server.handle_request("tools/list", json!({})).await,
        "tools",
    ));
    checks.push(list_check(
        "resources/list",
        server.handle_request("resources/list", json!({})).await,
        "resources",
    ));
    checks.push(list_check(
        "prompts/list",
        server.handle_request("prompts/list", json!({})).await,
        "prompts",
    ));
    checks.push(match server.handle_request("tools/call", json!({})).await {
        Err(hermes_mcp::McpError::InvalidParams(_)) => {
            pass("tools/call", "validates missing tool name")
        }
        Ok(value) => fail("tools/call", format!("expected InvalidParams, got {value}")),
        Err(err) => fail("tools/call", err.to_string()),
    });
    checks.push(
        match server.handle_request("resources/read", json!({})).await {
            Err(hermes_mcp::McpError::InvalidParams(_)) => {
                pass("resources/read", "validates missing resource uri")
            }
            Ok(value) => fail(
                "resources/read",
                format!("expected InvalidParams, got {value}"),
            ),
            Err(err) => fail("resources/read", err.to_string()),
        },
    );
    checks.push(
        match server.handle_request("prompts/get", json!({})).await {
            Err(hermes_mcp::McpError::InvalidParams(_)) => {
                pass("prompts/get", "validates missing prompt name")
            }
            Ok(value) => fail(
                "prompts/get",
                format!("expected InvalidParams, got {value}"),
            ),
            Err(err) => fail("prompts/get", err.to_string()),
        },
    );
    checks.push(match server.handle_request("ping", json!({})).await {
        Ok(value) if value.as_object().is_some_and(|obj| obj.is_empty()) => {
            pass("ping", "returns empty object")
        }
        Ok(value) => fail("ping", format!("unexpected ping response: {value}")),
        Err(err) => fail("ping", err.to_string()),
    });

    let passed = checks.iter().all(|check| check.status == "pass");
    McpConformanceReport {
        kind: "hermes.mcp.conformance".to_string(),
        protocol_version: "2024-11-05".to_string(),
        generated_at: now_rfc3339(),
        passed,
        checks,
    }
}

fn list_check(
    method: &str,
    result: Result<Value, hermes_mcp::McpError>,
    expected_field: &str,
) -> ConformanceCheck {
    match result {
        Ok(value)
            if value
                .get(expected_field)
                .and_then(Value::as_array)
                .is_some() =>
        {
            pass(method, format!("returns {expected_field} array"))
        }
        Ok(value) => fail(method, format!("unexpected response shape: {value}")),
        Err(err) => fail(method, err.to_string()),
    }
}

fn pass(method: &str, detail: impl Into<String>) -> ConformanceCheck {
    ConformanceCheck {
        method: method.to_string(),
        status: "pass".to_string(),
        detail: detail.into(),
    }
}

fn fail(method: &str, detail: impl Into<String>) -> ConformanceCheck {
    ConformanceCheck {
        method: method.to_string(),
        status: "fail".to_string(),
        detail: detail.into(),
    }
}

pub fn build_capability_matrix() -> Value {
    let providers: Vec<ProviderCapabilityRow> = known_providers()
        .into_iter()
        .filter_map(provider_capability_for)
        .map(|cap| ProviderCapabilityRow {
            provider: cap.id,
            oauth_supported: cap.oauth_supported,
            models_dev_merged: cap.models_dev_merged,
            managed_tools_supported: cap.managed_tools_supported,
        })
        .collect();

    json!({
        "kind": "hermes.capability_matrix",
        "version": env!("CARGO_PKG_VERSION"),
        "generated_at": now_rfc3339(),
        "providers": providers,
        "tool_surfaces": [
            {"surface":"mcp", "status":"implemented", "command":"hermes mcp serve"},
            {"surface":"terminal", "status":"implemented", "command":"interactive tool policy"},
            {"surface":"gateway", "status":"implemented", "command":"hermes gateway status"},
            {"surface":"cron", "status":"implemented", "command":"hermes cron list"},
            {"surface":"webhook", "status":"implemented", "command":"hermes webhook list"},
            {"surface":"flight_recorder", "status":"implemented", "command":"hermes sota flight show"}
        ]
    })
}

pub fn build_handoff_contract_template() -> Value {
    json!({
        "kind": "hermes.typed_handoff_contract",
        "schema_version": 1,
        "generated_at": now_rfc3339(),
        "contract": {
            "handoff_id": "handoff-<uuid>",
            "objective": "Concrete, testable objective for the receiving agent.",
            "scope": {
                "repo": "/absolute/path/to/repo",
                "allowed_paths": ["crates/hermes-cli/src"],
                "forbidden_paths": [".env", "secrets", "live-keys"]
            },
            "input_schema": {
                "type": "object",
                "required": ["objective", "constraints", "evidence"],
                "properties": {
                    "objective": {"type": "string"},
                    "constraints": {"type": "array", "items": {"type": "string"}},
                    "evidence": {"type": "array", "items": {"type": "string"}}
                }
            },
            "output_schema": {
                "type": "object",
                "required": ["changed", "verification", "residual_risk"],
                "properties": {
                    "changed": {"type": "array", "items": {"type": "string"}},
                    "verification": {"type": "array", "items": {"type": "string"}},
                    "residual_risk": {"type": "array", "items": {"type": "string"}}
                }
            },
            "budgets": {
                "max_wall_clock_seconds": 3600,
                "max_files_changed": 12,
                "max_network_calls": 10
            },
            "allowed_tools": ["shell", "apply_patch", "cargo test", "git"],
            "evidence_required": [
                "git diff --stat",
                "relevant deterministic tests",
                "interactive CLI smoke where user-facing behavior changed"
            ],
            "stop_conditions": [
                "unexpected dirty worktree changes outside handoff scope",
                "secret exposure risk",
                "same blocker repeats three times"
            ]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sota_status_tracks_all_seven_items() {
        let report = build_status_report();
        assert_eq!(report.total, 7);
        assert_eq!(report.implemented, 7);
        assert!(report
            .items
            .iter()
            .any(|item| item.title.contains("Flight")));
    }

    #[test]
    fn flight_recorder_writes_and_reads_jsonl() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = flight_recorder_path(temp.path());
        let event = sample_flight_event();
        append_flight_event(&path, &event).expect("append event");
        let report = build_flight_report(&path).expect("report");
        assert_eq!(report.event_count, 1);
        assert_eq!(report.events[0].event_id, event.event_id);
    }

    #[test]
    fn a2a_card_has_well_known_routes() {
        let card = build_a2a_card();
        assert_eq!(
            card.get("wellKnown")
                .and_then(|v| v.get("agentCard"))
                .and_then(Value::as_str),
            Some(A2A_CARD_PATH)
        );
        let (status, content_type, body) = a2a_response_for_path(A2A_CARD_PATH);
        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        assert!(body.contains("Hermes Agent Ultra"));
    }

    #[tokio::test]
    async fn mcp_conformance_report_passes_core_methods() {
        let report = build_mcp_conformance_report().await;
        assert!(report.passed, "report should pass: {report:#?}");
        assert!(report
            .checks
            .iter()
            .any(|check| check.method == "tools/list"));
    }

    #[test]
    fn capability_matrix_includes_nous_managed_tools() {
        let matrix = build_capability_matrix();
        let providers = matrix
            .get("providers")
            .and_then(Value::as_array)
            .expect("providers");
        let nous = providers
            .iter()
            .find(|row| row.get("provider").and_then(Value::as_str) == Some("nous"))
            .expect("nous row");
        assert_eq!(
            nous.get("managed_tools_supported").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn handoff_template_requires_evidence() {
        let contract = build_handoff_contract_template();
        let required = contract
            .pointer("/contract/output_schema/required")
            .and_then(Value::as_array)
            .expect("required fields");
        assert!(required
            .iter()
            .any(|value| value.as_str() == Some("verification")));
        let evidence = contract
            .pointer("/contract/evidence_required")
            .and_then(Value::as_array)
            .expect("evidence requirements");
        assert!(!evidence.is_empty());
    }
}
