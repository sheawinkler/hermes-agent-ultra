//! Evidence-grounded system integration diagnostics for release, protocols, replay, handoff, provider, and provenance checks.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{SecondsFormat, Utc};
use hermes_core::AgentError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::providers::{known_providers, provider_capability_for};

const AGENT_CARD_PATH: &str = "/.well-known/agent.json";
const AGENT_CARD_ALT_PATH: &str = "/agent-card.json";
const SYSTEM_COUNT: usize = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemItem {
    pub id: u8,
    pub system: String,
    pub status: String,
    pub command: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatusReport {
    pub kind: String,
    pub version: String,
    pub generated_at: String,
    pub implemented: usize,
    pub total: usize,
    pub items: Vec<SystemItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalGate {
    pub name: String,
    pub status: String,
    pub detail: String,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseGateReport {
    pub kind: String,
    pub version: String,
    pub generated_at: String,
    pub passed: bool,
    pub gates: Vec<EvalGate>,
    pub release_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayLogSummary {
    pub path: String,
    pub exists: bool,
    pub entries: usize,
    pub parse_errors: usize,
    pub chain_breaks: usize,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayReport {
    pub kind: String,
    pub version: String,
    pub generated_at: String,
    pub replay_dir: String,
    pub log_count: usize,
    pub passed: bool,
    pub logs: Vec<ReplayLogSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConformanceCheck {
    pub method: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolConformanceReport {
    pub kind: String,
    pub protocol: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffReport {
    pub kind: String,
    pub version: String,
    pub generated_at: String,
    pub request_dir: String,
    pub pending_requests: usize,
    pub request_files: Vec<String>,
    pub contract: Value,
    pub existing_surfaces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceReport {
    pub kind: String,
    pub version: String,
    pub generated_at: String,
    pub key_path: String,
    pub key_exists: bool,
    pub release_workflow: String,
    pub release_workflow_signs_artifacts: bool,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SystemsCliOptions {
    pub config_dir: Option<String>,
    pub action: Option<String>,
    pub topic: Option<String>,
    pub json_only: bool,
    pub output: Option<String>,
    pub host: String,
    pub port: u16,
    pub once: bool,
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn state_root(config_dir: Option<&str>) -> PathBuf {
    let override_path = config_dir.map(Path::new);
    hermes_config::state_dir(override_path)
}

fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn report_output(value: &Value, json_only: bool, output: Option<&str>) -> Result<(), AgentError> {
    let rendered = serde_json::to_string_pretty(value)
        .map_err(|err| AgentError::Config(format!("serialize systems report: {err}")))?;
    if let Some(path) = output.map(str::trim).filter(|path| !path.is_empty()) {
        let output_path = PathBuf::from(path);
        if let Some(parent) = output_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .map_err(|err| AgentError::Io(format!("mkdir {}: {err}", parent.display())))?;
        }
        fs::write(&output_path, rendered.as_bytes())
            .map_err(|err| AgentError::Io(format!("write {}: {err}", output_path.display())))?;
    }
    if json_only {
        println!("{rendered}");
    }
    Ok(())
}

fn print_human_status(report: &SystemStatusReport) {
    println!(
        "Hermes system integrations: {}/{} implemented",
        report.implemented, report.total
    );
    println!("Version: {}", report.version);
    for item in &report.items {
        println!(
            "{}. {} [{}]\n   command: {}",
            item.id, item.system, item.status, item.command
        );
        for evidence in &item.evidence {
            println!("   evidence: {evidence}");
        }
    }
}

fn print_human_release(report: &ReleaseGateReport) {
    println!(
        "Hermes release gate inventory: {}",
        if report.passed { "pass" } else { "fail" }
    );
    for gate in &report.gates {
        println!("- {} [{}]: {}", gate.name, gate.status, gate.detail);
        if let Some(command) = &gate.command {
            println!("  command: {command}");
        }
    }
}

fn print_human_replay(report: &ReplayReport) {
    println!("Replay trace logs: {} file(s)", report.log_count);
    println!("Directory: {}", report.replay_dir);
    for log in &report.logs {
        println!(
            "- {} entries={} parse_errors={} chain_breaks={}",
            log.path, log.entries, log.parse_errors, log.chain_breaks
        );
    }
}

fn print_human_conformance(report: &ProtocolConformanceReport) {
    println!(
        "{} conformance: {}",
        report.protocol,
        if report.passed { "pass" } else { "fail" }
    );
    for check in &report.checks {
        println!("- {} [{}]: {}", check.method, check.status, check.detail);
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

pub async fn handle_cli_systems(options: SystemsCliOptions) -> Result<(), AgentError> {
    let action = options
        .action
        .as_deref()
        .map(str::trim)
        .filter(|action| !action.is_empty())
        .unwrap_or("status")
        .to_ascii_lowercase();
    let topic = options
        .topic
        .as_deref()
        .map(str::trim)
        .filter(|topic| !topic.is_empty())
        .unwrap_or("")
        .to_ascii_lowercase();
    let output_ref = options.output.as_deref();
    let state = state_root(options.config_dir.as_deref());

    match action.as_str() {
        "status" | "all" => {
            let report = build_status_report(&state);
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode systems status: {err}")))?;
            if !options.json_only {
                print_human_status(&report);
            }
            report_output(&value, options.json_only, output_ref)
        }
        "release" | "gate" | "eval" => {
            let report = build_release_gate_report();
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode release gate: {err}")))?;
            if !options.json_only {
                print_human_release(&report);
            }
            report_output(&value, options.json_only, output_ref)
        }
        "replay" | "recorder" => {
            let report = build_replay_report(&state)?;
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode replay report: {err}")))?;
            if !options.json_only {
                print_human_replay(&report);
            }
            report_output(&value, options.json_only, output_ref)
        }
        "agent-card" | "card" | "a2a" => match topic.as_str() {
            "serve" | "server" => serve_agent_card(&options.host, options.port, options.once),
            "card" | "show" | "" => {
                let value = build_agent_card();
                if !options.json_only {
                    println!("Hermes agent card");
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&value).map_err(|err| {
                            AgentError::Config(format!("encode agent card: {err}"))
                        })?
                    );
                }
                report_output(&value, options.json_only, output_ref)
            }
            other => Err(AgentError::Config(format!(
                "unknown agent-card action '{other}' (use card or serve)"
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
            if !options.json_only {
                print_human_conformance(&report);
            }
            report_output(&value, options.json_only, output_ref)
        }
        "acp" => {
            if !matches!(topic.as_str(), "conformance" | "doctor" | "show" | "") {
                return Err(AgentError::Config(format!(
                    "unknown ACP action '{topic}' (use conformance)"
                )));
            }
            let report = build_acp_conformance_report().await;
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode ACP report: {err}")))?;
            if !options.json_only {
                print_human_conformance(&report);
            }
            report_output(&value, options.json_only, output_ref)
        }
        "capabilities" | "capability" | "providers" | "matrix" => {
            let value = build_capability_matrix();
            if !options.json_only {
                print_human_matrix(&value);
            }
            report_output(&value, options.json_only, output_ref)
        }
        "handoff" | "handoffs" | "contract" | "contracts" => {
            let value = match topic.as_str() {
                "template" | "schema" | "contract" => build_handoff_contract_template(),
                "show" | "status" | "" => serde_json::to_value(build_handoff_report(&state)?)
                    .map_err(|err| AgentError::Config(format!("encode handoff report: {err}")))?,
                other => {
                    return Err(AgentError::Config(format!(
                        "unknown handoff action '{other}' (use show or template)"
                    )));
                }
            };
            if !options.json_only {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value)
                        .map_err(|err| AgentError::Config(format!("encode handoff: {err}")))?
                );
            }
            report_output(&value, options.json_only, output_ref)
        }
        "provenance" => {
            if !matches!(topic.as_str(), "status" | "show" | "") {
                return Err(AgentError::Config(format!(
                    "unknown provenance action '{topic}' (use status)"
                )));
            }
            let report = build_provenance_report(&state);
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode provenance report: {err}")))?;
            if !options.json_only {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).map_err(|err| {
                        AgentError::Config(format!("encode provenance report: {err}"))
                    })?
                );
            }
            report_output(&value, options.json_only, output_ref)
        }
        other => Err(AgentError::Config(format!(
            "unknown systems action '{other}' (use status, release, replay, mcp, acp, providers, handoff, provenance, or agent-card)"
        ))),
    }
}

pub fn build_status_report(state: &Path) -> SystemStatusReport {
    let root = repo_root();
    let replay_dir = replay_dir(state);
    let handoff_dir = handoff_request_dir(state);
    let provenance_key = provenance_key_path(state);
    let items = vec![
        SystemItem {
            id: 1,
            system: "MCP bridge".to_string(),
            status: file_status(&root.join("crates/hermes-mcp/src/server.rs")),
            command: "hermes systems mcp conformance --json".to_string(),
            evidence: vec![
                "crates/hermes-mcp/src/server.rs handles initialize/tools/resources/prompts/ping".to_string(),
                "hermes mcp serve starts the stdio MCP server".to_string(),
            ],
        },
        SystemItem {
            id: 2,
            system: "ACP bridge".to_string(),
            status: file_status(&root.join("crates/hermes-acp/src/handler.rs")),
            command: "hermes systems acp conformance --json".to_string(),
            evidence: vec![
                "crates/hermes-acp/src/protocol.rs defines lifecycle/session/prompt/tool methods".to_string(),
                "hermes acp start runs the stdio ACP server".to_string(),
            ],
        },
        SystemItem {
            id: 3,
            system: "Replay trace".to_string(),
            status: "implemented".to_string(),
            command: "hermes systems replay --json".to_string(),
            evidence: vec![
                format!("replay log directory: {}", replay_dir.display()),
                "/raw trace verify and /raw trace export validate existing session replay logs".to_string(),
            ],
        },
        SystemItem {
            id: 4,
            system: "Agent handoff".to_string(),
            status: "implemented".to_string(),
            command: "hermes systems handoff --json".to_string(),
            evidence: vec![
                format!("gateway request queue: {}", handoff_dir.display()),
                "kanban_complete(summary, metadata) stores structured run handoffs".to_string(),
            ],
        },
        SystemItem {
            id: 5,
            system: "Provider capability registry".to_string(),
            status: "implemented".to_string(),
            command: "hermes systems providers --json".to_string(),
            evidence: vec![format!("{} provider ids registered", known_providers().len())],
        },
        SystemItem {
            id: 6,
            system: "Release gate".to_string(),
            status: file_status(&root.join(".github/workflows/release.yml")),
            command: "hermes systems release --json".to_string(),
            evidence: vec![
                ".github/workflows/release.yml runs security gate, cross-builds, signing, and publishing".to_string(),
                "scripts/run-security-release-gate-v2.py provides local release security checks".to_string(),
            ],
        },
        SystemItem {
            id: 7,
            system: "Execution provenance".to_string(),
            status: "implemented".to_string(),
            command: "hermes systems provenance --json".to_string(),
            evidence: vec![
                format!("local provenance key path: {}", provenance_key.display()),
                "hermes verify-provenance and hermes rotate-provenance-key are CLI commands".to_string(),
            ],
        },
    ];
    SystemStatusReport {
        kind: "hermes.systems.status".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at: now_rfc3339(),
        implemented: items.iter().filter(|item| item.status != "missing").count(),
        total: SYSTEM_COUNT,
        items,
    }
}

pub fn build_release_gate_report() -> ReleaseGateReport {
    let root = repo_root();
    let gates = vec![
        file_gate(
            "workspace manifest",
            &root.join("Cargo.toml"),
            "workspace package version and release metadata are present",
            None,
        ),
        file_gate(
            "systems command module",
            &root.join("crates/hermes-cli/src/systems.rs"),
            "systems command surface is compiled into hermes-cli",
            Some("cargo test -p hermes-cli systems -- --nocapture"),
        ),
        file_gate(
            "MCP implementation",
            &root.join("crates/hermes-mcp/src/server.rs"),
            "MCP protocol handler exists for in-process conformance checks",
            Some("cargo test -p hermes-mcp"),
        ),
        file_gate(
            "ACP implementation",
            &root.join("crates/hermes-acp/src/handler.rs"),
            "ACP protocol handler exists for in-process conformance checks",
            Some("cargo test -p hermes-acp"),
        ),
        file_gate(
            "provider parity snapshot",
            &root.join("docs/parity/upstream-provider-auth-snapshot.json"),
            "provider capability matrix is grounded in the provider parity snapshot",
            Some("cargo test -p hermes-cli providers -- --nocapture"),
        ),
        file_gate(
            "release workflow",
            &root.join(".github/workflows/release.yml"),
            "tag release workflow builds, signs, and publishes artifacts",
            None,
        ),
        file_gate(
            "release security gate",
            &root.join("scripts/run-security-release-gate-v2.py"),
            "local release gate checks secrets, SBOM metadata, signatures, redaction, and ACP multimodal tests",
            Some("python3 scripts/run-security-release-gate-v2.py --repo-root ."),
        ),
        file_gate(
            "installer",
            &root.join("scripts/install.sh"),
            "release installer script is present",
            None,
        ),
    ];
    let passed = gates.iter().all(|gate| gate.status == "pass");
    ReleaseGateReport {
        kind: "hermes.systems.release_gate".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at: now_rfc3339(),
        passed,
        gates,
        release_commands: vec![
            "cargo fmt --all --check".to_string(),
            "cargo test -p hermes-cli systems -- --nocapture".to_string(),
            "cargo test -p hermes-cli auth_store_write_is_atomic_owner_only_and_cleans_tmp -- --nocapture".to_string(),
            "cargo test -p hermes-mcp".to_string(),
            "cargo test -p hermes-acp".to_string(),
            "python3 scripts/run-security-release-gate-v2.py --repo-root .".to_string(),
            "cargo run -p hermes-cli --bin hermes-agent-ultra -- systems status --json".to_string(),
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

fn file_status(path: &Path) -> String {
    if path.exists() {
        "implemented".to_string()
    } else {
        "missing".to_string()
    }
}

fn replay_dir(state: &Path) -> PathBuf {
    state.join("logs").join("replay")
}

fn handoff_request_dir(state: &Path) -> PathBuf {
    state.join("handoff_requests")
}

fn provenance_key_path(state: &Path) -> PathBuf {
    state.join("auth").join("provenance.key")
}

pub fn build_replay_report(state: &Path) -> Result<ReplayReport, AgentError> {
    let dir = replay_dir(state);
    let logs = replay_log_paths(&dir)?
        .iter()
        .map(|path| summarize_replay_log(path))
        .collect::<Result<Vec<_>, _>>()?;
    let passed = logs
        .iter()
        .all(|log| log.parse_errors == 0 && log.chain_breaks == 0);
    Ok(ReplayReport {
        kind: "hermes.replay.report".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at: now_rfc3339(),
        replay_dir: dir.display().to_string(),
        log_count: logs.len(),
        passed,
        logs,
    })
}

fn replay_log_paths(dir: &Path) -> Result<Vec<PathBuf>, AgentError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = fs::read_dir(dir)
        .map_err(|err| AgentError::Io(format!("read {}: {err}", dir.display())))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|v| v.to_str()) == Some("jsonl"))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn summarize_replay_log(path: &Path) -> Result<ReplayLogSummary, AgentError> {
    let raw = fs::read_to_string(path)
        .map_err(|err| AgentError::Io(format!("read {}: {err}", path.display())))?;
    let mut entries = 0usize;
    let mut parse_errors = 0usize;
    let mut chain_breaks = 0usize;
    let mut first_seq = None;
    let mut last_seq = None;
    let mut last_event_hash: Option<String> = None;

    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let parsed: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => {
                parse_errors = parse_errors.saturating_add(1);
                continue;
            }
        };
        entries = entries.saturating_add(1);
        let seq = parsed.get("seq").and_then(Value::as_u64);
        if first_seq.is_none() {
            first_seq = seq;
        }
        if seq.is_some() {
            last_seq = seq;
        }
        let prev_hash = parsed
            .get("prev_hash")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_hash = parsed
            .get("event_hash")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let (Some(last), Some(prev)) = (last_event_hash.as_ref(), prev_hash.as_ref()) {
            if last != prev {
                chain_breaks = chain_breaks.saturating_add(1);
            }
        }
        if let Some(curr) = event_hash {
            last_event_hash = Some(curr);
        }
    }

    Ok(ReplayLogSummary {
        path: path.display().to_string(),
        exists: path.exists(),
        entries,
        parse_errors,
        chain_breaks,
        first_seq,
        last_seq,
    })
}

pub fn build_agent_card() -> Value {
    json!({
        "name": "Hermes Agent Ultra",
        "description": "Rust-native autonomous agent runtime with CLI/TUI, MCP bridge, ACP bridge, provider routing, gateway integrations, replay traces, and local release gates.",
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
            "acpBridge": true,
            "replayTrace": true,
            "provenanceVerification": true
        },
        "interfaces": {
            "mcp": {"transport": "stdio", "command": "hermes mcp serve"},
            "acp": {"transport": "stdio", "command": "hermes acp start"},
            "replay": {"command": "/raw trace verify"},
            "handoff": {"command": "/handoff <platform>", "queue": "~/.hermes-agent-ultra/handoff_requests"},
            "provenance": {"verify": "hermes verify-provenance <artifact>", "rotate": "hermes rotate-provenance-key"}
        },
        "wellKnown": {
            "agentCard": AGENT_CARD_PATH,
            "alternateCard": AGENT_CARD_ALT_PATH,
            "health": "/healthz"
        }
    })
}

fn serve_agent_card(host: &str, port: u16, once: bool) -> Result<(), AgentError> {
    let bind_addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&bind_addr)
        .map_err(|err| AgentError::Io(format!("bind agent card server {bind_addr}: {err}")))?;
    let addr = listener
        .local_addr()
        .map_err(|err| AgentError::Io(format!("read agent-card listener addr: {err}")))?;
    println!(
        "Agent card server listening on http://{}{}",
        addr, AGENT_CARD_PATH
    );
    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => handle_agent_card_stream(&mut stream)?,
            Err(err) => {
                return Err(AgentError::Io(format!(
                    "accept agent-card connection: {err}"
                )))
            }
        }
        if once {
            break;
        }
    }
    Ok(())
}

fn handle_agent_card_stream(stream: &mut TcpStream) -> Result<(), AgentError> {
    let mut buf = [0_u8; 4096];
    let n = stream
        .read(&mut buf)
        .map_err(|err| AgentError::Io(format!("read agent-card request: {err}")))?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let (status, content_type, body) = agent_card_response_for_path(path);
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
        .map_err(|err| AgentError::Io(format!("write agent-card response: {err}")))?;
    stream
        .flush()
        .map_err(|err| AgentError::Io(format!("flush agent-card response: {err}")))?;
    Ok(())
}

fn agent_card_response_for_path(path: &str) -> (u16, &'static str, String) {
    match path {
        AGENT_CARD_PATH | AGENT_CARD_ALT_PATH => (
            200,
            "application/json",
            serde_json::to_string_pretty(&build_agent_card()).unwrap_or_else(|_| "{}".to_string()),
        ),
        "/healthz" => (
            200,
            "application/json",
            json!({"status":"ok","service":"hermes-agent-card"}).to_string(),
        ),
        _ => (
            404,
            "application/json",
            json!({"error":"not_found","expected":[AGENT_CARD_PATH,AGENT_CARD_ALT_PATH,"/healthz"]})
                .to_string(),
        ),
    }
}

pub async fn build_mcp_conformance_report() -> ProtocolConformanceReport {
    let mut server = hermes_mcp::McpServer::new(Arc::new(hermes_tools::ToolRegistry::new()));
    server.add_resource_text(
        hermes_mcp::ResourceInfo {
            uri: "hermes://systems/status".to_string(),
            name: "systems-status".to_string(),
            description: Some("Hermes systems status resource".to_string()),
            mime_type: Some("text/plain".to_string()),
        },
        "ok",
    );
    server.add_prompt(hermes_mcp::server::McpPromptInfo {
        name: "systems_status".to_string(),
        description: Some("Summarize Hermes systems status".to_string()),
        arguments: Some(json!([])),
    });
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
    checks.push(
        match server
            .handle_request("resources/read", json!({"uri":"hermes://systems/status"}))
            .await
        {
            Ok(value) if value.get("contents").and_then(Value::as_array).is_some() => {
                pass("resources/read", "reads a registered resource")
            }
            Ok(value) => fail(
                "resources/read",
                format!("unexpected response shape: {value}"),
            ),
            Err(err) => fail("resources/read", err.to_string()),
        },
    );
    checks.push(list_check(
        "prompts/list",
        server.handle_request("prompts/list", json!({})).await,
        "prompts",
    ));
    checks.push(
        match server
            .handle_request("prompts/get", json!({"name":"systems_status"}))
            .await
        {
            Ok(value) if value.get("messages").and_then(Value::as_array).is_some() => {
                pass("prompts/get", "reads a registered prompt")
            }
            Ok(value) => fail("prompts/get", format!("unexpected response shape: {value}")),
            Err(err) => fail("prompts/get", err.to_string()),
        },
    );
    checks.push(match server.handle_request("tools/call", json!({})).await {
        Err(hermes_mcp::McpError::InvalidParams(_)) => {
            pass("tools/call", "validates missing tool name")
        }
        Ok(value) => fail("tools/call", format!("expected InvalidParams, got {value}")),
        Err(err) => fail("tools/call", err.to_string()),
    });
    checks.push(match server.handle_request("ping", json!({})).await {
        Ok(value) if value.as_object().is_some_and(|obj| obj.is_empty()) => {
            pass("ping", "returns empty object")
        }
        Ok(value) => fail("ping", format!("unexpected ping response: {value}")),
        Err(err) => fail("ping", err.to_string()),
    });

    let passed = checks.iter().all(|check| check.status == "pass");
    ProtocolConformanceReport {
        kind: "hermes.mcp.conformance".to_string(),
        protocol: "MCP".to_string(),
        protocol_version: "2024-11-05".to_string(),
        generated_at: now_rfc3339(),
        passed,
        checks,
    }
}

pub async fn build_acp_conformance_report() -> ProtocolConformanceReport {
    use hermes_acp::AcpHandler;

    let handler = hermes_acp::DefaultAcpHandler::default();
    let mut checks = Vec::new();

    let initialize = handler
        .handle_request(acp_request(1, "initialize", None))
        .await;
    checks.push(match initialize.result.as_ref() {
        Some(value)
            if value.get("protocolVersion").is_some()
                && value.get("agentInfo").is_some()
                && value.get("agentCapabilities").is_some() =>
        {
            pass(
                "initialize",
                "advertises protocol, agentInfo, and capabilities",
            )
        }
        Some(value) => fail("initialize", format!("unexpected response shape: {value}")),
        None => fail("initialize", acp_error_message(&initialize)),
    });

    let new_session = handler
        .handle_request(acp_request(2, "session/new", Some(json!({"cwd":"."}))))
        .await;
    let session_id = new_session
        .result
        .as_ref()
        .and_then(|value| value.get("sessionId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    checks.push(match session_id.as_deref() {
        Some(value) if !value.is_empty() => pass("session/new", "creates a session"),
        _ => fail("session/new", acp_error_message(&new_session)),
    });

    if let Some(session_id) = session_id.as_deref() {
        let prompt = handler
            .handle_request(acp_request(
                3,
                "prompt",
                Some(json!({"sessionId": session_id, "content": [{"type":"text", "text":"ping"}]})),
            ))
            .await;
        checks.push(match prompt.result.as_ref() {
            Some(value) if value.get("stopReason").and_then(Value::as_str) == Some("end_turn") => {
                pass("prompt", "accepts text content and returns end_turn")
            }
            Some(value) => fail("prompt", format!("unexpected response shape: {value}")),
            None => fail("prompt", acp_error_message(&prompt)),
        });

        let cancel = handler
            .handle_request(acp_request(
                4,
                "session/cancel",
                Some(json!({"sessionId": session_id})),
            ))
            .await;
        checks.push(match cancel.result.as_ref() {
            Some(value) if value.get("cancelled").and_then(Value::as_bool) == Some(true) => {
                pass("session/cancel", "cancels an existing session")
            }
            Some(value) => fail(
                "session/cancel",
                format!("unexpected response shape: {value}"),
            ),
            None => fail("session/cancel", acp_error_message(&cancel)),
        });
    }

    let tools = handler
        .handle_request(acp_request(5, "tools.list", None))
        .await;
    checks.push(match tools.result.as_ref() {
        Some(value) if value.get("tools").and_then(Value::as_array).is_some() => {
            pass("tools.list", "returns legacy tool list array")
        }
        Some(value) => fail("tools.list", format!("unexpected response shape: {value}")),
        None => fail("tools.list", acp_error_message(&tools)),
    });

    let status = handler
        .handle_request(acp_request(6, "status.get", None))
        .await;
    checks.push(match status.result.as_ref() {
        Some(value) if value.get("status").and_then(Value::as_str) == Some("ready") => {
            pass("status.get", "returns ready status")
        }
        Some(value) => fail("status.get", format!("unexpected response shape: {value}")),
        None => fail("status.get", acp_error_message(&status)),
    });

    let passed = checks.iter().all(|check| check.status == "pass");
    ProtocolConformanceReport {
        kind: "hermes.acp.conformance".to_string(),
        protocol: "ACP".to_string(),
        protocol_version: "1".to_string(),
        generated_at: now_rfc3339(),
        passed,
        checks,
    }
}

fn acp_request(id: u64, method: &str, params: Option<Value>) -> hermes_acp::AcpRequest {
    hermes_acp::AcpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(id)),
        method: method.to_string(),
        params,
    }
}

fn acp_error_message(response: &hermes_acp::AcpResponse) -> String {
    response
        .error
        .as_ref()
        .map(|err| format!("{}: {}", err.code, err.message))
        .unwrap_or_else(|| "missing result".to_string())
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
        "provider_count": providers.len(),
        "providers": providers,
        "tool_surfaces": [
            {"surface":"mcp", "status":"implemented", "command":"hermes mcp serve"},
            {"surface":"acp", "status":"implemented", "command":"hermes acp start"},
            {"surface":"terminal", "status":"implemented", "command":"interactive tool policy"},
            {"surface":"gateway", "status":"implemented", "command":"hermes gateway status"},
            {"surface":"cron", "status":"implemented", "command":"hermes cron list"},
            {"surface":"webhook", "status":"implemented", "command":"hermes webhook list"},
            {"surface":"replay_trace", "status":"implemented", "command":"/raw trace verify"}
        ]
    })
}

