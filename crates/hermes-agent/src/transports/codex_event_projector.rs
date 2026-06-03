//! Projects codex app-server `item/completed` notifications into Hermes messages.

use serde_json::{json, Value};

use hermes_core::{FunctionCall, Message, MessageRole, ToolCall};

#[derive(Debug, Default, Clone)]
pub struct ProjectionResult {
    pub messages: Vec<Message>,
    pub is_tool_iteration: bool,
    pub final_text: Option<String>,
}

pub struct CodexEventProjector {
    pending_reasoning: Vec<String>,
}

impl Default for CodexEventProjector {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexEventProjector {
    pub fn new() -> Self {
        Self {
            pending_reasoning: Vec::new(),
        }
    }

    pub fn project(&mut self, notification: &Value) -> ProjectionResult {
        let method = notification.get("method").and_then(|v| v.as_str()).unwrap_or("");
        if method != "item/completed" {
            return ProjectionResult::default();
        }
        let item = notification
            .pointer("/params/item")
            .cloned()
            .unwrap_or(Value::Null);
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");

        match item_type {
            "agentMessage" => self.project_agent_message(&item),
            "reasoning" => {
                if let Some(summary) = item.get("summary").and_then(|v| v.as_array()) {
                    for s in summary {
                        if let Some(t) = s.as_str() {
                            self.pending_reasoning.push(t.to_string());
                        }
                    }
                }
                if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                    for s in content {
                        if let Some(t) = s.as_str() {
                            self.pending_reasoning.push(t.to_string());
                        }
                    }
                }
                ProjectionResult::default()
            }
            "commandExecution" => self.project_command(&item, item_id),
            "fileChange" => self.project_file_change(&item, item_id),
            "mcpToolCall" => self.project_mcp_tool_call(&item, item_id),
            "dynamicToolCall" => self.project_dynamic_tool_call(&item, item_id),
            "userMessage" => self.project_user_message(&item),
            _ => self.project_opaque(&item, item_type),
        }
    }

    fn project_agent_message(&mut self, item: &Value) -> ProjectionResult {
        let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut msg = Message::assistant(&text);
        if !self.pending_reasoning.is_empty() {
            msg.reasoning_content = Some(self.pending_reasoning.join("\n"));
            self.pending_reasoning.clear();
        }
        ProjectionResult {
            messages: vec![msg],
            is_tool_iteration: false,
            final_text: Some(text),
        }
    }

