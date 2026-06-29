use std::sync::Arc;

use hermes_gateway::gateway::IncomingMessage;
use hermes_tasks::types::{Actor, EventKind, Task, TaskEvent, TaskStatus, TurnId};
use hermes_tasks::{TaskRuntime, types::TaskId};
use serde_json::json;
use tracing::{debug, warn};

use crate::task_ws::TaskStreamHub;
use crate::{HTTP_PLATFORM, HttpServerState};

pub fn spawn_task_agent_run(
    state: HttpServerState,
    task: Task,
    instruction: String,
    turn_id: Option<TurnId>,
) {
    tokio::spawn(async move {
        if let Err(err) = run_task_agent(&state, &task, &instruction, turn_id).await {
            warn!(task_id = %task.id, error = %err, "task agent run failed");
        }
    });
}

async fn run_task_agent(
    state: &HttpServerState,
    task: &Task,
    instruction: &str,
    turn_id: Option<TurnId>,
) -> Result<(), String> {
    let Some(tasks) = state.tasks.as_ref() else {
        return Err("task api unavailable".into());
    };
    let runtime = tasks.runtime.clone();
    let hub = tasks.stream_hub.clone();

    let system = append_event(
        &runtime,
        &hub,
        task.id,
        EventKind::System,
        Actor::System,
        json!({ "text": "Agent run started" }),
        "agent-start",
        turn_id,
    )?;
    debug!(event_id = %system.id, "task agent system event");

    state.outbound.clear_chat(&task.id.to_string());
    let incoming = IncomingMessage {
        platform: HTTP_PLATFORM.to_string(),
        chat_id: task.id.to_string(),
        user_id: task.owner_user_id.to_string(),
        text: instruction.to_string(),
        media_urls: vec![],
        media_types: vec![],
        message_id: None,
        is_dm: false,
        interaction_id: None,
        interaction_token: None,
        role_ids: vec![],
        parent_channel_id: None,
        channel_prompt: None,
        channel_skills: vec![],
        channel_topic: None,
        message_thread_id: None,
    };

    state
        .gateway
        .route_message(&incoming)
        .await
        .map_err(|e| e.to_string())?;

    let parts = state.outbound.drain_chat(&task.id.to_string());
    let reply = if parts.is_empty() {
        "(no agent output — configure LLM provider in hermes config)".to_string()
    } else {
        parts.join("\n")
    };

    append_event(
        &runtime,
        &hub,
        task.id,
        EventKind::Message,
        Actor::Agent {
            model_id: state
                .config
                .model
                .clone()
                .unwrap_or_else(|| "openai:gpt-4o".to_string()),
            provider_id: "hermes-http".to_string(),
        },
        json!({ "text": reply, "role": "assistant" }),
        "assistant-reply",
        turn_id,
    )?;

    let mut updated = task.clone();
    updated.status = TaskStatus::Done;
    updated.updated_at = chrono::Utc::now();
    runtime
        .tasks()
        .update(&updated)
        .map_err(|e| e.to_string())?;

    Ok(())
}

fn append_event(
    runtime: &Arc<TaskRuntime>,
    hub: &TaskStreamHub,
    task_id: TaskId,
    kind: EventKind,
    actor: Actor,
    payload: serde_json::Value,
    anchor: &str,
    turn_id: Option<TurnId>,
) -> Result<TaskEvent, String> {
    let mut event = TaskEvent::new(task_id, kind, actor, payload, anchor);
    event.turn_id = turn_id;
    runtime.events().append(&event).map_err(|e| e.to_string())?;
    let hub = hub.clone();
    let event_clone = event.clone();
    tokio::spawn(async move {
        hub.publish(task_id, event_clone).await;
    });
    Ok(event)
}
