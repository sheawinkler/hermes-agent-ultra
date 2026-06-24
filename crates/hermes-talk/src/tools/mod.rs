pub mod execute;
pub mod hermes;
pub mod hermes_queue;

use tracing::info;

use crate::config::LlmConfig;
use crate::error::{DemoError, Result};
use crate::llm::{FunctionDef, ToolDefinition};
use crate::tools::hermes_queue::{HermesPriority, HermesQueueSender, ListResult};

const SHUTUP_TOOL: &str = r#"{
  "type": "object",
  "properties": {},
  "required": []
}"#;

const EXECUTE_TOOL: &str = r#"{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "description": "要执行的单行shell命令（不含&& ; |等链式操作），执行时间必须<=5s"
    }
  },
  "required": ["command"]
}"#;

const HERMES_TOOL: &str = r#"{
  "type": "object",
  "properties": {
    "text": {
      "type": "string",
      "description": "发送给智能助手的问题，用于联网搜索、复杂推理、多步操作、定时任务等"
    },
    "model": {
      "type": "string",
      "description": "指定使用的模型（可选，留空使用默认模型）"
    },
    "provider": {
      "type": "string",
      "description": "指定模型提供者（可选，留空使用默认提供者）"
    },
    "priority": {
      "type": "string",
      "enum": ["high", "normal", "low"],
      "description": "请求优先级：high(高)、normal(普通)、low(低)，默认 normal"
    },
    "spoken": {
      "type": "string",
      "description": "给用户的自然口语播报文本，简述你即将帮用户处理什么任务。例如'帮你查一下今天的天气''我看看北京到上海的航班'。必须口语化、自然，不要模板化开头。"
    }
  },
  "required": ["text", "spoken"]
}"#;

const CANCEL_HERMES_TOOL: &str = r#"{
  "type": "object",
  "properties": {
    "request_id": {
      "type": "string",
      "description": "要取消的 call_hermes 请求 ID"
    }
  },
  "required": ["request_id"]
}"#;

const LIST_HERMES_TOOL: &str = r#"{
  "type": "object",
  "properties": {
    "request_id": {
      "type": "string",
      "description": "可选，指定要查询的任务请求ID。不填则列出全部等待中和已完成的任务"
    }
  },
  "required": []
}"#;

pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            r#type: "function".to_string(),
            function: FunctionDef {
                name: "execute".to_string(),
                description: "执行简单命令（一行、≤5秒、无交互）。仅用于查看信息。Windows: PowerShell cmdlet需用 powershell -Command \"...\"，cmd内置命令需用 cmd /c ...。不可修改文件或执行危险操作。".to_string(),
                parameters: serde_json::from_str(EXECUTE_TOOL).unwrap(),
            },
        },
        ToolDefinition {
            r#type: "function".to_string(),
            function: FunctionDef {
                name: "call_hermes".to_string(),
                description: "将复杂问题交给 hermes 异步处理（联网搜索、复杂推理、多步操作、定时任务等）。调用后仅收到入队确认，不代表任务完成——hermes 处理完后会主动推送真实结果，你届时再向用户播报。调用后你只能说'已帮你提交请求等待处理'，严禁说'已完成''已设置'等表示任务已结束的话。仅当execute无法满足需求时使用。".to_string(),
                parameters: serde_json::from_str(HERMES_TOOL).unwrap(),
            },
        },
        ToolDefinition {
            r#type: "function".to_string(),
            function: FunctionDef {
                name: "cancel_call_hermes".to_string(),
                description: "取消一个尚未开始的 call_hermes 请求（仅能取消还在队列中等待的，无法取消正在执行的）。".to_string(),
                parameters: serde_json::from_str(CANCEL_HERMES_TOOL).unwrap(),
            },
        },
        ToolDefinition {
            r#type: "function".to_string(),
            function: FunctionDef {
                name: "list_call_hermes".to_string(),
                description: "查看所有已交给 hermes 的任务状态。不填请求ID则列出全部等待中和已完成的任务及其历史，填写请求ID则只查该任务的状态。".to_string(),
                parameters: serde_json::from_str(LIST_HERMES_TOOL).unwrap(),
            },
        },
        ToolDefinition {
            r#type: "function".to_string(),
            function: FunctionDef {
                name: "shutup".to_string(),
                description: "用户要求安静/闭嘴/不再说话时调用。调用后系统将进入休眠模式，下次对话前需要叫唤醒词才能继续交互。".to_string(),
                parameters: serde_json::from_str(SHUTUP_TOOL).unwrap(),
            },
        },
    ]
}

