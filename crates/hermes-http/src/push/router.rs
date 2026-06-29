use axum::Json;
use axum::Router;
use axum::routing::post;
use serde::{Deserialize, Serialize};

use crate::HttpServerState;

use super::cn::{CnPushVendor, send_cn};

#[derive(Debug, Deserialize)]
pub struct SendPushRequest {
    pub manufacturer: String,
    pub token: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Serialize)]
pub struct SendPushResponse {
    pub ok: bool,
}

fn vendor_from_manufacturer(m: &str) -> Option<CnPushVendor> {
    match m.to_ascii_lowercase().as_str() {
        "xiaomi" => Some(CnPushVendor::Xiaomi),
        "huawei" => Some(CnPushVendor::Huawei),
        "vivo" => Some(CnPushVendor::Vivo),
        "oppo" => Some(CnPushVendor::Oppo),
        _ => None,
    }
}

pub async fn send_handler(Json(body): Json<SendPushRequest>) -> Json<SendPushResponse> {
    if let Some(vendor) = vendor_from_manufacturer(&body.manufacturer) {
        let _ = send_cn(vendor, &body.token, &body.title, &body.body).await;
    }
    Json(SendPushResponse { ok: true })
}

pub fn routes() -> Router<HttpServerState> {
    Router::new().route("/api/push/send", post(send_handler))
}
