use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use hermes_datasources::{
    AkshareCloudDataSource, AkshareLocalDataSource, DataSourceProvider, DataSourceRegistry,
    UserCustomDataSource, UserCustomDataSourceConfig,
};
use hermes_tasks::{
    ArtifactStore, DeviceId, ForkRequest, SignedUrlConfig, Task, TaskId, TaskListQuery,
    TaskRuntime, UserId, VerticalId, generate_signed_url,
};
use hermes_verticals::VerticalLoader;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::HttpServerState;

#[derive(Clone)]
pub struct TaskApiState {
    pub runtime: Arc<TaskRuntime>,
    pub verticals: Arc<VerticalLoader>,
    pub datasources: Arc<Mutex<DataSourceRegistry>>,
    pub artifacts: Arc<ArtifactStore>,
    pub signed_url: Arc<SignedUrlConfig>,
    pub stream_hub: crate::task_ws::TaskStreamHub,
}

impl TaskApiState {
    pub fn new() -> Result<Self, hermes_tasks::DbError> {
        let db = hermes_tasks::TaskDb::open_default()?;
        let runtime = Arc::new(TaskRuntime::new(db.clone()));
        let mut registry = DataSourceRegistry::new();
        registry.register(Box::new(AkshareCloudDataSource::from_env()));
        registry.register(Box::new(AkshareLocalDataSource::default_bridge()));
        Ok(Self {
            runtime,
            verticals: Arc::new(VerticalLoader::bundled()),
            datasources: Arc::new(Mutex::new(registry)),
            artifacts: Arc::new(ArtifactStore::open(db)?),
            signed_url: Arc::new(SignedUrlConfig::from_env()),
            stream_hub: crate::task_ws::TaskStreamHub::default(),
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateTaskRequest {
    pub title: String,
    pub vertical: Option<String>,
    pub instruction: Option<String>,
    pub owner_user_id: Option<String>,
    pub device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TaskListParams {
    pub owner_user_id: Option<String>,
    pub status: Option<String>,
    pub vertical: Option<String>,
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
pub struct ContinueTaskRequest {
    pub instruction: String,
}

#[derive(Debug, Deserialize)]
pub struct ApproveTaskRequest {
    pub event_id: String,
    pub approved: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ForkTaskRequest {
    pub turn_id: String,
    pub vertical: String,
    pub title: String,
    pub instruction: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateDataSourceRequest {
    pub config: UserCustomDataSourceConfig,
}

pub async fn create_task(
    State(state): State<HttpServerState>,
    Json(req): Json<CreateTaskRequest>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let owner = parse_user_id(req.owner_user_id.as_deref())?;
    let device = parse_device_id(req.device_id.as_deref())?;
    let vertical = req.vertical.map(VerticalId::from);
    let instruction = req.instruction.unwrap_or_default();
    let (task, event) = tasks
        .runtime
        .create_and_run(owner, device, req.title, vertical, &instruction)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    crate::task_agent::spawn_task_agent_run(
        state.clone(),
        task.clone(),
        instruction,
        event.turn_id,
    );
    Ok(Json(json!({ "task": task, "event": event })))
}

pub async fn list_tasks(
    State(state): State<HttpServerState>,
    Query(params): Query<TaskListParams>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let page = tasks
        .runtime
        .tasks()
        .list(&TaskListQuery {
            owner_user_id: params
                .owner_user_id
                .as_deref()
                .map(|s| parse_user_id(Some(s)))
                .transpose()?,
            status: params.status.as_deref().and_then(parse_task_status),
            vertical: params.vertical.map(VerticalId::from),
            cursor: params.cursor,
            limit: params.limit,
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({
        "tasks": page.tasks,
        "next_cursor": page.next_cursor,
    })))
}

pub async fn get_task(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Task>, StatusCode> {
    let tasks = task_state(&state)?;
    let task_id: TaskId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let task = tasks
        .runtime
        .tasks()
        .get(task_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(task))
}

pub async fn delete_task(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let tasks = task_state(&state)?;
    let task_id: TaskId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let deleted = tasks
        .runtime
        .tasks()
        .delete(task_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub async fn list_turns(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let task_id: TaskId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let turns = tasks
        .runtime
        .turns()
        .list_for_task(task_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "turns": turns })))
}

pub async fn list_events(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let task_id: TaskId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let events = tasks
        .runtime
        .events()
        .list_for_task(task_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "events": events })))
}

pub async fn task_toc(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let task_id: TaskId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let turns = tasks
        .runtime
        .turns()
        .list_for_task(task_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let events = tasks
        .runtime
        .events()
        .list_for_task(task_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let toc: Vec<Value> = turns
        .iter()
        .map(|turn| {
            json!({
                "turn_id": turn.id.to_string(),
                "label": turn.label,
                "status": turn.status,
                "events": events.iter().filter(|e| e.turn_id == Some(turn.id)).map(|e| json!({
                    "event_id": e.id.to_string(),
                    "kind": e.kind,
                    "toc_label": e.toc_label,
                    "toc_icon": e.toc_icon,
                    "anchor_slug": e.anchor_slug,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Json(json!({ "toc": toc })))
}

pub async fn continue_task(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    Json(req): Json<ContinueTaskRequest>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let task_id: TaskId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let Some(task) = tasks
        .runtime
        .tasks()
        .get(task_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    else {
        return Err(StatusCode::NOT_FOUND);
    };
    let event = hermes_tasks::TaskEvent::new(
        task_id,
        hermes_tasks::EventKind::Instruction,
        hermes_tasks::Actor::User {
            user_id: task.owner_user_id,
            device_id: task.primary_device_id,
        },
        json!({ "text": req.instruction }),
        "continue",
    );
    let mut event = event;
    let turn = tasks
        .runtime
        .turns()
        .bind_instruction_event(tasks.runtime.events(), &mut event, &req.instruction)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    crate::task_agent::spawn_task_agent_run(
        state.clone(),
        task,
        req.instruction.clone(),
        Some(turn.id),
    );
    Ok(Json(json!({ "event": event, "turn": turn })))
}

pub async fn cancel_task(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let task_id: TaskId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let registry = hermes_tasks::TaskCancellationRegistry::default();
    let cancelled = tasks
        .runtime
        .cancel_task(&registry, task_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "cancelled": cancelled })))
}

pub async fn approve_task(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    Json(req): Json<ApproveTaskRequest>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let task_id: TaskId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let event_id: hermes_tasks::EventId =
        req.event_id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let event = hermes_tasks::TaskEvent::new(
        task_id,
        hermes_tasks::EventKind::ApprovalResponse,
        hermes_tasks::Actor::User {
            user_id: parse_user_id(None)?,
            device_id: parse_device_id(None)?,
        },
        json!({
            "approved": req.approved,
            "reason": req.reason,
            "request_event_id": event_id.to_string(),
        }),
        format!("approval-{}", Utc::now().timestamp_millis()),
    );
    tasks
        .runtime
        .events()
        .append(&event)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "event": event })))
}

pub async fn fork_task(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    Json(req): Json<ForkTaskRequest>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let parent_task_id: TaskId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let parent_turn_id: hermes_tasks::TurnId =
        req.turn_id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let parent = tasks
        .runtime
        .tasks()
        .get(parent_task_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let (subtask, event, turn) = tasks
        .runtime
        .fork_subtask(ForkRequest {
            parent_task_id,
            parent_turn_id,
            owner_user_id: parent.owner_user_id,
            device_id: parent.primary_device_id,
            vertical: VerticalId::from(req.vertical),
            title: req.title,
            instruction: req.instruction,
            persona: hermes_tasks::AgentPersona {
                vertical_id: Some(VerticalId::from("fork")),
                system_prompt: String::new(),
                model_id: None,
                provider_id: None,
            },
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        json!({ "task": subtask, "event": event, "turn": turn }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct VerticalListParams {
    pub category: Option<String>,
    pub search: Option<String>,
}

pub async fn list_verticals(
    State(state): State<HttpServerState>,
    Query(params): Query<VerticalListParams>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let mut verticals = tasks
        .verticals
        .list()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Some(category) = &params.category {
        verticals.retain(|v| v.meta.category == *category);
    }
    if let Some(search) = &params.search {
        let s = search.to_lowercase();
        verticals.retain(|v| {
            v.meta.id.to_lowercase().contains(&s)
                || v.meta.display_name_key.to_lowercase().contains(&s)
        });
    }
    Ok(Json(
        json!({ "verticals": verticals.iter().map(|v| &v.meta).collect::<Vec<_>>() }),
    ))
}

pub async fn get_vertical(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let vertical = tasks
        .verticals
        .load(&id)
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Json(json!({
        "meta": vertical.meta,
        "starters": vertical.starters,
        "datasources": vertical.datasources,
    })))
}

pub async fn list_datasources(
    State(state): State<HttpServerState>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let registry = tasks.datasources.lock().await;
    Ok(Json(json!({ "providers": registry.list_ids() })))
}

pub async fn create_datasource(
    State(state): State<HttpServerState>,
    Json(req): Json<CreateDataSourceRequest>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let provider =
        UserCustomDataSource::new(req.config.clone()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let id = provider.id().to_string();
    tasks.datasources.lock().await.register(Box::new(provider));
    Ok(Json(json!({ "id": id })))
}

pub async fn test_datasource(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let registry = tasks.datasources.lock().await;
    let provider = registry.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    provider
        .test_connection()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn get_artifact(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let tasks = task_state(&state)?;
    let artifact_id: hermes_tasks::ArtifactId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let record = tasks
        .artifacts
        .get(artifact_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let signed = generate_signed_url(&tasks.signed_url, artifact_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "artifact": record, "signed_url": signed })))
}

pub async fn get_artifact_raw(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let tasks = task_state(&state)?;
    let artifact_id: hermes_tasks::ArtifactId = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let bytes = tasks
        .artifacts
        .read_bytes(artifact_id)
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let record = tasks
        .artifacts
        .get(artifact_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    if let Some(range) = headers.get(header::RANGE) {
        if let Ok(range_str) = range.to_str() {
            if let Some(slice) = parse_range(range_str, bytes.len()) {
                let start = slice.start;
                let end = slice.end;
                let body = bytes[slice].to_vec();
                return Ok((
                    StatusCode::PARTIAL_CONTENT,
                    [
                        (header::CONTENT_TYPE, record.mime_type.as_str()),
                        (
                            header::CONTENT_RANGE,
                            &format!("bytes {}-{}/{}", start, end - 1, bytes.len()),
                        ),
                    ],
                    body,
                )
                    .into_response());
            }
        }
    }

    Ok(([(header::CONTENT_TYPE, record.mime_type.as_str())], bytes).into_response())
}

fn parse_range(range: &str, len: usize) -> Option<std::ops::Range<usize>> {
    let range = range.strip_prefix("bytes=")?;
    let (start, end) = range.split_once('-')?;
    let start: usize = start.parse().ok()?;
    let end = if end.is_empty() {
        len
    } else {
        end.parse::<usize>().ok()? + 1
    };
    if start >= len || end > len || start >= end {
        return None;
    }
    Some(start..end)
}

fn task_state(state: &HttpServerState) -> Result<&TaskApiState, StatusCode> {
    state
        .tasks
        .as_ref()
        .map(|t| t.as_ref())
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

fn parse_task_status(s: &str) -> Option<hermes_tasks::TaskStatus> {
    match s {
        "pending" => Some(hermes_tasks::TaskStatus::Pending),
        "running" => Some(hermes_tasks::TaskStatus::Running),
        "needs_approval" => Some(hermes_tasks::TaskStatus::NeedsApproval),
        "done" => Some(hermes_tasks::TaskStatus::Done),
        "failed" => Some(hermes_tasks::TaskStatus::Failed),
        "cancelled" => Some(hermes_tasks::TaskStatus::Cancelled),
        "scheduled" => Some(hermes_tasks::TaskStatus::Scheduled),
        "paused" => Some(hermes_tasks::TaskStatus::Paused),
        _ => None,
    }
}

fn parse_user_id(raw: Option<&str>) -> Result<UserId, StatusCode> {
    match raw {
        Some(s) => s.parse().map_err(|_| StatusCode::BAD_REQUEST),
        None => Ok(UserId::from_ulid(
            ulid::Ulid::from_string("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
        )),
    }
}

fn parse_device_id(raw: Option<&str>) -> Result<DeviceId, StatusCode> {
    match raw {
        Some(s) => s.parse().map_err(|_| StatusCode::BAD_REQUEST),
        None => Ok(DeviceId::from_ulid(
            ulid::Ulid::from_string("01ARZ3NDEKTSV4RRFFQ69G5FB0").unwrap(),
        )),
    }
}

pub fn task_routes() -> axum::Router<HttpServerState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/tasks", post(create_task).get(list_tasks))
        .route("/api/tasks/{id}", get(get_task).delete(delete_task))
        .route(
            "/api/tasks/{id}/stream",
            get(crate::task_ws::task_stream_upgrade),
        )
        .route("/api/tasks/{id}/turns", get(list_turns))
        .route("/api/tasks/{id}/events", get(list_events))
        .route("/api/tasks/{id}/toc", get(task_toc))
        .route("/api/tasks/{id}/continue", post(continue_task))
        .route("/api/tasks/{id}/cancel", post(cancel_task))
        .route("/api/tasks/{id}/approve", post(approve_task))
        .route("/api/tasks/{id}/fork", post(fork_task))
        .route("/api/verticals", get(list_verticals))
        .route("/api/verticals/{id}", get(get_vertical))
        .route(
            "/api/datasources",
            get(list_datasources).post(create_datasource),
        )
        .route("/api/datasources/{id}/test", post(test_datasource))
        .route("/api/artifacts/{id}", get(get_artifact))
        .route("/api/artifacts/{id}/raw", get(get_artifact_raw))
}