    fn project_user_message(&self, item: &Value) -> ProjectionResult {
        let mut parts = Vec::new();
        if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
            for fragment in content {
                if fragment.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(t) = fragment.get("text").and_then(|v| v.as_str()) {
                        parts.push(t.to_string());
                    }
                } else if let Some(t) = fragment.get("text").and_then(|v| v.as_str()) {
                    parts.push(t.to_string());
                }
            }
        }
        ProjectionResult {
            messages: vec![Message::user(parts.join("\n"))],
            is_tool_iteration: false,
            final_text: None,
        }
    }

    fn project_command(&mut self, item: &Value, item_id: &str) -> ProjectionResult {
        let call_id = deterministic_call_id("exec", item_id);
        let args = json!({
            "command": item.get("command").and_then(|v| v.as_str()).unwrap_or(""),
            "cwd": item.get("cwd").and_then(|v| v.as_str()).unwrap_or(""),
        });
        let mut assistant = Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: call_id.clone(),
                function: FunctionCall {
                    name: "exec_command".into(),
                    arguments: args.to_string(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: self.take_reasoning(),
            cache_control: None,
        };
        let mut output = item
            .get("aggregatedOutput")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(code) = item.get("exitCode").and_then(|v| v.as_i64()) {
            if code != 0 {
                output = format!("[exit {code}]\n{output}");
            }
        }
        let tool_msg = Message {
            role: MessageRole::Tool,
            content: Some(output),
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
            reasoning_content: None,
            cache_control: None,
        };
        ProjectionResult {
            messages: vec![assistant, tool_msg],
            is_tool_iteration: true,
            final_text: None,
        }
    }

    fn project_file_change(&mut self, item: &Value, item_id: &str) -> ProjectionResult {
        let call_id = deterministic_call_id("apply_patch", item_id);
        let mut changes_summary = Vec::new();
        if let Some(changes) = item.get("changes").and_then(|v| v.as_array()) {
            for change in changes {
                let kind = change
                    .pointer("/kind/type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("update");
                let path = change.get("path").and_then(|v| v.as_str()).unwrap_or("");
                changes_summary.push(json!({ "kind": kind, "path": path }));
            }
        }
        let args = json!({ "changes": changes_summary });
        let assistant = Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: call_id.clone(),
                function: FunctionCall {
                    name: "apply_patch".into(),
                    arguments: args.to_string(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: self.take_reasoning(),
            cache_control: None,
        };
        let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
        let n = changes_summary.len();
        let tool_msg = Message {
            role: MessageRole::Tool,
            content: Some(format!("apply_patch status={status}, {n} change(s)")),
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
            reasoning_content: None,
            cache_control: None,
        };
        ProjectionResult {
            messages: vec![assistant, tool_msg],
            is_tool_iteration: true,
            final_text: None,
        }
    }

    fn project_mcp_tool_call(&mut self, item: &Value, item_id: &str) -> ProjectionResult {
        let server = item.get("server").and_then(|v| v.as_str()).unwrap_or("mcp");
        let tool = item.get("tool").and_then(|v| v.as_str()).unwrap_or("unknown");
        let call_id = deterministic_call_id(&format!("mcp_{server}_{tool}"), item_id);
        let args = item.get("arguments").cloned().unwrap_or(json!({}));
        let args_val = if args.is_object() {
            args
        } else {
            json!({ "arguments": args })
        };
        let assistant = Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: call_id.clone(),
                function: FunctionCall {
                    name: format!("mcp.{server}.{tool}"),
                    arguments: args_val.to_string(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: self.take_reasoning(),
            cache_control: None,
        };
        let content = if let Some(err) = item.get("error") {
            format!("[error] {}", err.to_string().chars().take(1000).collect::<String>())
        } else if let Some(result) = item.get("result") {
            result.to_string().chars().take(4000).collect()
        } else {
            String::new()
        };
        let tool_msg = Message {
            role: MessageRole::Tool,
            content: Some(content),
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
            reasoning_content: None,
            cache_control: None,
        };
        ProjectionResult {
            messages: vec![assistant, tool_msg],
            is_tool_iteration: true,
            final_text: None,
        }
    }

    fn project_dynamic_tool_call(&mut self, item: &Value, item_id: &str) -> ProjectionResult {
        let tool = item.get("tool").and_then(|v| v.as_str()).unwrap_or("unknown");
        let call_id = deterministic_call_id(tool, item_id);
        let args = item.get("arguments").cloned().unwrap_or(json!({}));
        let assistant = Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: call_id.clone(),
                function: FunctionCall {
                    name: tool.to_string(),
                    arguments: args.to_string(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: self.take_reasoning(),
            cache_control: None,
        };
        let content = item
            .get("result")
            .map(|r| r.to_string().chars().take(4000).collect::<String>())
            .unwrap_or_default();
        let tool_msg = Message {
            role: MessageRole::Tool,
            content: Some(content),
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
            reasoning_content: None,
            cache_control: None,
        };
        ProjectionResult {
            messages: vec![assistant, tool_msg],
            is_tool_iteration: true,
            final_text: None,
        }
    }

    fn project_opaque(&self, item: &Value, item_type: &str) -> ProjectionResult {
        let note = format!("[codex item: {item_type}] {}", item);
        ProjectionResult {
            messages: vec![Message::assistant(note)],
            is_tool_iteration: false,
            final_text: None,
        }
    }

    fn take_reasoning(&mut self) -> Option<String> {
        if self.pending_reasoning.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending_reasoning).join("\n"))
        }
    }
}

fn deterministic_call_id(item_type: &str, item_id: &str) -> String {
    if !item_id.is_empty() {
        return format!("codex_{item_type}_{item_id}");
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(item_type.as_bytes());
    format!("codex_{item_type}_{}", hex::encode(&digest[..8]))
}

pub fn has_turn_aborted_marker(text: &str) -> bool {
    text.contains("<turn_aborted>") || text.contains("<turn_aborted/>")
}
