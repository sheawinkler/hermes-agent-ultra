//! Evidence-grounded system integration diagnostics for release, protocols, replay, handoff, provider, and provenance checks.

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{SecondsFormat, Utc};
use hermes_core::AgentError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::providers::{known_providers, provider_capability_for};

const A2A_AVAILABILITY_PATH: &str = "/a2a-availability.json";
const SYSTEM_COUNT: usize = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemItem {
    pub id: u8,
    pub system: String,
    pub availability: String,
    pub command: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatusReport {
    pub kind: String,
    pub version: String,
    pub generated_at: String,
    pub authority: String,
    pub production_completeness_assessed: bool,
    pub available_surfaces: usize,
    pub total: usize,
    pub items: Vec<SystemItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseEvidenceItem {
    pub name: String,
    pub status: String,
    pub detail: String,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseEvidenceReport {
    pub kind: String,
    pub version: String,
    pub generated_at: String,
    pub validation_executed: bool,
    pub release_ready: bool,
    pub items: Vec<ReleaseEvidenceItem>,
    pub release_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyReplayLogDiagnostic {
    pub path: String,
    pub exists: bool,
    pub entries: usize,
    pub parse_errors: usize,
    pub chain_breaks: usize,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    pub read_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyReplayDiagnostics {
    pub directory: String,
    pub log_count: usize,
    pub logs: Vec<LegacyReplayLogDiagnostic>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealedArtifactAssessment {
    pub digest: String,
    pub path: String,
    pub valid: bool,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunExportAssessment {
    pub path: String,
    pub structurally_verified: bool,
    pub verifier_evidence_complete: bool,
    pub legacy_delivery_result_count: Option<u64>,
    pub evidence_complete: bool,
    pub run_id: Option<String>,
    pub event_count: Option<u64>,
    pub outbox_count: Option<u64>,
    pub sealed_artifacts: Vec<SealedArtifactAssessment>,
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunExportReport {
    pub kind: String,
    pub version: String,
    pub generated_at: String,
    pub exports_dir: String,
    pub artifacts_dir: String,
    pub assessed_export_count: usize,
    pub evidence_complete: bool,
    pub passed: bool,
    pub exports: Vec<RunExportAssessment>,
    pub assessment_errors: Vec<String>,
    pub diagnostic_only: LegacyReplayDiagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolDiagnosticCheck {
    pub method: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolDiagnosticReport {
    pub kind: String,
    pub protocol: String,
    pub protocol_version: String,
    pub generated_at: String,
    pub scope: String,
    pub local_checks_passed: bool,
    pub external_client_interoperability_verified: bool,
    pub production_complete: bool,
    pub checks: Vec<ProtocolDiagnosticCheck>,
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

fn source_tree_root() -> Option<PathBuf> {
    let root = repo_root();
    source_tree_is_available(&root).then_some(root)
}

fn source_tree_is_available(root: &Path) -> bool {
    root.join("Cargo.toml").is_file() && root.join("crates/hermes-cli/src/systems.rs").is_file()
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
        "Hermes system-surface availability: {}/{} observed",
        report.available_surfaces, report.total
    );
    println!(
        "Authority: {}; production_completeness_assessed={}",
        report.authority, report.production_completeness_assessed
    );
    println!("Version: {}", report.version);
    for item in &report.items {
        println!(
            "{}. {} [{}]\n   command: {}",
            item.id, item.system, item.availability, item.command
        );
        for evidence in &item.evidence {
            println!("   evidence: {evidence}");
        }
    }
}

fn print_human_release(report: &ReleaseEvidenceReport) {
    println!(
        "Hermes release evidence inventory: validation_executed={}; release_ready={}",
        report.validation_executed, report.release_ready
    );
    for item in &report.items {
        println!("- {} [{}]: {}", item.name, item.status, item.detail);
        if let Some(command) = &item.command {
            println!("  validation command (not executed here): {command}");
        }
    }
}

fn print_human_run_export(report: &RunExportReport) {
    let sealed_artifact_count = report
        .exports
        .iter()
        .map(|export| export.sealed_artifacts.len())
        .sum::<usize>();
    println!(
        "Run-export proof: exports_assessed={}; evidence_complete={}; passed={}",
        report.assessed_export_count, report.evidence_complete, report.passed
    );
    println!("Exports directory: {}", report.exports_dir);
    println!(
        "Sealed artifacts directory: {}; assessed={sealed_artifact_count}",
        report.artifacts_dir
    );
    for error in &report.assessment_errors {
        println!("- assessment failure: {error}");
    }
    for export in &report.exports {
        println!(
            "- {} evidence_complete={} structurally_verified={} verifier_evidence_complete={} legacy_delivery_results={:?} run_id={:?} events={:?} outbox={:?}",
            export.path,
            export.evidence_complete,
            export.structurally_verified,
            export.verifier_evidence_complete,
            export.legacy_delivery_result_count,
            export.run_id,
            export.event_count,
            export.outbox_count
        );
        for artifact in &export.sealed_artifacts {
            println!(
                "  artifact {} valid={} path={}",
                artifact.digest, artifact.valid, artifact.path
            );
            if let Some(failure) = &artifact.failure {
                println!("    failure: {failure}");
            }
        }
        for failure in &export.failures {
            println!("  failure: {failure}");
        }
    }
    println!(
        "Legacy JSONL diagnostics only: {} file(s); cannot establish proof",
        report.diagnostic_only.log_count
    );
    println!("Legacy diagnostic directory: {}", report.diagnostic_only.directory);
    for log in &report.diagnostic_only.logs {
        println!(
            "- {} entries={} parse_errors={} chain_breaks={}",
            log.path, log.entries, log.parse_errors, log.chain_breaks
        );
        if let Some(error) = &log.read_error {
            println!("  diagnostic read error: {error}");
        }
    }
    for error in &report.diagnostic_only.errors {
        println!("- diagnostic error: {error}");
    }
}

fn print_human_protocol_diagnostic(report: &ProtocolDiagnosticReport) {
    println!(
        "{} in-process diagnostic: {}",
        report.protocol,
        if report.local_checks_passed {
            "pass"
        } else {
            "fail"
        }
    );
    println!(
        "Scope: {}; external_interoperability_verified={}; production_complete={}",
        report.scope, report.external_client_interoperability_verified, report.production_complete
    );
    for check in &report.checks {
        println!("- {} [{}]: {}", check.method, check.status, check.detail);
    }
}

fn print_human_matrix(value: &Value) {
    println!("Provider metadata and command-surface availability");
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
        "release" | "release-evidence" => {
            let report = build_release_evidence_report();
            let value = serde_json::to_value(&report).map_err(|err| {
                AgentError::Config(format!("encode release evidence inventory: {err}"))
            })?;
            if !options.json_only {
                print_human_release(&report);
            }
            report_output(&value, options.json_only, output_ref)
        }
        "replay" | "recorder" => {
            let report = build_run_export_report(&state);
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode run-export report: {err}")))?;
            if !options.json_only {
                print_human_run_export(&report);
            }
            report_output(&value, options.json_only, output_ref)
        }
        "a2a-availability" => match topic.as_str() {
            "serve" => serve_a2a_availability(&options.host, options.port, options.once),
            "show" | "" => {
                let value = build_a2a_availability_report();
                if !options.json_only {
                    println!("Hermes A2A availability");
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&value).map_err(|err| {
                            AgentError::Config(format!("encode A2A availability report: {err}"))
                        })?
                    );
                }
                report_output(&value, options.json_only, output_ref)
            }
            other => Err(AgentError::Config(format!(
                "unknown A2A availability action '{other}' (use show or serve)"
            ))),
        },
        "mcp" => {
            if !matches!(topic.as_str(), "diagnostic" | "doctor" | "show" | "") {
                return Err(AgentError::Config(format!(
                    "unknown MCP action '{topic}' (use diagnostic)"
                )));
            }
            let report = build_mcp_diagnostic_report().await;
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode MCP report: {err}")))?;
            if !options.json_only {
                print_human_protocol_diagnostic(&report);
            }
            report_output(&value, options.json_only, output_ref)
        }
        "acp" => {
            if !matches!(topic.as_str(), "diagnostic" | "doctor" | "show" | "") {
                return Err(AgentError::Config(format!(
                    "unknown ACP action '{topic}' (use diagnostic)"
                )));
            }
            let report = build_acp_diagnostic_report().await;
            let value = serde_json::to_value(&report)
                .map_err(|err| AgentError::Config(format!("encode ACP report: {err}")))?;
            if !options.json_only {
                print_human_protocol_diagnostic(&report);
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
            "unknown systems action '{other}' (use status, release, replay, mcp, acp, providers, handoff, provenance, or a2a-availability)"
        ))),
    }
}

pub fn build_status_report(state: &Path) -> SystemStatusReport {
    let source_root = source_tree_root();
    build_status_report_for_source_root(state, source_root.as_deref())
}

fn build_status_report_for_source_root(
    state: &Path,
    source_root: Option<&Path>,
) -> SystemStatusReport {
    let exports_dir = run_export_dir(state);
    let handoff_dir = handoff_request_dir(state);
    let provenance_key = provenance_key_path(state);
    let items = vec![
        SystemItem {
            id: 1,
            system: "MCP bridge".to_string(),
            availability: source_file_status(source_root, "crates/hermes-mcp/src/server.rs"),
            command: "hermes systems mcp diagnostic --json".to_string(),
            evidence: vec![
                "crates/hermes-mcp/src/server.rs handles initialize/tools/resources/prompts/ping".to_string(),
                "hermes mcp serve starts the stdio MCP server".to_string(),
            ],
        },
        SystemItem {
            id: 2,
            system: "ACP bridge".to_string(),
            availability: source_file_status(source_root, "crates/hermes-acp/src/handler.rs"),
            command: "hermes systems acp diagnostic --json".to_string(),
            evidence: vec![
                "crates/hermes-acp/src/protocol.rs defines lifecycle/session/prompt/tool methods".to_string(),
                "hermes acp start runs the stdio ACP server".to_string(),
            ],
        },
        SystemItem {
            id: 3,
            system: "Replay run-export evidence".to_string(),
            availability: "command_registered".to_string(),
            command: "hermes systems replay --json".to_string(),
            evidence: vec![
                format!(
                    "authoritative complete export directory: {}",
                    exports_dir.display()
                ),
                "/raw trace export publishes complete per-run exports; /raw trace verify <PATH> verifies JCS exports and sealed artifact CAS".to_string(),
            ],
        },
        SystemItem {
            id: 4,
            system: "Agent handoff".to_string(),
            availability: "command_registered".to_string(),
            command: "hermes systems handoff --json".to_string(),
            evidence: vec![
                format!("gateway request queue: {}", handoff_dir.display()),
                "kanban_complete(summary, metadata) stores structured run handoffs".to_string(),
            ],
        },
        SystemItem {
            id: 5,
            system: "Provider capability registry".to_string(),
            availability: "registry_present".to_string(),
            command: "hermes systems providers --json".to_string(),
            evidence: vec![format!("{} provider ids registered", known_providers().len())],
        },
        SystemItem {
            id: 6,
            system: "Release evidence inventory".to_string(),
            availability: source_file_status(source_root, ".github/workflows/release.yml"),
            command: "hermes systems release --json".to_string(),
            evidence: vec![
                ".github/workflows/release.yml is a source artifact; this report does not execute it".to_string(),
                "scripts/run-security-release-gate-v2.py is listed as an unexecuted validation command".to_string(),
            ],
        },
        SystemItem {
            id: 7,
            system: "Execution provenance".to_string(),
            availability: "command_registered".to_string(),
            command: "hermes systems provenance --json".to_string(),
            evidence: vec![
                format!("local provenance key path: {}", provenance_key.display()),
                "hermes verify-provenance and hermes rotate-provenance-key are CLI commands".to_string(),
            ],
        },
    ];
    SystemStatusReport {
        kind: "hermes.systems.surface_availability".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at: now_rfc3339(),
        authority: "registration_and_source_presence_only".to_string(),
        production_completeness_assessed: false,
        available_surfaces: items
            .iter()
            .filter(|item| {
                !matches!(
                    item.availability.as_str(),
                    "source_missing" | "not_assessed"
                )
            })
            .count(),
        total: SYSTEM_COUNT,
        items,
    }
}

pub fn build_release_evidence_report() -> ReleaseEvidenceReport {
    let source_root = source_tree_root();
    build_release_evidence_report_for_source_root(source_root.as_deref())
}

fn build_release_evidence_report_for_source_root(
    source_root: Option<&Path>,
) -> ReleaseEvidenceReport {
    let gates = vec![
        source_file_gate(
            "workspace manifest",
            source_root,
            "Cargo.toml",
            "workspace package version and release metadata are present",
            None,
        ),
        source_file_gate(
            "systems command module",
            source_root,
            "crates/hermes-cli/src/systems.rs",
            "systems command surface is compiled into hermes-cli",
            Some("cargo test -p hermes-cli systems -- --nocapture"),
        ),
        source_file_gate(
            "MCP implementation",
            source_root,
            "crates/hermes-mcp/src/server.rs",
            "MCP protocol handler exists for in-process conformance checks",
            Some("cargo test -p hermes-mcp"),
        ),
        source_file_gate(
            "ACP implementation",
            source_root,
            "crates/hermes-acp/src/handler.rs",
            "ACP protocol handler exists for in-process conformance checks",
            Some("cargo test -p hermes-acp"),
        ),
        source_file_gate(
            "provider parity snapshot",
            source_root,
            "docs/parity/upstream-provider-auth-snapshot.json",
            "provider capability matrix is grounded in the provider parity snapshot",
            Some("cargo test -p hermes-cli providers -- --nocapture"),
        ),
        source_file_gate(
            "release workflow",
            source_root,
            ".github/workflows/release.yml",
            "tag release workflow builds, signs, and publishes artifacts",
            None,
        ),
        source_file_gate(
            "release security gate",
            source_root,
            "scripts/run-security-release-gate-v2.py",
            "local release gate checks secrets, SBOM metadata, signatures, redaction, and ACP multimodal tests",
            Some("python3 scripts/run-security-release-gate-v2.py --repo-root ."),
        ),
        source_file_gate(
            "installer",
            source_root,
            "scripts/install.sh",
            "release installer script is present",
            None,
        ),
    ];
    ReleaseEvidenceReport {
        kind: "hermes.systems.release_evidence_inventory".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at: now_rfc3339(),
        validation_executed: false,
        release_ready: false,
        items: gates,
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

fn source_file_gate(
    name: &str,
    source_root: Option<&Path>,
    relative_path: &str,
    detail: &str,
    command: Option<&str>,
) -> ReleaseEvidenceItem {
    if let Some(root) = source_root {
        return ReleaseEvidenceItem {
            name: name.to_string(),
            status: if root.join(relative_path).exists() {
                "source_present"
            } else {
                "source_missing"
            }
            .to_string(),
            detail: format!("{detail} ({relative_path})"),
            command: command.map(str::to_string),
        };
    }
    ReleaseEvidenceItem {
        name: name.to_string(),
        status: "not_assessed".to_string(),
        detail: format!(
            "{detail}; source checkout is not present, so this evidence item cannot be inspected"
        ),
        command: command.map(str::to_string),
    }
}

fn file_status(path: &Path) -> String {
    if path.exists() {
        "source_present".to_string()
    } else {
        "source_missing".to_string()
    }
}

fn source_file_status(source_root: Option<&Path>, relative_path: &str) -> String {
    source_root.map_or_else(
        || "not_assessed".to_string(),
        |root| file_status(&root.join(relative_path)),
    )
}

fn legacy_replay_dir(state: &Path) -> PathBuf {
    state.join("logs").join("replay")
}

fn run_export_dir(state: &Path) -> PathBuf {
    state.join("run-record-v1").join("exports")
}

fn sealed_artifact_dir(state: &Path) -> PathBuf {
    state.join("run-record-v1").join("artifacts")
}

fn handoff_request_dir(state: &Path) -> PathBuf {
    state.join("handoff_requests")
}

fn provenance_key_path(state: &Path) -> PathBuf {
    state.join("auth").join("provenance.key")
}

pub fn build_run_export_report(state: &Path) -> RunExportReport {
    let exports_dir = run_export_dir(state);
    let artifacts_dir = sealed_artifact_dir(state);
    let mut assessment_errors = Vec::new();
    let paths = match run_export_paths(&exports_dir) {
        Ok(paths) => paths,
        Err(error) => {
            assessment_errors.push(error);
            Vec::new()
        }
    };
    let exports = paths
        .iter()
        .map(|path| assess_run_export(path, &artifacts_dir))
        .collect::<Vec<_>>();
    let evidence_complete = !exports.is_empty()
        && assessment_errors.is_empty()
        && exports.iter().all(|export| export.evidence_complete);

    RunExportReport {
        kind: "hermes.run-export.report".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        generated_at: now_rfc3339(),
        exports_dir: exports_dir.display().to_string(),
        artifacts_dir: artifacts_dir.display().to_string(),
        assessed_export_count: exports.len(),
        evidence_complete,
        passed: evidence_complete,
        exports,
        assessment_errors,
        diagnostic_only: build_legacy_replay_diagnostics(state),
    }
}

fn run_export_paths(dir: &Path) -> Result<Vec<PathBuf>, String> {
    match fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!("run export directory {} is a symlink", dir.display()));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(format!(
                "run export path {} is not a directory",
                dir.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "inspect run export directory {}: {error}",
                dir.display()
            ));
        }
    }

    let entries =
        fs::read_dir(dir).map_err(|error| format!("read run export directory {}: {error}", dir.display()))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("read entry in run export directory {}: {error}", dir.display()))?;
        let name = entry.file_name();
        if name
            .to_str()
            .map(|name| name.ends_with(".run.json"))
            .unwrap_or(false)
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn assess_run_export(path: &Path, artifacts_dir: &Path) -> RunExportAssessment {
    let mut failures = Vec::new();
    let path_is_safe = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            failures.push("run export path is a symlink".to_string());
            false
        }
        Ok(metadata) if !metadata.is_file() => {
            failures.push("run export path is not a regular file".to_string());
            false
        }
        Ok(_) => true,
        Err(error) => {
            failures.push(format!("inspect run export path: {error}"));
            false
        }
    };

    let mut structurally_verified = false;
    let mut verifier_evidence_complete = false;
    let mut legacy_delivery_result_count = None;
    let mut run_id = None;
    let mut event_count = None;
    let mut outbox_count = None;
    match hermes_agent::run_export::verify_run_export(path) {
        Ok(verified) => {
            structurally_verified = true;
            verifier_evidence_complete = verified.evidence_complete();
            legacy_delivery_result_count = Some(verified.legacy_delivery_result_count);
            run_id = Some(verified.run_id.to_string());
            event_count = Some(verified.event_count);
            outbox_count = Some(verified.outbox_count);
            if !verifier_evidence_complete {
                failures.push(format!(
                    "export has {} legacy evidence-less delivery result(s)",
                    verified.legacy_delivery_result_count
                ));
            }
        }
        Err(error) => failures.push(format!("run export verifier failed: {error}")),
    }

    let mut sealed_artifacts = Vec::new();
    if structurally_verified && path_is_safe {
        match sealed_artifact_digests_from_verified_export(path) {
            Ok(digests) => {
                sealed_artifacts = digests
                    .iter()
                    .map(|digest| assess_sealed_artifact(artifacts_dir, digest))
                    .collect();
            }
            Err(error) => failures.push(format!("verified export parse failed: {error}")),
        }
    }
    let evidence_complete = structurally_verified
        && verifier_evidence_complete
        && failures.is_empty()
        && sealed_artifacts.iter().all(|artifact| artifact.valid);

    RunExportAssessment {
        path: path.display().to_string(),
        structurally_verified,
        verifier_evidence_complete,
        legacy_delivery_result_count,
        evidence_complete,
        run_id,
        event_count,
        outbox_count,
        sealed_artifacts,
        failures,
    }
}

fn sealed_artifact_digests_from_verified_export(path: &Path) -> Result<Vec<String>, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("read {} after verification: {error}", path.display()))?;
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("decode {} after verification: {error}", path.display()))?;
    let mut digests = BTreeSet::new();
    collect_sealed_artifact_digests(&document, "events", false, &mut digests)?;
    collect_sealed_artifact_digests(&document, "outbox", true, &mut digests)?;
    Ok(digests.into_iter().collect())
}

