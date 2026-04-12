//! Real Home Assistant backend: REST API calls.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::tools::homeassistant::HomeAssistantBackend;
use hermes_core::ToolError;

/// Home Assistant backend using the REST API.
pub struct HaRestBackend {
    client: Client,
    base_url: String,
    token: String,
}

impl HaRestBackend {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let base_url = std::env::var("HASS_URL").map_err(|_| {
            ToolError::ExecutionFailed("HASS_URL environment variable not set".into())
        })?;
        let token = std::env::var("HASS_TOKEN").map_err(|_| {
            ToolError::ExecutionFailed("HASS_TOKEN environment variable not set".into())
        })?;
        Ok(Self::new(base_url, token))
    }

    async fn get(&self, path: &str) -> Result<Value, ToolError> {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("HA API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read HA response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "HA API error ({}): {}",
                status, text
            )));
        }

        serde_json::from_str(&text)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to parse HA response: {}", e)))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, ToolError> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.token))
            .json(body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("HA API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read HA response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "HA API error ({}): {}",
                status, text
            )));
        }

        serde_json::from_str(&text)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to parse HA response: {}", e)))
    }
}

#[async_trait]
impl HomeAssistantBackend for HaRestBackend {
    async fn list_entities(&self, domain: Option<&str>) -> Result<String, ToolError> {
        let states: Value = self.get("/api/states").await?;
        let empty = vec![];
        let entities = states.as_array().unwrap_or(&empty);

        let filtered: Vec<Value> = entities
            .iter()
            .filter(|e| {
                if let Some(d) = domain {
                    e.get("entity_id")
                        .and_then(|id| id.as_str())
                        .map(|id| id.starts_with(&format!("{}.", d)))
                        .unwrap_or(false)
                } else {
                    true
                }
            })
            .map(|e| {
                json!({
                    "entity_id": e.get("entity_id").and_then(|v| v.as_str()).unwrap_or(""),
                    "state": e.get("state").and_then(|v| v.as_str()).unwrap_or(""),
                    "friendly_name": e.get("attributes")
                        .and_then(|a| a.get("friendly_name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or(""),
                })
            })
            .collect();

        Ok(json!({"entities": filtered, "total": filtered.len()}).to_string())
    }

    async fn get_state(&self, entity_id: &str) -> Result<String, ToolError> {
        let state: Value = self.get(&format!("/api/states/{}", entity_id)).await?;
        Ok(serde_json::to_string_pretty(&state)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize state: {}", e)))?)
    }

    async fn list_services(&self, domain: Option<&str>) -> Result<String, ToolError> {
        let services: Value = self.get("/api/services").await?;
        let empty = vec![];
        let all = services.as_array().unwrap_or(&empty);

        let filtered: Vec<&Value> = match domain {
            Some(d) => all
                .iter()
                .filter(|s| s.get("domain").and_then(|v| v.as_str()) == Some(d))
                .collect(),
            None => all.iter().collect(),
        };

        Ok(json!({"services": filtered, "total": filtered.len()}).to_string())
    }

    async fn call_service(
        &self,
        service: &str,
        entity_id: &str,
        data: Option<&Value>,
    ) -> Result<String, ToolError> {
        // Parse service as "domain.service" or just "service" with domain from entity_id
        let (domain, svc) = if let Some(dot_pos) = service.find('.') {
            (&service[..dot_pos], &service[dot_pos + 1..])
        } else {
            let domain = entity_id.split('.').next().unwrap_or("homeassistant");
            (domain, service)
        };

        let mut body = data.cloned().unwrap_or(json!({}));
        if let Some(obj) = body.as_object_mut() {
            obj.insert("entity_id".to_string(), json!(entity_id));
        }

        let result = self
            .post(&format!("/api/services/{}/{}", domain, svc), &body)
            .await?;
        Ok(json!({"status": "ok", "result": result}).to_string())
    }
}
