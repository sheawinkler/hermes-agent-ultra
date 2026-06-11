//! Plan-then-execute mode: read-only planning phase, user approval, then write execution.
//!
//! **Scope (v1):** CLI/TUI only (`--plan`, `/plan-mode`). Messaging Gateway sessions keep
//! [`PlanPhase::Off`] until P1 adds channel approval UI.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Turn-level plan mode state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPhase {
    #[default]
    Off,
    Planning,
    AwaitingApproval,
    Executing,
}

impl PlanPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Planning => "planning",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Executing => "executing",
        }
    }
}

/// Read/write classification for a tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRwClass {
    Read,
    Write,
    /// Blocked during Planning regardless of params (terminal, browser mutations).
    ConditionalBlockInPlanning,
}

const READ_TOOLS: &[&str] = &[
    "read_file",
    "search_files",
    "session_search",
    "skill_view",
    "skills_list",
    "web_search",
    "web_extract",
    "vision_analyze",
    "ha_get_state",
    "ha_list_entities",
    "ha_list_services",
    "clarify",
    "browser_navigate",
    "browser_snapshot",
    "browser_go_back",
    "browser_get_images",
    "browser_vision",
];

const WRITE_TOOLS: &[&str] = &[
    "write_file",
    "patch",
    "send_message",
    "delegate_task",
    "code_execution",
    "execute_code",
    "mixture_of_agents",
    "skill_manage",
    "skills_install",
    "skills_uninstall",
    "image_gen",
    "tts",
    "transcription",
    "video_gen",
    "computer_use",
    "capture",
    "spotify_control",
    "homeassistant_call",
];

const BROWSER_WRITE_TOOLS: &[&str] = &[
    "browser_click",
    "browser_type",
    "browser_press",
    "browser_scroll",
    "browser_console",
];

const PROCESS_REGISTRY_READ_ACTIONS: &[&str] = &["list", "status", "output"];

/// Classify a tool call as read, write, or planning-blocked conditional.
pub fn classify_tool(name: &str, params: &Value) -> ToolRwClass {
    if name.starts_with("mcp_") {
        return ToolRwClass::Write;
    }
    if READ_TOOLS.contains(&name) {
        return ToolRwClass::Read;
    }
    if WRITE_TOOLS.contains(&name) {
        return ToolRwClass::Write;
    }
    if BROWSER_WRITE_TOOLS.contains(&name) {
        return ToolRwClass::ConditionalBlockInPlanning;
    }
    if name == "terminal" {
        return ToolRwClass::ConditionalBlockInPlanning;
    }
    if name == "memory" {
        return classify_memory(params);
    }
    if name == "todo" {
        return classify_todo(params);
    }
    if name == "cronjob" {
        return classify_cronjob(params);
    }
    if name == "process_registry" {
        return classify_process_registry(params);
    }
    if name.starts_with("browser_") {
        return ToolRwClass::ConditionalBlockInPlanning;
    }
    // Unknown tools: conservative — block during Planning.
    ToolRwClass::Write
}

fn classify_memory(params: &Value) -> ToolRwClass {
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match action.as_str() {
        "read" | "search" | "list" | "get" => ToolRwClass::Read,
        _ => ToolRwClass::Write,
    }
}

fn classify_todo(params: &Value) -> ToolRwClass {
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("list")
        .to_ascii_lowercase();
    match action.as_str() {
        "list" | "show" | "get" => ToolRwClass::Read,
        _ => ToolRwClass::Write,
    }
}

fn classify_cronjob(params: &Value) -> ToolRwClass {
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match action.as_str() {
        "list" | "status" | "show" => ToolRwClass::Read,
        _ => ToolRwClass::Write,
    }
}

fn classify_process_registry(params: &Value) -> ToolRwClass {
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if PROCESS_REGISTRY_READ_ACTIONS.contains(&action.as_str()) {
        ToolRwClass::Read
    } else {
        ToolRwClass::Write
    }
}

/// Whether the given tool is allowed in the current plan phase.
pub fn plan_allows_tool(phase: PlanPhase, name: &str, params: &Value) -> bool {
    match phase {
        PlanPhase::Off | PlanPhase::Executing | PlanPhase::AwaitingApproval => true,
        PlanPhase::Planning => matches!(classify_tool(name, params), ToolRwClass::Read),
    }
}

/// Structured JSON error returned to the LLM when a write tool is blocked in Planning.
pub fn plan_block_payload(tool_name: &str) -> String {
    json!({
        "error": format!(
            "Blocked by plan mode: tool '{tool_name}' is not allowed during the planning phase. \
             Use read-only tools to research, then submit a structured plan and wait for user approval. \
             Approve with /plan-mode approve after reviewing the plan."
        ),
        "plan": {
            "tool": tool_name,
            "decision": "plan_block",
            "code": "plan_write_denied",
            "phase": "planning",
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_tools_allowed_in_planning() {
        let phase = PlanPhase::Planning;
        assert!(plan_allows_tool(phase, "read_file", &json!({"path": "a.rs"})));
        assert!(plan_allows_tool(phase, "search_files", &json!({"pattern": "foo"})));
        assert!(plan_allows_tool(phase, "web_search", &json!({"query": "x"})));
        assert!(plan_allows_tool(
            phase,
            "memory",
            &json!({"action": "search"})
        ));
        assert!(plan_allows_tool(
            phase,
            "process_registry",
            &json!({"action": "list"})
        ));
    }

    #[test]
    fn write_tools_blocked_in_planning() {
        let phase = PlanPhase::Planning;
        assert!(!plan_allows_tool(phase, "write_file", &json!({"path": "a.rs"})));
        assert!(!plan_allows_tool(phase, "patch", &json!({"path": "a.rs"})));
        assert!(!plan_allows_tool(
            phase,
            "delegate_task",
            &json!({"task": "x"})
        ));
        assert!(!plan_allows_tool(phase, "mcp_server_tool", &json!({})));
    }

    #[test]
    fn terminal_blocked_in_planning() {
        assert!(!plan_allows_tool(
            PlanPhase::Planning,
            "terminal",
            &json!({"command": "ls"})
        ));
    }

    #[test]
    fn write_tools_allowed_in_executing() {
        let phase = PlanPhase::Executing;
        assert!(plan_allows_tool(phase, "write_file", &json!({"path": "a.rs"})));
        assert!(plan_allows_tool(phase, "terminal", &json!({"command": "ls"})));
    }

    #[test]
    fn plan_block_payload_is_valid_json() {
        let payload = plan_block_payload("patch");
        let v: Value = serde_json::from_str(&payload).expect("json");
        assert_eq!(v["plan"]["decision"], "plan_block");
        assert_eq!(v["plan"]["code"], "plan_write_denied");
    }
}
