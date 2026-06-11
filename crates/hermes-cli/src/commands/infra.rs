//! Infrastructure slash commands: toolsets, plugins, MCP, reload, cron, agents, LSP, graph.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hermes_core::AgentError;
use regex::Regex;

use crate::alpha_runtime::load_contextlattice_policy;
use crate::commands::{CommandResult, emit_command_output, yes_no};
use crate::App;

pub(crate) fn handle_toolsets_command(app: &mut App) -> Result<CommandResult, AgentError> {
    if app.config.platform_toolsets.is_empty() {
        emit_command_output(app, "No explicit platform toolsets configured.");
        return Ok(CommandResult::Handled);
    }
    let mut rows: Vec<_> = app.config.platform_toolsets.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = String::from("Configured toolsets by platform:\n");
    for (platform, toolsets) in rows {
        let _ = writeln!(out, "  - {:<10} {}", platform, toolsets.join(", "));
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

pub(crate) fn handle_plugins_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let rows = super::discover_plugin_surface(true);
    if rows.is_empty() {
        let plugins_dir = hermes_config::hermes_home().join("plugins");
        emit_command_output(
            app,
            format!(
                "No plugin bundles discovered.\nUser plugin dir: {}\nInstall with `hermes plugins install <owner/repo>`.",
                plugins_dir.display()
            ),
        );
    } else {
        emit_command_output(
            app,
            format!(
                "Plugin surface ({} entries):\n{}",
                rows.len(),
                super::render_plugin_surface_table(&rows)
            ),
        );
    }
    Ok(CommandResult::Handled)
}

pub(crate) fn handle_mcp_command(app: &mut App) -> Result<CommandResult, AgentError> {
    if app.config.mcp_servers.is_empty() {
        emit_command_output(app, "No MCP servers configured in `config.yaml`.");
        return Ok(CommandResult::Handled);
    }
    let mut out = String::from("Configured MCP servers:\n");
    for server in &app.config.mcp_servers {
        let endpoint = server
            .url
            .as_deref()
            .filter(|u| !u.is_empty())
            .unwrap_or("<stdio>");
        let _ = writeln!(
            out,
            "  - {:<18} {}  [parallel_tool_calls:{}]",
            server.name,
            endpoint,
            if server.supports_parallel_tool_calls {
                "on"
            } else {
                "off"
            }
        );
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

pub(crate) fn handle_reload_command(app: &mut App, cmd: &str) -> Result<CommandResult, AgentError> {
    if cmd == "/reload-mcp" {
        emit_command_output(
            app,
            "MCP reload requested. Restart session/gateway for full connector renegotiation.",
        );
    } else {
        hermes_config::loader::load_dotenv();
        match hermes_config::load_config(app.state_root.to_str()) {
            Ok(cfg) => {
                app.config = Arc::new(cfg);
                emit_command_output(
                    app,
                    "Reload complete: env + config rehydrated for this session.",
                );
            }
            Err(err) => {
                emit_command_output(
                    app,
                    format!(
                        "Reload partially applied (.env refreshed), but config parse failed: {}",
                        err
                    ),
                );
            }
        }
    }
    Ok(CommandResult::Handled)
}

pub(crate) fn handle_cron_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let cron_data = hermes_config::cron_dir();
    let jobs_file = cron_data.join("jobs.json");
    let count = std::fs::read_to_string(&jobs_file)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("jobs")
                .and_then(|j| j.as_array())
                .map(|arr| arr.len())
                .or_else(|| v.as_array().map(|arr| arr.len()))
        })
        .unwrap_or_else(|| {
            std::fs::read_dir(&cron_data)
                .ok()
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| {
                            e.path().extension().and_then(|x| x.to_str()) == Some("json")
                                && e.file_name().to_string_lossy() != "jobs.json"
                        })
                        .count()
                })
                .unwrap_or(0)
        });
    emit_command_output(
        app,
        format!(
            "Cron scheduler data dir: {}\nPersisted jobs: {}\nUse `hermes cron list` for full job table.",
            cron_data.display(),
            count
        ),
    );
    Ok(CommandResult::Handled)
}

fn background_status_rows() -> Vec<String> {
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    let mut rows = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(&jobs_dir) else {
        return rows;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("unknown");
        let status = v
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown");
        let task = v
            .get("task")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .replace('\n', " ");
        rows.push(format!("{id}  [{status}]  {task}"));
    }
    rows.sort();
    rows
}