fn collect_sealed_artifact_digests(
    document: &Value,
    section: &str,
    reference_required: bool,
    digests: &mut BTreeSet<String>,
) -> Result<(), String> {
    let records = document
        .get(section)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{section} is not an array"))?;
    for (index, record) in records.iter().enumerate() {
        let record = record
            .as_object()
            .ok_or_else(|| format!("{section}[{index}] is not an object"))?;
        let Some(reference) = record.get("sealed_artifact_ref") else {
            if reference_required {
                return Err(format!("{section}[{index}] lacks a sealed artifact reference"));
            }
            continue;
        };
        if reference.is_null() {
            if reference_required {
                return Err(format!("{section}[{index}] has a null sealed artifact reference"));
            }
            continue;
        }
        let reference = reference
            .as_object()
            .ok_or_else(|| format!("{section}[{index}] sealed artifact reference is not an object"))?;
        let digest = reference
            .get("envelope_digest")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!("{section}[{index}] sealed artifact reference lacks envelope_digest")
            })?;
        if !is_sha256_digest(digest) {
            return Err(format!(
                "{section}[{index}] sealed artifact envelope_digest is not lowercase SHA-256"
            ));
        }
        digests.insert(digest.to_string());
    }
    Ok(())
}

fn is_sha256_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn assess_sealed_artifact(artifacts_dir: &Path, digest: &str) -> SealedArtifactAssessment {
    let path = artifacts_dir.join(format!("{digest}.sealed"));
    if !is_sha256_digest(digest) {
        return failed_sealed_artifact_assessment(
            digest,
            &path,
            "sealed artifact reference is not lowercase SHA-256",
        );
    }
    match fs::symlink_metadata(artifacts_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return failed_sealed_artifact_assessment(
                digest,
                &path,
                "sealed artifact directory is a symlink",
            );
        }
        Ok(metadata) if !metadata.is_dir() => {
            return failed_sealed_artifact_assessment(
                digest,
                &path,
                "sealed artifact path parent is not a directory",
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return failed_sealed_artifact_assessment(digest, &path, "sealed artifact is missing");
        }
        Err(error) => {
            return failed_sealed_artifact_assessment(
                digest,
                &path,
                format!("inspect sealed artifact directory: {error}"),
            );
        }
    }
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return failed_sealed_artifact_assessment(
                digest,
                &path,
                "sealed artifact path is a symlink",
            );
        }
        Ok(metadata) if !metadata.is_file() => {
            return failed_sealed_artifact_assessment(
                digest,
                &path,
                "sealed artifact is not a regular file",
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return failed_sealed_artifact_assessment(digest, &path, "sealed artifact is missing");
        }
        Err(error) => {
            return failed_sealed_artifact_assessment(
                digest,
                &path,
                format!("inspect sealed artifact: {error}"),
            );
        }
    }
    match verify_owner_only_sealed_artifact(&path, digest) {
        Ok(()) => SealedArtifactAssessment {
            digest: digest.to_string(),
            path: path.display().to_string(),
            valid: true,
            failure: None,
        },
        Err(error) => failed_sealed_artifact_assessment(digest, &path, error),
    }
}

