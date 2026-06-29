use axum::Json;
use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};

use crate::HttpServerState;

pub async fn marketplace() -> Json<Value> {
    Json(json!({
        "servers": [
            {
                "id": "filesystem",
                "name": "Filesystem",
                "description": "Read/write local files via MCP",
                "verified": true
            },
            {
                "id": "github",
                "name": "GitHub",
                "description": "Repository and issue access",
                "verified": true
            }
        ],
        "source": "curated"
    }))
}

pub fn routes() -> Router<HttpServerState> {
    Router::new().route("/api/mcp/marketplace", get(marketplace))
}
