use axum::Json;
use axum::Router;
use axum::routing::post;
use serde::{Deserialize, Serialize};

use crate::HttpServerState;

#[derive(Debug, Deserialize)]
pub struct RegisterPushRequest {
    pub device_id: String,
    pub token: String,
    pub platform: String,
    pub manufacturer: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterPushResponse {
    pub ok: bool,
}

pub async fn register_handler(Json(body): Json<RegisterPushRequest>) -> Json<RegisterPushResponse> {
    let _ = body;
    Json(RegisterPushResponse { ok: true })
}

pub fn routes() -> Router<HttpServerState> {
    Router::new().route("/api/push/register", post(register_handler))
}