fn env_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub(crate) fn handle_agents_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sub = args.first().map(|s| s.trim().to_ascii_lowercase());

    if matches!(sub.as_deref(), Some("pause")) {
        crate::env_vars::set_var("HERMES_DELEGATION_PAUSED", "1");
        emit_command_output(
            app,
            "Delegation spawning paused for this runtime.\nSet with `/agents resume`.\nStatus: `/agents status`.",
        );
        return Ok(CommandResult::Handled);
    }
    if matches!(sub.as_deref(), Some("resume" | "unpause")) {
        crate::env_vars::set_var("HERMES_DELEGATION_PAUSED", "0");
        emit_command_output(
            app,
            "Delegation spawning resumed for this runtime.\nStatus: `/agents status`.",
        );
        return Ok(CommandResult::Handled);
    }
    if matches!(sub.as_deref(), Some("doctor")) {
        emit_command_output(
            app,
            "Agents doctor\n- queue manifest audit: `python3 scripts/audit_background_queue.py`\n- optional repair: `python3 scripts/audit_background_queue.py --repair`\n- delegation state: `/agents status`\n- spawn tree UI: `/agents` (TUI overlay)",
        );
        return Ok(CommandResult::Handled);
    }

    if matches!(sub.as_deref(), Some(other) if other != "status" && other != "list") {
        emit_command_output(app, "Usage: /agents [status|pause|resume|doctor]");
        return Ok(CommandResult::Handled);
    }

    let paused = std::env::var("HERMES_DELEGATION_PAUSED")
        .ok()
        .map(|raw| env_truthy(&raw))
        .unwrap_or(false);
    let rows = background_status_rows();
    if rows.is_empty() {
        emit_command_output(
            app,
            format!(
                "Delegation spawning: {}\nBackground jobs: 0\n\nNo background jobs found.\nAudit/repair queue manifests with `python3 scripts/audit_background_queue.py [--repair]`.",
                if paused { "paused" } else { "active" }
            ),
        );
    } else {
        let joined = rows.into_iter().take(20).collect::<Vec<_>>().join("\n");
        emit_command_output(
            app,
            format!(
                "Delegation spawning: {}\nBackground jobs (top 20):\n{}\n\nQueue audit: `python3 scripts/audit_background_queue.py`\nPause/resume: `/agents pause` or `/agents resume`",
                if paused { "paused" } else { "active" },
                joined,
            ),
        );
    }
    Ok(CommandResult::Handled)
}

pub(crate) fn handle_lsp_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_else(|| "status".to_string());
    match sub.as_str() {
        "status" | "show" => {
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unavailable>".to_string());
            let mut out = String::new();
            let _ = writeln!(out, "LSP/code-index status");
            let _ = writeln!(out, "  cwd: {}", cwd);
            let _ = writeln!(
                out,
                "  code_index_enabled: {}",
                yes_no(app.config.agent.code_index_enabled)
            );
            let _ = writeln!(
                out,
                "  code_index_max_files: {}",
                app.config.agent.code_index_max_files
            );
            let _ = writeln!(
                out,
                "  code_index_max_symbols: {}",
                app.config.agent.code_index_max_symbols
            );
            let _ = writeln!(
                out,
                "  lsp_context_enabled: {}",
                yes_no(app.config.agent.lsp_context_enabled)
            );
            let _ = writeln!(
                out,
                "  lsp_context_max_chars: {}",
                app.config.agent.lsp_context_max_chars
            );
            let _ = writeln!(
                out,
                "  tip: run `/plan map the repo architecture` to force a high-signal repo-map pass."
            );
            emit_command_output(app, out.trim_end());
        }
        "refresh" => {
            emit_command_output(
                app,
                "Code index refresh is automatic while the agent executes tool calls. Queue a focused analysis with `/plan <task>` if you want a deliberate repo-map rebuild now.",
            );
        }
        "help" => {
            emit_command_output(
                app,
                "Usage: /lsp [status|refresh]\n  status   show code-index + LSP context configuration\n  refresh  explain how to trigger a fresh index pass",
            );
        }
        _ => emit_command_output(app, "Usage: /lsp [status|refresh]"),
    }
    Ok(CommandResult::Handled)
}