fn failed_sealed_artifact_assessment(
    digest: &str,
    path: &Path,
    failure: impl Into<String>,
) -> SealedArtifactAssessment {
    SealedArtifactAssessment {
        digest: digest.to_string(),
        path: path.display().to_string(),
        valid: false,
        failure: Some(failure.into()),
    }
}

#[cfg(unix)]
fn verify_owner_only_sealed_artifact(path: &Path, digest: &str) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("open sealed artifact without following links: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("read sealed artifact metadata: {error}"))?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o177 != 0
    {
        return Err("sealed artifact is not an owner-only, single-link regular file".to_string());
    }

    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8_192];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read sealed artifact: {error}"))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = hex::encode(hasher.finalize());
    if actual != digest {
        return Err("sealed artifact SHA-256 does not match its reference".to_string());
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_owner_only_sealed_artifact(_path: &Path, _digest: &str) -> Result<(), String> {
    Err("owner-only sealed artifact verification is unsupported on this platform".to_string())
}

fn build_legacy_replay_diagnostics(state: &Path) -> LegacyReplayDiagnostics {
    let directory = legacy_replay_dir(state);
    let mut errors = Vec::new();
    let paths = match legacy_replay_log_paths(&directory) {
        Ok(paths) => paths,
        Err(error) => {
            errors.push(error);
            Vec::new()
        }
    };
    let mut logs = Vec::new();
    for path in paths {
        match summarize_legacy_replay_log(&path) {
            Ok(log) => logs.push(log),
            Err(error) => {
                errors.push(format!("{}: {error}", path.display()));
                logs.push(LegacyReplayLogDiagnostic {
                    path: path.display().to_string(),
                    exists: path.exists(),
                    entries: 0,
                    parse_errors: 0,
                    chain_breaks: 0,
                    first_seq: None,
                    last_seq: None,
                    read_error: Some(error),
                });
            }
        }
    }
    LegacyReplayDiagnostics {
        directory: directory.display().to_string(),
        log_count: logs.len(),
        logs,
        errors,
    }
}

fn legacy_replay_log_paths(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "read legacy replay diagnostic directory {}: {error}",
                dir.display()
            ));
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "read entry in legacy replay diagnostic directory {}: {error}",
                dir.display()
            )
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn summarize_legacy_replay_log(path: &Path) -> Result<LegacyReplayLogDiagnostic, String> {
    let raw =
        fs::read_to_string(path).map_err(|error| format!("read legacy replay log: {error}"))?;
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

    Ok(LegacyReplayLogDiagnostic {
        path: path.display().to_string(),
        exists: path.exists(),
        entries,
        parse_errors,
        chain_breaks,
        first_seq,
        last_seq,
        read_error: None,
    })
}

