use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use hermes_tasks::types::UserId;
use hermes_tasks::{CronJob, CronSchedule};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::HttpServerState;

#[derive(Debug, Deserialize)]
pub struct CreateScheduleRequest {
    pub title: String,
    pub prompt_template: String,
    pub expr: String,
    pub timezone: Option<String>,
    pub owner_user_id: Option<String>,
    pub vertical: Option<String>,
}

fn parse_user(raw: Option<&str>) -> Result<UserId, StatusCode> {
    match raw {
        Some(s) => s.parse().map_err(|_| StatusCode::BAD_REQUEST),
        None => Ok(UserId::from_ulid(
            ulid::Ulid::from_string("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
        )),
    }
}

pub async fn create_schedule(
    State(state): State<HttpServerState>,
    Json(req): Json<CreateScheduleRequest>,
) -> Result<Json<Value>, StatusCode> {
    let owner = parse_user(req.owner_user_id.as_deref())?;
    let schedule = CronSchedule {
        expr: req.expr,
        timezone: req.timezone.unwrap_or_else(|| "Asia/Shanghai".to_string()),
        next_run: None,
        last_run: None,
        enabled: true,
    };
    let mut job = CronJob::new(owner, req.title, req.prompt_template, schedule);
    if let Some(v) = req.vertical {
        job.vertical = Some(hermes_tasks::VerticalId::from(v));
    }
    state.cron.upsert(job.clone()).await;
    Ok(Json(json!({ "schedule": job })))
}

pub async fn list_schedules(
    State(state): State<HttpServerState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, StatusCode> {
    let owner = parse_user(params.get("owner_user_id").map(String::as_str))?;
    let schedules = state.cron.list_for_user(owner).await;
    Ok(Json(json!({ "schedules": schedules })))
}

pub async fn get_schedule(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let job = state.cron.get(&id).await.ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({ "schedule": job })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateScheduleRequest {
    pub title: Option<String>,
    pub prompt_template: Option<String>,
    pub expr: Option<String>,
    pub timezone: Option<String>,
    pub enabled: Option<bool>,
    pub vertical: Option<String>,
}

pub async fn update_schedule(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateScheduleRequest>,
) -> Result<Json<Value>, StatusCode> {
    let mut job = state.cron.get(&id).await.ok_or(StatusCode::NOT_FOUND)?;
    if let Some(title) = req.title {
        job.title = title;
    }
    if let Some(prompt_template) = req.prompt_template {
        job.prompt_template = prompt_template;
    }
    if let Some(expr) = req.expr {
        job.schedule.expr = expr;
    }
    if let Some(timezone) = req.timezone {
        job.schedule.timezone = timezone;
    }
    if let Some(enabled) = req.enabled {
        job.enabled = enabled;
        job.schedule.enabled = enabled;
    }
    if let Some(v) = req.vertical {
        job.vertical = Some(hermes_tasks::VerticalId::from(v));
    }
    job.updated_at = chrono::Utc::now();
    state.cron.upsert(job.clone()).await;
    Ok(Json(json!({ "schedule": job })))
}

pub async fn compat_cron_jobs(State(state): State<HttpServerState>) -> Json<Value> {
    let jobs = state.cron.list_all().await;
    Json(json!(jobs))
}

pub async fn delete_schedule(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    if state.cron.delete(&id).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub async fn schedule_next_run(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let job = state.cron.get(&id).await.ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({
        "schedule_id": id,
        "next_run": job.schedule.next_run,
    })))
}

pub async fn schedule_history(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let job = state.cron.get(&id).await.ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({
        "schedule_id": id,
        "last_task_id": job.last_task_id,
        "last_run": job.schedule.last_run,
    })))
}

pub fn routes() -> Router<HttpServerState> {
    Router::new()
        .route("/api/schedules", post(create_schedule).get(list_schedules))
        .route(
            "/api/schedules/{id}",
            get(get_schedule)
                .put(update_schedule)
                .delete(delete_schedule),
        )
        .route("/api/schedules/{id}/next-run", get(schedule_next_run))
        .route("/api/schedules/{id}/history", get(schedule_history))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn w16_cron_http_api_create_request_fields() {
        let raw = r#"{"title":"t","prompt_template":"p","expr":"0 9 * * *"}"#;
        let req: CreateScheduleRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.title, "t");
        assert_eq!(req.expr, "0 9 * * *");
    }
}
