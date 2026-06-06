//! First-class runtime runbooks for common operator recovery paths.
//!
//! The CLI exposes these through `/runbook`; this tool makes the same catalog
//! available to agents without requiring a TUI slash command round-trip.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

const TOOL_NAME: &str = "runbook_control";

#[derive(Debug, Clone, Copy)]
pub struct RuntimeRunbook {
    pub name: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub steps: &'static [&'static str],
    pub related_commands: &'static [&'static str],
    pub tags: &'static [&'static str],
}

const RUNBOOKS: &[RuntimeRunbook] = &[
    RuntimeRunbook {
        name: "auth-refresh",
        title: "Provider auth/session rejected",
        summary: "Recover from expired provider credentials or OAuth session drift.",
        steps: &[
            "`/auth status`",
            "`/auth refresh`",
            "retry prompt",
            "if still failing, run `/model` and confirm provider/model pair is valid for your account",
        ],
        related_commands: &["/auth status", "/auth refresh", "/model"],
        tags: &["auth", "provider", "oauth"],
    },
    RuntimeRunbook {
        name: "model-not-found",
        title: "Catalog drift or unknown model",
        summary: "Recover when the configured model is absent from the active provider catalog.",
        steps: &[
            "`/model` and select a valid catalog model",
            "retry request",
            "if provider alias was stale, run `/auth verify` and re-check",
        ],
        related_commands: &["/model", "/auth verify"],
        tags: &["model", "provider", "catalog"],
    },
    RuntimeRunbook {
        name: "contextlattice-connect",
        title: "Local memory integration bootstrap",
        summary: "Verify ContextLattice integration through registered tools and durable checkpointing.",
        steps: &[
            "ensure ContextLattice tools are registered via `/tools`",
            "ask agent to run `contextlattice_search` first, not shell command `contextlattice`",
            "checkpoint verified integration via `contextlattice_write`",
        ],
        related_commands: &["/tools", "contextlattice_search", "contextlattice_write"],
        tags: &["memory", "contextlattice", "checkpoint"],
    },
    RuntimeRunbook {
        name: "tool-policy-deny",
        title: "Blocked by policy or sandbox profile",
        summary: "Recover from policy denials without weakening the active safety posture.",
        steps: &[
            "inspect denial reason in tool card `[remediation]` section",
            "remove secret-like args from inline command payload",
            "retry with safer params or approved tool route (`/tools`)",
        ],
        related_commands: &["/tools", "/policy status", "tool_policy_simulate"],
        tags: &["policy", "sandbox", "tools"],
    },
    RuntimeRunbook {
        name: "stream-finalization",
        title: "Stream done but transcript not finalized",
        summary: "Recover when streamed output has completed but the transcript still appears stale.",
        steps: &[
            "wait for final transcript writeback; status shows `Finalizing response...`",
            "avoid submitting a new prompt until finalization completes",
            "if UI appears stale, use Ctrl+G to refresh and jump latest",
        ],
        related_commands: &["Ctrl+G", "/telemetry lane"],
        tags: &["tui", "streaming", "transcript"],
    },
    RuntimeRunbook {
        name: "replay-trace",
        title: "Deterministic replay trace investigation",
        summary: "Inspect, verify, export, and diff replay traces for runtime reproducibility evidence.",
        steps: &[
            "`/raw trace status` or `replay_trace_control {\"action\":\"status\"}`",
            "`/raw trace verify` or `replay_trace_control {\"action\":\"verify\"}`",
            "export a bounded trace window with `/raw trace export` or `replay_trace_control {\"action\":\"export\"}`",
            "compare exports with `/studio replay diff` or `replay_trace_control {\"action\":\"diff\"}`",
        ],
        related_commands: &[
            "/raw trace status",
            "/raw trace verify",
            "/raw trace export",
            "/studio replay diff",
            "replay_trace_control",
        ],
        tags: &["replay", "trace", "debugging"],
    },
];

#[derive(Clone, Default)]
pub struct RunbookControlHandler;