pub fn build_a2a_availability_report() -> Value {
    json!({
        "kind": "hermes.a2a.availability",
        "version": env!("CARGO_PKG_VERSION"),
        "available": false,
        "production_complete": false,
        "reason": "no A2A task or message endpoint is implemented",
        "advertised_capabilities": [],
        "evidence_scope": "static availability document only",
        "registered_command_surfaces": {
            "mcp": {"transport": "stdio", "command": "hermes mcp serve"},
            "acp": {"transport": "stdio", "command": "hermes acp start"},
            "run_export_verification": {"command": "/raw trace verify <PATH>"},
            "handoff": {"command": "/handoff <platform>"},
            "provenance": {"verify": "hermes verify-provenance <artifact>", "rotate": "hermes rotate-provenance-key"}
        },
        "availability_endpoint": A2A_AVAILABILITY_PATH,
        "health_endpoint": "/healthz"
    })
}

fn serve_a2a_availability(host: &str, port: u16, once: bool) -> Result<(), AgentError> {
    let bind_addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&bind_addr).map_err(|err| {
        AgentError::Io(format!("bind A2A availability server {bind_addr}: {err}"))
    })?;
    let addr = listener
        .local_addr()
        .map_err(|err| AgentError::Io(format!("read A2A availability listener addr: {err}")))?;
    println!(
        "A2A availability server listening on http://{}{}",
        addr, A2A_AVAILABILITY_PATH
    );
    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => handle_a2a_availability_stream(&mut stream)?,
            Err(err) => {
                return Err(AgentError::Io(format!(
                    "accept A2A availability connection: {err}"
                )))
            }
        }
        if once {
            break;
        }
    }
    Ok(())
}

