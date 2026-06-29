use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::HttpServerState;

#[derive(Debug, Deserialize)]
pub struct TranscribeRequest {
    pub audio_base64: String,
    pub language: Option<String>,
}

pub async fn transcribe(
    State(_state): State<HttpServerState>,
    Json(req): Json<TranscribeRequest>,
) -> Result<Json<Value>, StatusCode> {
    if req.audio_base64.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(json!({
        "text": "",
        "language": req.language.unwrap_or_else(|| "auto".to_string()),
        "provider": "stub",
        "note": "Configure hermes-audio or cloud STT for production transcription"
    })))
}

pub fn routes() -> Router<HttpServerState> {
    Router::new().route("/api/voice/transcribe", post(transcribe))
}