fn collect_graph_candidate_files(
    root: &Path,
    max_files: usize,
    out: &mut Vec<PathBuf>,
) -> Result<(), AgentError> {
    if out.len() >= max_files {
        return Ok(());
    }
    let rd = std::fs::read_dir(root)
        .map_err(|e| AgentError::Io(format!("read_dir {}: {}", root.display(), e)))?;
    for entry in rd {
        if out.len() >= max_files {
            break;
        }
        let entry = match entry {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if matches!(
                name,
                ".git"
                    | "target"
                    | "node_modules"
                    | ".venv"
                    | "venv"
                    | "__pycache__"
                    | ".mypy_cache"
                    | ".pytest_cache"
            ) {
                continue;
            }
            collect_graph_candidate_files(&path, max_files, out)?;
            continue;
        }
        let ext = path
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(ext.as_str(), "rs" | "py" | "ts" | "tsx" | "js" | "jsx") {
            out.push(path);
        }
    }
    Ok(())
}

fn extract_semantic_refs_for_file(ext: &str, content: &str) -> Vec<String> {
    let mut refs = Vec::new();
    match ext {
        "rs" => {
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("use ") {
                    let target = rest.split(';').next().unwrap_or_default().trim();
                    if !target.is_empty() {
                        refs.push(target.to_string());
                    }
                }
                if let Some(rest) = trimmed.strip_prefix("mod ") {
                    let target = rest.split(';').next().unwrap_or_default().trim();
                    if !target.is_empty() {
                        refs.push(target.to_string());
                    }
                }
            }
        }
        "py" => {
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("import ") {
                    for item in rest.split(',') {
                        let target = item.split_whitespace().next().unwrap_or_default().trim();
                        if !target.is_empty() {
                            refs.push(target.to_string());
                        }
                    }
                } else if let Some(rest) = trimmed.strip_prefix("from ") {
                    let target = rest.split_whitespace().next().unwrap_or_default().trim();
                    if !target.is_empty() {
                        refs.push(target.to_string());
                    }
                }
            }
        }
        "ts" | "tsx" | "js" | "jsx" => {
            let re = Regex::new(r#"(?m)from\s+["']([^"']+)["']"#).expect("valid import regex");
            for caps in re.captures_iter(content) {
                if let Some(m) = caps.get(1) {
                    refs.push(m.as_str().trim().to_string());
                }
            }
            let re_req = Regex::new(r#"(?m)require\(\s*["']([^"']+)["']\s*\)"#)
                .expect("valid require regex");
            for caps in re_req.captures_iter(content) {
                if let Some(m) = caps.get(1) {
                    refs.push(m.as_str().trim().to_string());
                }
            }
        }
        _ => {}
    }
    refs
}

fn sanitize_graph_node(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn contextlattice_base_url_for_graph() -> String {
    std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("MEMMCP_ORCHESTRATOR_URL").ok())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8075".to_string())
}

fn contextlattice_api_key_for_graph() -> Option<String> {
    std::env::var("CONTEXTLATTICE_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("MEMMCP_API_KEY").ok())
        .filter(|v| !v.trim().is_empty())
}

fn extract_json_path<'a>(
    value: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a serde_json::Value> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    Some(cur)
}

fn extract_embedding_diag_line(payload: &serde_json::Value) -> String {
    let backend = [
        &["backend"][..],
        &["embedding_backend"][..],
        &["embeddings", "backend"][..],
        &["retrieval", "embedding_backend"][..],
    ]
    .into_iter()
    .find_map(|path| extract_json_path(payload, path))
    .and_then(|v| v.as_str())
    .unwrap_or("unknown");
    let dimension = [
        &["dimension"][..],
        &["embeddings", "dimension"][..],
        &["retrieval", "embedding_dimension"][..],
    ]
    .into_iter()
    .find_map(|path| extract_json_path(payload, path))
    .and_then(|v| v.as_u64())
    .map(|v| v.to_string())
    .unwrap_or_else(|| "n/a".to_string());
    let model = [
        &["model"][..],
        &["embeddings", "model"][..],
        &["retrieval", "embedding_model"][..],
    ]
    .into_iter()
    .find_map(|path| extract_json_path(payload, path))
    .and_then(|v| v.as_str())
    .unwrap_or("unknown");
    format!(
        "embedding_diagnostics: backend={} model={} dimension={}",
        backend, model, dimension
    )
}