fn handle_a2a_availability_stream(stream: &mut TcpStream) -> Result<(), AgentError> {
    let mut buf = [0_u8; 4096];
    let n = stream
        .read(&mut buf)
        .map_err(|err| AgentError::Io(format!("read A2A availability request: {err}")))?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let (status, content_type, body) = a2a_availability_response_for_path(path);
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
        .map_err(|err| AgentError::Io(format!("write A2A availability response: {err}")))?;
    stream
        .flush()
        .map_err(|err| AgentError::Io(format!("flush A2A availability response: {err}")))?;
    Ok(())
}

fn a2a_availability_response_for_path(path: &str) -> (u16, &'static str, String) {
    match path {
        A2A_AVAILABILITY_PATH => (
            200,
            "application/json",
            serde_json::to_string_pretty(&build_a2a_availability_report())
                .unwrap_or_else(|_| "{}".to_string()),
        ),
        "/healthz" => (
            200,
            "application/json",
            json!({"status":"ok","service":"hermes-a2a-availability"}).to_string(),
        ),
        _ => (
            404,
            "application/json",
            json!({"error":"not_found","expected":[A2A_AVAILABILITY_PATH,"/healthz"]}).to_string(),
        ),
    }
}

pub async fn build_mcp_diagnostic_report() -> ProtocolDiagnosticReport {
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

    let local_checks_passed = checks.iter().all(|check| check.status == "pass");
    ProtocolDiagnosticReport {
        kind: "hermes.mcp.in_process_diagnostic".to_string(),
        protocol: "MCP".to_string(),
        protocol_version: "2024-11-05".to_string(),
        generated_at: now_rfc3339(),
        scope: "in_process_method_shape_only".to_string(),
        local_checks_passed,
        external_client_interoperability_verified: false,
        production_complete: false,
        checks,
    }
}