impl RunbookControlHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for RunbookControlHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
            .trim()
            .to_ascii_lowercase();

        match action.as_str() {
            "list" | "status" => Ok(json!({
                "status": "ok",
                "count": RUNBOOKS.len(),
                "runbooks": RUNBOOKS.iter().map(runbook_summary_json).collect::<Vec<_>>(),
                "usage": "runbook_control {\"action\":\"show\",\"name\":\"auth-refresh\"}",
            })
            .to_string()),
            "show" => {
                let name = params
                    .get("name")
                    .or_else(|| params.get("runbook"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| ToolError::InvalidParams("show requires name".to_string()))?;
                let runbook = find_runbook(name).ok_or_else(|| {
                    ToolError::InvalidParams(format!(
                        "unknown runbook '{name}'; use action=list for available runbooks"
                    ))
                })?;
                Ok(json!({
                    "status": "ok",
                    "runbook": runbook_json(runbook),
                })
                .to_string())
            }
            "help" => Ok(json!({
                "status": "ok",
                "tool": TOOL_NAME,
                "actions": ["list", "status", "show", "help"],
                "notes": [
                    "mirrors `/runbook` recovery guidance without requiring a TUI",
                    "use action=show with a runbook name for structured recovery steps"
                ],
            })
            .to_string()),
            _ => Err(ToolError::InvalidParams(format!(
                "unknown action '{action}'; expected list|status|show|help"
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["list", "status", "show", "help"],
                "description": "Runbook action. Defaults to list."
            }),
        );
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Runbook name for show action."
            }),
        );
        tool_schema(
            TOOL_NAME,
            "List and show deterministic runtime recovery runbooks.",
            JsonSchema::object(props, vec![]),
        )
    }
}

pub fn runbooks() -> &'static [RuntimeRunbook] {
    RUNBOOKS
}

pub fn find_runbook(name: &str) -> Option<&'static RuntimeRunbook> {
    let token = normalize_name(name);
    RUNBOOKS
        .iter()
        .find(|runbook| normalize_name(runbook.name) == token)
}

pub fn render_runbook_list() -> String {
    let mut out = String::from("Runbooks\n");
    for runbook in RUNBOOKS {
        out.push_str("- ");
        out.push_str(runbook.name);
        out.push_str(": ");
        out.push_str(runbook.title);
        out.push('\n');
    }
    out.push_str("\nUse `/runbook show <name>`.");
    out
}

pub fn render_runbook(runbook: &RuntimeRunbook) -> String {
    let mut out = format!("Runbook: {}\n{}", runbook.name, runbook.summary);
    for (idx, step) in runbook.steps.iter().enumerate() {
        out.push('\n');
        out.push_str(&(idx + 1).to_string());
        out.push_str(") ");
        out.push_str(step);
    }
    if !runbook.related_commands.is_empty() {
        out.push_str("\nrelated: ");
        out.push_str(&runbook.related_commands.join(", "));
    }
    out
}

fn normalize_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace(['_', ' '], "-")
}

fn runbook_summary_json(runbook: &RuntimeRunbook) -> Value {
    json!({
        "name": runbook.name,
        "title": runbook.title,
        "summary": runbook.summary,
        "tags": runbook.tags,
    })
}

fn runbook_json(runbook: &RuntimeRunbook) -> Value {
    json!({
        "name": runbook.name,
        "title": runbook.title,
        "summary": runbook.summary,
        "steps": runbook.steps,
        "related_commands": runbook.related_commands,
        "tags": runbook.tags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn list_returns_structured_runbook_catalog() {
        let handler = RunbookControlHandler::new();
        let payload: Value =
            serde_json::from_str(&handler.execute(json!({"action":"list"})).await.unwrap())
                .expect("json");

        assert_eq!(payload["status"], "ok");
        assert!(payload["count"].as_u64().unwrap() >= 5);
        assert!(payload["runbooks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "auth-refresh"));
    }

    #[tokio::test]
    async fn show_accepts_normalized_names_and_returns_steps() {
        let handler = RunbookControlHandler::new();
        let payload: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"show","name":"tool_policy_deny"}))
                .await
                .unwrap(),
        )
        .expect("json");

        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["runbook"]["name"], "tool-policy-deny");
        assert!(payload["runbook"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step.as_str().unwrap().contains("remediation")));
    }

    #[tokio::test]
    async fn unknown_runbook_is_invalid_params() {
        let handler = RunbookControlHandler::new();
        let err = handler
            .execute(json!({"action":"show","name":"missing"}))
            .await
            .expect_err("unknown runbook should fail");
        match err {
            ToolError::InvalidParams(message) => {
                assert!(message.contains("unknown runbook"));
                assert!(message.contains("action=list"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn cli_renderers_share_the_tool_catalog() {
        let list = render_runbook_list();
        assert!(list.contains("auth-refresh"));
        assert!(list.contains("replay-trace"));

        let runbook = find_runbook("contextlattice connect").expect("runbook");
        let rendered = render_runbook(runbook);
        assert!(rendered.contains("contextlattice_search"));
        assert!(rendered.contains("contextlattice_write"));
    }
}