async fn contextlattice_embedding_diagnostics_lines() -> Vec<String> {
    let base_url = contextlattice_base_url_for_graph();
    let mut lines = Vec::new();
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            lines.push(format!("client_error: {}", err));
            return lines;
        }
    };

    let mut health_req = client.get(format!("{}/health", base_url.trim_end_matches('/')));
    if let Some(key) = contextlattice_api_key_for_graph() {
        health_req = health_req.header("x-api-key", key);
    }
    match health_req.send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            lines.push(format!("health_status: {}", code));
        }
        Err(err) => {
            lines.push(format!("health_status: unreachable ({})", err));
        }
    }

    let mut emb_req = client.get(format!(
        "{}/telemetry/embeddings",
        base_url.trim_end_matches('/')
    ));
    if let Some(key) = contextlattice_api_key_for_graph() {
        emb_req = emb_req.header("x-api-key", key);
    }
    match emb_req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                match resp.json::<serde_json::Value>().await {
                    Ok(payload) => lines.push(extract_embedding_diag_line(&payload)),
                    Err(err) => {
                        lines.push(format!("embedding_diagnostics: invalid_json ({})", err))
                    }
                }
            } else {
                lines.push(format!(
                    "embedding_diagnostics: unavailable (telemetry/embeddings status={})",
                    status.as_u16()
                ));
                lines.push("embedding_diagnostics: fallback=recall_telemetry".to_string());
            }
        }
        Err(err) => {
            lines.push(format!(
                "embedding_diagnostics: unavailable (unreachable: {})",
                err
            ));
            lines.push("embedding_diagnostics: fallback=recall_telemetry".to_string());
        }
    }

    let mut recall_req = client.get(format!(
        "{}/telemetry/recall",
        base_url.trim_end_matches('/')
    ));
    if let Some(key) = contextlattice_api_key_for_graph() {
        recall_req = recall_req.header("x-api-key", key);
    }
    match recall_req.send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(payload) => {
                let qps = payload
                    .get("query_per_sec")
                    .or_else(|| payload.get("qps"))
                    .and_then(|v| v.as_f64())
                    .map(|v| format!("{:.3}", v))
                    .unwrap_or_else(|| "n/a".to_string());
                let hit_rate = payload
                    .get("hit_rate")
                    .or_else(|| payload.get("grounded_hit_rate"))
                    .and_then(|v| v.as_f64())
                    .map(|v| format!("{:.3}", v))
                    .unwrap_or_else(|| "n/a".to_string());
                lines.push(format!(
                    "recall_telemetry: qps={} hit_rate={}",
                    qps, hit_rate
                ));
            }
            Err(err) => lines.push(format!("recall_telemetry: invalid_json ({})", err)),
        },
        Ok(resp) => lines.push(format!(
            "recall_telemetry: endpoint_status={}",
            resp.status()
        )),
        Err(err) => lines.push(format!("recall_telemetry: unreachable ({})", err)),
    }

    lines
}