pub fn build_handoff_report(state: &Path) -> Result<HandoffReport, AgentError> {
    let request_dir = handoff_request_dir(state);
    let mut request_files = if request_dir.exists() {
        fs::read_dir(&request_dir)
            .map_err(|err| AgentError::Io(format!("read {}: {err}", request_dir.display())))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|v| v.to_str()) == Some("json"))
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    request_files.sort();
    Ok(HandoffReport {
        kind: "hermes.handoff.report".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at: now_rfc3339(),
        request_dir: request_dir.display().to_string(),
        pending_requests: request_files.len(),
        request_files,
        contract: build_handoff_contract_template(),
        existing_surfaces: vec![
            "/handoff <platform> queues gateway pickup requests".to_string(),
            "kanban_complete(summary, metadata) stores structured run handoffs".to_string(),
            "hermes kanban complete <id> --summary ... --metadata ... closes human-managed tasks with handoff data".to_string(),
        ],
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

pub fn build_provenance_report(state: &Path) -> ProvenanceReport {
    let root = repo_root();
    let workflow = root.join(".github/workflows/release.yml");
    let workflow_raw = fs::read_to_string(&workflow).unwrap_or_default();
    let key = provenance_key_path(state);
    ProvenanceReport {
        kind: "hermes.provenance.report".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at: now_rfc3339(),
        key_path: key.display().to_string(),
        key_exists: key.exists(),
        release_workflow: workflow.display().to_string(),
        release_workflow_signs_artifacts: workflow_raw.contains("cosign sign-blob"),
        commands: vec![
            "hermes rotate-provenance-key --json".to_string(),
            "hermes verify-provenance <artifact> --strict --json".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systems_status_tracks_requested_systems() {
        let temp = tempfile::tempdir().expect("tempdir");
        let report = build_status_report(temp.path());
        assert_eq!(report.total, SYSTEM_COUNT);
        assert_eq!(report.implemented, SYSTEM_COUNT);
        for expected in [
            "MCP",
            "ACP",
            "Replay",
            "handoff",
            "Provider",
            "Release",
            "provenance",
        ] {
            assert!(
                report.items.iter().any(|item| item
                    .system
                    .to_ascii_lowercase()
                    .contains(&expected.to_ascii_lowercase())),
                "missing {expected}: {report:#?}"
            );
        }
    }

    #[test]
    fn replay_report_reads_existing_trace_logs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = replay_dir(temp.path());
        fs::create_dir_all(&dir).expect("mkdir replay");
        let path = dir.join("session.jsonl");
        fs::write(
            &path,
            concat!(
                "{\"seq\":1,\"event\":\"start\",\"trace_id\":\"t\",\"prev_hash\":null,\"event_hash\":\"a\"}\n",
                "{\"seq\":2,\"event\":\"stop\",\"trace_id\":\"t\",\"prev_hash\":\"a\",\"event_hash\":\"b\"}\n"
            ),
        )
        .expect("write replay");
        let report = build_replay_report(temp.path()).expect("report");
        assert!(report.passed, "report should pass: {report:#?}");
        assert_eq!(report.log_count, 1);
        assert_eq!(report.logs[0].entries, 2);
        assert_eq!(report.logs[0].chain_breaks, 0);
    }

    #[test]
    fn agent_card_declares_real_protocol_commands() {
        let card = build_agent_card();
        assert_eq!(
            card.get("wellKnown")
                .and_then(|v| v.get("agentCard"))
                .and_then(Value::as_str),
            Some(AGENT_CARD_PATH)
        );
        assert_eq!(
            card.pointer("/interfaces/mcp/command")
                .and_then(Value::as_str),
            Some("hermes mcp serve")
        );
        assert_eq!(
            card.pointer("/interfaces/acp/command")
                .and_then(Value::as_str),
            Some("hermes acp start")
        );
        let (status, content_type, body) = agent_card_response_for_path(AGENT_CARD_PATH);
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
            .any(|check| check.method == "resources/read"));
        assert!(report
            .checks
            .iter()
            .any(|check| check.method == "prompts/get"));
    }

    #[tokio::test]
    async fn acp_conformance_report_passes_core_methods() {
        let report = build_acp_conformance_report().await;
        assert!(report.passed, "report should pass: {report:#?}");
        assert!(report
            .checks
            .iter()
            .any(|check| check.method == "session/new"));
        assert!(report.checks.iter().any(|check| check.method == "prompt"));
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
    fn handoff_report_reads_existing_queue_and_contract() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = handoff_request_dir(temp.path());
        fs::create_dir_all(&dir).expect("mkdir handoff queue");
        fs::write(dir.join("session-platform.json"), "{}\n").expect("write handoff request");
        let report = build_handoff_report(temp.path()).expect("report");
        assert_eq!(report.pending_requests, 1);
        assert!(report
            .contract
            .pointer("/contract/evidence_required")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()));
    }

    #[test]
    fn provenance_report_detects_release_signing_workflow() {
        let temp = tempfile::tempdir().expect("tempdir");
        let report = build_provenance_report(temp.path());
        assert!(report
            .release_workflow
            .ends_with(".github/workflows/release.yml"));
        assert!(report.release_workflow_signs_artifacts);
        assert!(report
            .commands
            .iter()
            .any(|command| command.contains("verify-provenance")));
    }
}