pub async fn execute_tool(
    name: &str,
    arguments: &str,
    cfg: &LlmConfig,
    hermes_sender: Option<&HermesQueueSender>,
) -> Result<String> {
    info!(%name, arguments, "tool: executing");

    match name {
        "execute" => {
            let args: serde_json::Value = serde_json::from_str(arguments)
                .map_err(|e| DemoError::Tool(format!("execute: invalid arguments JSON: {e}")))?;
            let command = args["command"]
                .as_str()
                .ok_or_else(|| DemoError::Tool("execute: missing 'command'".to_string()))?;

            let tokens = execute::validate_command(command, &cfg.execute_allowlist)?;
            execute::run_command(tokens).await
        }
        "call_hermes" => {
            let args: serde_json::Value = serde_json::from_str(arguments).map_err(|e| {
                DemoError::Tool(format!("call_hermes: invalid arguments JSON: {e}"))
            })?;
            let text = args["text"]
                .as_str()
                .ok_or_else(|| DemoError::Tool("call_hermes: missing 'text'".to_string()))?;

            let model = args["model"].as_str().map(|s| s.to_string());
            let provider = args["provider"].as_str().map(|s| s.to_string());
            let priority = args["priority"]
                .as_str()
                .map(HermesPriority::from_str)
                .unwrap_or(HermesPriority::Normal);

            let sender = hermes_sender.ok_or_else(|| {
                DemoError::Tool("call_hermes: hermes queue not available".to_string())
            })?;

            let request_id = sender
                .add_request(text.to_string(), priority, model, provider)
                .await?;
            Ok(format!("call_hermes 请求已入队, ID: {request_id}"))
        }
        "cancel_call_hermes" => {
            let args: serde_json::Value = serde_json::from_str(arguments).map_err(|e| {
                DemoError::Tool(format!("cancel_call_hermes: invalid arguments JSON: {e}"))
            })?;
            let request_id = args["request_id"].as_str().ok_or_else(|| {
                DemoError::Tool("cancel_call_hermes: missing 'request_id'".to_string())
            })?;

            let sender = hermes_sender.ok_or_else(|| {
                DemoError::Tool("cancel_call_hermes: hermes queue not available".to_string())
            })?;

            let found = sender.cancel_request(request_id).await?;
            if found {
                Ok(format!("已取消请求 {request_id}"))
            } else {
                Ok(format!("未找到请求 {request_id}（可能已开始执行或已完成）"))
            }
        }
        "list_call_hermes" => {
            let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
            let request_id = args["request_id"].as_str().map(|s| s.to_string());

            let sender = hermes_sender.ok_or_else(|| {
                DemoError::Tool("list_call_hermes: hermes queue not available".to_string())
            })?;

            let result = sender.list_tasks(request_id).await?;
            Ok(format_list_result(&result))
        }
        "shutup" => Ok("shutup: 系统已进入休眠模式。".to_string()),
        other => Err(DemoError::Tool(format!("unknown tool: {other}"))),
    }
}

pub fn extract_spoken(arguments: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| v["spoken"].as_str().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
}

pub fn generate_hermes_spoken(arguments: &str) -> Option<String> {
    let text = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|v| v["text"].as_str().map(|s| s.to_string()))?;
    if text.is_empty() {
        return None;
    }
    if text.len() > 60 {
        Some(format!("{}...", text.chars().take(60).collect::<String>()))
    } else {
        Some(text)
    }
}

fn format_list_result(r: &ListResult) -> String {
    let mut out = String::new();

    if r.pending.is_empty() && r.history.is_empty() {
        return "当前没有 waiting 或已完成的 hermes 任务。".to_string();
    }

    if !r.pending.is_empty() {
        out.push_str(&format!("等待中 ({}):\n", r.pending.len()));
        for t in &r.pending {
            out.push_str(&format!(
                "- [{}] ({}) {}（已排队{}秒）\n",
                t.request_id, t.priority, t.text, t.created_at_secs
            ));
        }
    }

    if !r.history.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("已完成 ({}):\n", r.history.len()));
        for c in &r.history {
            let status_label = if c.status == "ok" { "成功" } else { "失败" };
            out.push_str(&format!(
                "- [{}] ({status_label}) {}\n",
                c.request_id, c.text,
            ));
        }
    }

    out
}