pub(crate) async fn handle_graph_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_else(|| "status".to_string());
    match sub.as_str() {
        "status" | "show" => {
            let contextlattice_mcp = app.config.mcp_servers.iter().any(|entry| {
                let name = entry.name.to_ascii_lowercase();
                let url = entry
                    .url
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                name.contains("contextlattice") || url.contains("contextlattice")
            });
            let policy = load_contextlattice_policy().ok();
            let mut out = String::new();
            let _ = writeln!(out, "Graph-memory status");
            let _ = writeln!(out, "  contextlattice_mcp: {}", yes_no(contextlattice_mcp));
            let diag = contextlattice_embedding_diagnostics_lines().await;
            for row in &diag {
                let _ = writeln!(out, "  {}", row);
            }
            if let Some(policy) = policy {
                let _ = writeln!(
                    out,
                    "  retrieval_mode_hint: {}",
                    policy.preferred_retrieval_mode
                );
                let _ = writeln!(out, "  preflight_required: {}", policy.preflight_required);
                let _ = writeln!(
                    out,
                    "  include_grounding_required: {}",
                    policy.include_grounding_required
                );
                let _ = writeln!(
                    out,
                    "  degradation_aware_planning: {}",
                    policy.degradation_aware_planning
                );
            } else {
                let _ = writeln!(out, "  contextlattice_policy: unavailable");
            }
            emit_command_output(app, out.trim_end());
        }
        "embeddings" | "embedding" | "diag" => {
            let mut out = String::new();
            let _ = writeln!(out, "ContextLattice embedding diagnostics");
            let _ = writeln!(out, "base_url: {}", contextlattice_base_url_for_graph());
            let lines = contextlattice_embedding_diagnostics_lines().await;
            if lines.is_empty() {
                out.push_str("no diagnostic lines returned.");
            } else {
                for line in lines {
                    let _ = writeln!(out, "- {}", line);
                }
            }
            out.push_str("\nIf endpoint support is partial, Hermes falls back to `/telemetry/recall` snapshots.");
            emit_command_output(app, out.trim_end());
        }
        "repo" | "semantic" => {
            let mut max_files = 220usize;
            let mut repo_arg: Option<&str> = None;
            let mut idx = 1usize;
            while idx < args.len() {
                if args[idx] == "--max-files" {
                    if let Some(raw) = args.get(idx + 1).copied() {
                        if let Ok(parsed) = raw.parse::<usize>() {
                            max_files = parsed.clamp(20, 1500);
                        }
                        idx += 2;
                        continue;
                    }
                }
                repo_arg = Some(args[idx]);
                idx += 1;
            }
            let repo_root = if let Some(raw) = repo_arg {
                PathBuf::from(raw)
            } else {
                std::env::current_dir()
                    .map_err(|e| AgentError::Io(format!("current_dir: {}", e)))?
            };
            if !repo_root.exists() {
                emit_command_output(
                    app,
                    format!("Repo path does not exist: {}", repo_root.display()),
                );
                return Ok(CommandResult::Handled);
            }

            let mut files = Vec::new();
            collect_graph_candidate_files(&repo_root, max_files, &mut files)?;
            if files.is_empty() {
                emit_command_output(
                    app,
                    format!(
                        "No candidate source files found under {} (max_files={}).",
                        repo_root.display(),
                        max_files
                    ),
                );
                return Ok(CommandResult::Handled);
            }

            let mut edges: HashMap<(String, String), usize> = HashMap::new();
            let mut node_degree: HashMap<String, usize> = HashMap::new();
            for path in &files {
                let rel = path
                    .strip_prefix(&repo_root)
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                let ext = path
                    .extension()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let content = std::fs::read_to_string(path).unwrap_or_default();
                for rf in extract_semantic_refs_for_file(&ext, &content) {
                    let key = (rel.clone(), rf.clone());
                    *edges.entry(key).or_insert(0usize) += 1;
                    *node_degree.entry(rel.clone()).or_insert(0usize) += 1;
                    *node_degree.entry(rf).or_insert(0usize) += 1;
                }
            }

            let mut degree_ranked: Vec<(String, usize)> = node_degree.into_iter().collect();
            degree_ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let mut edge_ranked: Vec<((String, String), usize)> = edges.into_iter().collect();
            edge_ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

            let mut out = String::new();
            let _ = writeln!(out, "Semantic repo graph");
            let _ = writeln!(out, "  repo_root={}", repo_root.display());
            let _ = writeln!(out, "  files_scanned={} (cap={})", files.len(), max_files);
            let _ = writeln!(out, "  semantic_edges={}", edge_ranked.len());
            let _ = writeln!(out);
            let _ = writeln!(out, "Top hubs (degree):");
            for (idx, (node, degree)) in degree_ranked.iter().take(12).enumerate() {
                let _ = writeln!(out, "  {}. {} ({})", idx + 1, node, degree);
            }
            let _ = writeln!(out);
            let _ = writeln!(out, "Top semantic edges:");
            for (idx, ((src, dst), weight)) in edge_ranked.iter().take(16).enumerate() {
                let _ = writeln!(out, "  {}. {} -> {} ({})", idx + 1, src, dst, weight);
            }
            let _ = writeln!(out);
            let _ = writeln!(out, "Mermaid preview:");
            let _ = writeln!(out, "```mermaid");
            let _ = writeln!(out, "graph LR");
            for ((src, dst), _) in edge_ranked.iter().take(32) {
                let src_n = sanitize_graph_node(src);
                let dst_n = sanitize_graph_node(dst);
                let _ = writeln!(out, "  {}[\"{}\"] --> {}[\"{}\"]", src_n, src, dst_n, dst);
            }
            let _ = writeln!(out, "```");
            emit_command_output(app, out.trim_end());
        }
        "help" => emit_command_output(
            app,
            "Usage: /graph [status|embeddings|repo [path] [--max-files N]]",
        ),
        _ => emit_command_output(
            app,
            "Usage: /graph [status|embeddings|repo [path] [--max-files N]]",
        ),
    }
    Ok(CommandResult::Handled)
}

