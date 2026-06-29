use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use hermes_tasks::types::{Device, DeviceRole, UserId};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::HttpServerState;

#[derive(Clone, Default)]
pub struct DeviceRegistry {
    inner: Arc<Mutex<Vec<Device>>>,
}

impl DeviceRegistry {
    pub async fn register(&self, device: Device) -> Device {
        let mut guard = self.inner.lock().await;
        if device.role == DeviceRole::Primary {
            for existing in guard.iter_mut() {
                if existing.owner_user_id == device.owner_user_id
                    && existing.role == DeviceRole::Primary
                {
                    existing.role = DeviceRole::Secondary;
                }
            }
        }
        guard.push(device.clone());
        device
    }

    pub async fn list_for_user(&self, owner: UserId) -> Vec<Device> {
        self.inner
            .lock()
            .await
            .iter()
            .filter(|d| d.owner_user_id == owner)
            .cloned()
            .collect()
    }

    pub async fn get(&self, id: &str) -> Option<Device> {
        self.inner
            .lock()
            .await
            .iter()
            .find(|d| d.id.to_string() == id)
            .cloned()
    }
}

#[derive(Debug, Deserialize)]
pub struct RegisterDeviceRequest {
    pub name: String,
    pub platform: String,
    pub role: Option<DeviceRole>,
    pub owner_user_id: Option<String>,
}

fn parse_user(raw: Option<&str>) -> Result<UserId, StatusCode> {
    match raw {
        Some(s) => s.parse().map_err(|_| StatusCode::BAD_REQUEST),
        None => Ok(UserId::from_ulid(
            ulid::Ulid::from_string("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
        )),
    }
}

pub async fn register_device(
    State(state): State<HttpServerState>,
    Json(req): Json<RegisterDeviceRequest>,
) -> Result<Json<Value>, StatusCode> {
    let owner = parse_user(req.owner_user_id.as_deref())?;
    let role = req.role.unwrap_or(DeviceRole::Secondary);
    let device = Device::new(owner, req.name, req.platform, role);
    let registered = state.devices.register(device).await;
    Ok(Json(json!({ "device": registered })))
}

pub async fn list_devices(
    State(state): State<HttpServerState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, StatusCode> {
    let owner = parse_user(params.get("owner_user_id").map(String::as_str))?;
    let devices = state.devices.list_for_user(owner).await;
    Ok(Json(json!({ "devices": devices })))
}

pub async fn get_device(
    State(state): State<HttpServerState>,
    Path(id): Path<String>,
) -> Result<Json<Device>, StatusCode> {
    let device = state.devices.get(&id).await.ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(device))
}

pub fn routes() -> Router<HttpServerState> {
    Router::new()
        .route("/api/devices", post(register_device).get(list_devices))
        .route("/api/devices/{id}", get(get_device))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn w14_primary_election_demotes_existing() {
        let reg = DeviceRegistry::default();
        let owner = UserId::new();
        let first = Device::new(owner, "a", "macos", DeviceRole::Primary);
        let first_id = first.id;
        reg.register(first).await;
        let second = Device::new(owner, "b", "macos", DeviceRole::Primary);
        reg.register(second).await;
        let listed = reg.list_for_user(owner).await;
        let primaries: Vec<_> = listed
            .iter()
            .filter(|d| d.role == DeviceRole::Primary)
            .collect();
        assert_eq!(primaries.len(), 1);
        assert_ne!(primaries[0].id, first_id);
    }

    #[tokio::test]
    async fn w14_device_register_persists_in_registry() {
        let reg = DeviceRegistry::default();
        let owner = UserId::new();
        let device = Device::new(owner, "phone", "ios", DeviceRole::Mobile);
        let id = device.id.to_string();
        reg.register(device).await;
        assert!(reg.get(&id).await.is_some());
    }
}