pub async fn build_acp_diagnostic_report() -> ProtocolDiagnosticReport {
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

    let local_checks_passed = checks.iter().all(|check| check.status == "pass");
    ProtocolDiagnosticReport {
        kind: "hermes.acp.in_process_diagnostic".to_string(),
        protocol: "ACP".to_string(),
        protocol_version: "1".to_string(),
        generated_at: now_rfc3339(),
        scope: "in_process_method_shape_only".to_string(),
        local_checks_passed,
        external_client_interoperability_verified: false,
        production_complete: false,
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
) -> ProtocolDiagnosticCheck {
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

fn pass(method: &str, detail: impl Into<String>) -> ProtocolDiagnosticCheck {
    ProtocolDiagnosticCheck {
        method: method.to_string(),
        status: "pass".to_string(),
        detail: detail.into(),
    }
}

fn fail(method: &str, detail: impl Into<String>) -> ProtocolDiagnosticCheck {
    ProtocolDiagnosticCheck {
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
        "kind": "hermes.command_surface_availability",
        "version": env!("CARGO_PKG_VERSION"),
        "generated_at": now_rfc3339(),
        "authority": "registration_and_provider_metadata_only",
        "production_completeness_assessed": false,
        "does_not_prove": [
            "external_client_interoperability",
            "failure_recovery",
            "production_smoke",
            "release_readiness"
        ],
        "provider_count": providers.len(),
        "providers": providers,
        "tool_surfaces": [
            {"surface":"mcp", "availability":"entrypoint_registered", "evidence_scope":"registration_only", "entrypoint":"hermes mcp serve"},
            {"surface":"acp", "availability":"entrypoint_registered", "evidence_scope":"registration_only", "entrypoint":"hermes acp start"},
            {"surface":"terminal", "availability":"policy_surface_present", "evidence_scope":"registration_only", "entrypoint":"interactive agent loop"},
            {"surface":"gateway", "availability":"entrypoint_registered", "evidence_scope":"registration_only", "entrypoint":"hermes gateway status"},
            {"surface":"cron", "availability":"entrypoint_registered", "evidence_scope":"registration_only", "entrypoint":"hermes cron list"},
            {"surface":"webhook", "availability":"entrypoint_registered", "evidence_scope":"registration_only", "entrypoint":"hermes webhook list"},
            {"surface":"run_export_verification", "availability":"entrypoint_registered", "evidence_scope":"registration_only", "entrypoint":"/raw trace verify <PATH>"}
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

    #[cfg(unix)]
    use std::path::{Path, PathBuf};
    #[cfg(unix)]
    use hermes_agent::run_ledger::{ClaimNext, OutboxRouteRegistry, OutboxState};
    #[cfg(unix)]
    use hermes_agent::{AlphaRuntimeV1Request, RunRuntime};
    #[cfg(unix)]
    use hermes_core::execution_mode::{ExecutionMode, ExecutionModeSource, ResolvedExecutionMode};

    #[test]
    fn systems_status_tracks_requested_surfaces_without_false_completion() {
        let temp = tempfile::tempdir().expect("tempdir");
        let report = build_status_report(temp.path());
        assert_eq!(report.total, SYSTEM_COUNT);
        assert!(!report.production_completeness_assessed);
        assert_eq!(report.authority, "registration_and_source_presence_only");
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
    fn systems_status_marks_source_checks_unassessed_without_checkout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let report = build_status_report_for_source_root(temp.path(), None);

        assert_eq!(report.total, SYSTEM_COUNT);
        assert_eq!(report.available_surfaces, 4);
        assert_eq!(
            report
                .items
                .iter()
                .filter(|item| item.availability == "not_assessed")
                .count(),
            3
        );
        assert!(!report.production_completeness_assessed);
    }

    #[test]
    fn release_evidence_inventory_never_claims_unexecuted_validation() {
        let report = build_release_evidence_report_for_source_root(None);
        let rendered = serde_json::to_string(&report).expect("serialize report");

        assert!(!report.validation_executed);
        assert!(!report.release_ready);
        assert!(report
            .items
            .iter()
            .all(|item| item.status == "not_assessed"));
        assert!(!rendered.contains(env!("CARGO_MANIFEST_DIR")));
    }

    #[test]
    fn release_evidence_inventory_reports_missing_source_without_passing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        fs::create_dir_all(root.join("crates/hermes-cli/src")).expect("mkdir source");
        fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("write manifest");
        fs::write(
            root.join("crates/hermes-cli/src/systems.rs"),
            "// systems\n",
        )
        .expect("write systems");

        assert!(source_tree_is_available(root));
        let report = build_release_evidence_report_for_source_root(Some(root));
        let rendered = serde_json::to_string(&report).expect("serialize report");

        assert!(!report.validation_executed);
        assert!(!report.release_ready);
        assert!(report
            .items
            .iter()
            .any(|item| { item.name == "MCP implementation" && item.status == "source_missing" }));
        assert!(report
            .items
            .iter()
            .any(|item| item.status == "source_present"));
        assert!(!rendered.contains(root.to_string_lossy().as_ref()));
    }

    #[cfg(unix)]
    fn report_test_mode() -> ResolvedExecutionMode {
        ResolvedExecutionMode {
            mode: ExecutionMode::Manual,
            source: ExecutionModeSource::BuiltInDefault,
        }
    }

    #[cfg(unix)]
    fn report_test_alpha_request() -> AlphaRuntimeV1Request {
        AlphaRuntimeV1Request::new(
            "primary-objective-loop",
            "manual",
            "systems report sealed task",
        )
        .expect("valid alpha runtime request")
    }

    #[cfg(unix)]
    fn write_report_test_export(
        state: &Path,
        terminal_missing_route: bool,
    ) -> (PathBuf, PathBuf) {
        let runtime =
            RunRuntime::open_for_ephemeral_testing(state).expect("open ephemeral run runtime");
        let (run_id, expected_artifact_count) = if terminal_missing_route {
            let enqueue = runtime
                .enqueue_alpha_runtime_v1(report_test_mode(), report_test_alpha_request())
                .expect("enqueue alpha runtime request");
            let routes = OutboxRouteRegistry::default();
            let mut dead_lettered = false;
            for _ in 0..16 {
                match runtime
                    .ledger()
                    .claim_next(report_test_mode(), &routes)
                    .expect("reject missing route")
                {
                    ClaimNext::Rejected {
                        state: OutboxState::DeadLettered,
                        ..
                    } => {
                        dead_lettered = true;
                        break;
                    }
                    ClaimNext::Rejected { .. } => {}
                    other => panic!("expected missing-route rejection: {other:?}"),
                }
            }
            assert!(dead_lettered, "missing route must eventually dead-letter");
            (enqueue.run_id, 1)
        } else {
            let mut dispatched_run_id = None;
            for _ in 0..64 {
                let enqueue = runtime
                    .enqueue_alpha_runtime_v1(report_test_mode(), report_test_alpha_request())
                    .expect("enqueue alpha runtime request");
                let outbox = runtime
                    .ledger()
                    .outbox_for_run(enqueue.run_id)
                    .expect("inspect alpha runtime outbox");
                assert_eq!(outbox.len(), 1, "alpha run has one outbox row");
                let evidence_id = outbox[0].outbox_id.to_string();
                let evidence_subject = format!(
                    "hermes.session-run.outbox/alpha-runtime-v1/{}",
                    outbox[0].idempotency_key.as_str()
                );
                let identity_is_redaction_safe =
                    hermes_intelligence::redact_sensitive_text(&evidence_id) == evidence_id
                        && hermes_intelligence::redact_sensitive_text(&evidence_subject)
                            == evidence_subject;
                let dispatch = runtime.dispatch_alpha_runtime_v1(report_test_mode(), 1);
                if identity_is_redaction_safe {
                    let dispatch = dispatch.expect("dispatch redaction-safe alpha runtime request");
                    assert_eq!(dispatch.acknowledged, 1, "dispatch: {dispatch:#?}");
                    dispatched_run_id = Some(enqueue.run_id);
                    break;
                }
                let error = dispatch.expect_err(
                    "a redactor-unsafe delivery-evidence identity must reject dispatch",
                );
                assert!(
                    error
                        .to_string()
                        .contains("credential or secret indicator"),
                    "unexpected unsafe-identity dispatch failure: {error}"
                );
            }
            (
                dispatched_run_id
                    .expect("generate a redaction-safe alpha delivery-evidence identity"),
                2,
            )
        };

        let export = runtime
            .publish_run_export(run_id)
            .expect("publish complete run export");
        let digests =
            sealed_artifact_digests_from_verified_export(&export.path).expect("extract artifact");
        assert_eq!(
            digests.len(),
            expected_artifact_count,
            "export should reference every authoritative sealed artifact"
        );
        let artifact_path = sealed_artifact_dir(state).join(format!("{}.sealed", digests[0]));
        assert!(artifact_path.is_file(), "sealed artifact should exist");
        (export.path, artifact_path)
    }

    #[test]
    fn run_export_report_with_no_exports_is_unassessed_and_not_passing() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = build_run_export_report(temp.path());

        assert_eq!(report.assessed_export_count, 0);
        assert!(!report.evidence_complete);
        assert!(!report.passed);
        assert_eq!(report.diagnostic_only.log_count, 0);
    }

    #[cfg(unix)]
    #[test]
    fn run_export_report_requires_verified_export_and_matching_sealed_cas() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_report_test_export(temp.path(), false);

        let report = build_run_export_report(temp.path());

        assert_eq!(report.assessed_export_count, 1);
        assert!(report.evidence_complete, "report: {report:#?}");
        assert!(report.passed, "report: {report:#?}");
        let export = &report.exports[0];
        assert!(export.structurally_verified, "export: {export:#?}");
        assert!(export.verifier_evidence_complete, "export: {export:#?}");
        assert_eq!(export.legacy_delivery_result_count, Some(0));
        assert!(export.evidence_complete, "export: {export:#?}");
        assert_eq!(export.sealed_artifacts.len(), 1);
        assert!(export.sealed_artifacts[0].valid, "export: {export:#?}");
    }

    #[cfg(unix)]
    #[test]
    fn run_export_report_rejects_tampered_export() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (export_path, _) =
            write_report_test_export(temp.path(), false);
        let mut bytes = fs::read(&export_path).expect("read export");
        bytes.push(b'\n');
        fs::write(export_path, bytes).expect("tamper export");

        let report = build_run_export_report(temp.path());

        assert_eq!(report.assessed_export_count, 1);
        assert!(!report.evidence_complete);
        assert!(!report.passed);
        assert!(
            !report.exports[0].structurally_verified,
            "report: {report:#?}"
        );
        assert!(report.exports[0]
            .failures
            .iter()
            .any(|failure| failure.contains("verifier failed")));
    }

    #[cfg(unix)]
    #[test]
    fn run_export_report_rejects_missing_sealed_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (_, artifact_path) =
            write_report_test_export(temp.path(), false);
        fs::remove_file(artifact_path).expect("remove sealed artifact");

        let report = build_run_export_report(temp.path());

        assert!(!report.evidence_complete);
        assert!(!report.passed);
        assert!(report.exports[0].structurally_verified);
        assert!(!report.exports[0].evidence_complete);
        assert_eq!(
            report.exports[0].sealed_artifacts[0].failure.as_deref(),
            Some("sealed artifact is missing")
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_export_report_rejects_corrupt_sealed_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (_, artifact_path) =
            write_report_test_export(temp.path(), false);
        fs::write(&artifact_path, b"corrupt sealed artifact").expect("corrupt sealed artifact");

        let report = build_run_export_report(temp.path());

        assert!(!report.evidence_complete);
        assert!(!report.passed);
        assert!(report.exports[0].structurally_verified);
        assert!(report.exports[0].sealed_artifacts[0]
            .failure
            .as_deref()
            .is_some_and(|failure| failure.contains("SHA-256")));
    }

    #[cfg(unix)]
    #[test]
    fn run_export_report_rejects_symlinked_sealed_artifact() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let (_, artifact_path) =
            write_report_test_export(temp.path(), false);
        let target = temp.path().join("sealed-artifact-target");
        fs::write(&target, fs::read(&artifact_path).expect("read sealed artifact"))
            .expect("write target");
        fs::remove_file(&artifact_path).expect("remove artifact");
        symlink(&target, &artifact_path).expect("symlink artifact");

        let report = build_run_export_report(temp.path());

        assert!(!report.evidence_complete);
        assert!(!report.passed);
        assert!(report.exports[0].structurally_verified);
        assert_eq!(
            report.exports[0].sealed_artifacts[0].failure.as_deref(),
            Some("sealed artifact path is a symlink")
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_export_report_accepts_public_terminal_outbox_export() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_report_test_export(temp.path(), true);

        let report = build_run_export_report(temp.path());

        assert_eq!(report.assessed_export_count, 1);
        assert!(report.evidence_complete, "report: {report:#?}");
        assert!(report.passed, "report: {report:#?}");
        let export = &report.exports[0];
        assert!(export.structurally_verified, "export: {export:#?}");
        assert!(export.verifier_evidence_complete, "export: {export:#?}");
        assert_eq!(export.legacy_delivery_result_count, Some(0));
        assert_eq!(export.outbox_count, Some(1));
        assert!(export.evidence_complete, "export: {export:#?}");
    }

    #[test]
    fn run_export_report_keeps_legacy_jsonl_diagnostic_only_and_unassessed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory = legacy_replay_dir(temp.path());
        fs::create_dir_all(&directory).expect("create legacy diagnostic directory");
        let legacy = concat!(
            "{\"event_hash\":\"0123456789abcdef\",\"prev_hash\":\"fedcba9876543210\",\"seq\":1}\n",
            "{\"event_hash\":\"1111111111111111\",\"prev_hash\":\"0123456789abcdef\",\"seq\":2}\n"
        );
        let legacy_path = directory.join("session.jsonl");
        fs::write(&legacy_path, legacy).expect("write legacy replay log");
        assert!(matches!(
            hermes_agent::run_export::classify_run_export(legacy.as_bytes()),
            Ok(hermes_agent::run_export::RunExportClassification::LegacyV0Jsonl {
                record_count: 2,
                ..
            })
        ));
        assert!(matches!(
            hermes_agent::run_export::verify_run_export(&legacy_path),
            Err(hermes_agent::run_export::RunExportError::LegacyV0NotVerifiable)
        ));

        let report = build_run_export_report(temp.path());

        assert_eq!(report.assessed_export_count, 0);
        assert!(!report.evidence_complete);
        assert!(!report.passed);
        assert_eq!(report.diagnostic_only.log_count, 1);
        assert_eq!(report.diagnostic_only.logs[0].entries, 2);
        assert_eq!(report.diagnostic_only.logs[0].chain_breaks, 0);
    }

    #[test]
    fn a2a_availability_does_not_advertise_unimplemented_runtime() {
        let report = build_a2a_availability_report();
        assert_eq!(
            report.get("available").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            report.get("production_complete").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            report
                .get("advertised_capabilities")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
        assert!(report.get("capabilities").is_none());
        assert_eq!(
            report
                .pointer("/registered_command_surfaces/mcp/command")
                .and_then(Value::as_str),
            Some("hermes mcp serve")
        );
        let (status, content_type, body) =
            a2a_availability_response_for_path(A2A_AVAILABILITY_PATH);
        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        assert!(body.contains("\"available\": false"));
        assert!(!body.contains("\"streaming\": true"));
    }

    #[tokio::test]
    async fn mcp_diagnostic_reports_local_scope_without_false_completion() {
        let report = build_mcp_diagnostic_report().await;
        assert!(
            report.local_checks_passed,
            "local diagnostic should pass: {report:#?}"
        );
        assert_eq!(report.scope, "in_process_method_shape_only");
        assert!(!report.external_client_interoperability_verified);
        assert!(!report.production_complete);
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
    async fn acp_diagnostic_reports_local_scope_without_false_completion() {
        let report = build_acp_diagnostic_report().await;
        assert!(
            report.local_checks_passed,
            "local diagnostic should pass: {report:#?}"
        );
        assert_eq!(report.scope, "in_process_method_shape_only");
        assert!(!report.external_client_interoperability_verified);
        assert!(!report.production_complete);
        assert!(report
            .checks
            .iter()
            .any(|check| check.method == "session/new"));
        assert!(report.checks.iter().any(|check| check.method == "prompt"));
    }

    #[test]
    fn capability_matrix_reports_metadata_without_false_completion() {
        let matrix = build_capability_matrix();
        assert_eq!(
            matrix
                .get("production_completeness_assessed")
                .and_then(Value::as_bool),
            Some(false)
        );
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
        let surfaces = matrix
            .get("tool_surfaces")
            .and_then(Value::as_array)
            .expect("tool surfaces");
        assert!(surfaces
            .iter()
            .all(|surface| surface.get("status").is_none()));
        assert!(surfaces.iter().all(|surface| {
            surface.get("evidence_scope").and_then(Value::as_str) == Some("registration_only")
        }));
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
