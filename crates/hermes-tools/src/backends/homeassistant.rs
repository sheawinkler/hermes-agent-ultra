//! Real Home Assistant backend: REST API calls.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::tools::homeassistant::HomeAssistantBackend;
use hermes_core::ToolError;

const BLOCKED_SERVICE_DOMAINS: &[&str] = &["shell_command", "python_script", "hassio"];

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

fn is_valid_ha_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn parse_entity_id(entity_id: &str) -> Result<(&str, &str), ToolError> {
    let trimmed = entity_id.trim();
    let (domain, object_id) = trimmed.split_once('.').ok_or_else(|| {
        ToolError::InvalidParams(format!(
            "Invalid entity_id '{}': expected format 'domain.object_id'",
            entity_id
        ))
    })?;
    if !is_valid_ha_component(domain) || !is_valid_ha_component(object_id) {
        return Err(ToolError::InvalidParams(format!(
            "Invalid entity_id '{}': only lowercase letters, digits, and '_' are allowed",
            entity_id
        )));
    }
    Ok((domain, object_id))
}

fn parse_service(domain_hint: &str, service: &str) -> Result<(String, String), ToolError> {
    let trimmed = service.trim();
    let (domain, action) = if let Some((d, s)) = trimmed.split_once('.') {
        (d, s)
    } else {
        (domain_hint, trimmed)
    };
    if !is_valid_ha_component(domain) || !is_valid_ha_component(action) {
        return Err(ToolError::InvalidParams(format!(
            "Invalid service '{}': use 'domain.service' with lowercase alnum/underscore",
            service
        )));
    }
    Ok((domain.to_string(), action.to_string()))
}

fn validate_service_domain(domain: &str) -> Result<(), ToolError> {
    if BLOCKED_SERVICE_DOMAINS.contains(&domain) {
        return Err(ToolError::InvalidParams(format!(
            "Service domain '{}' is blocked for safety",
            domain
        )));
    }
    Ok(())
}

#[async_trait]
impl HomeAssistantBackend for HaRestBackend {
    async fn list_entities(&self, domain: Option<&str>) -> Result<String, ToolError> {
        if let Some(d) = domain {
            if !is_valid_ha_component(d) {
                return Err(ToolError::InvalidParams(format!(
                    "Invalid domain '{}': use lowercase alnum/underscore",
                    d
                )));
            }
        }
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
        let _ = parse_entity_id(entity_id)?;
        let state: Value = self.get(&format!("/api/states/{}", entity_id)).await?;
        Ok(serde_json::to_string_pretty(&state)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize state: {}", e)))?)
    }

    async fn list_services(&self, domain: Option<&str>) -> Result<String, ToolError> {
        if let Some(d) = domain {
            if !is_valid_ha_component(d) {
                return Err(ToolError::InvalidParams(format!(
                    "Invalid domain '{}': use lowercase alnum/underscore",
                    d
                )));
            }
            validate_service_domain(d)?;
        }
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
        let (entity_domain, _) = parse_entity_id(entity_id)?;
        // Parse service as "domain.service" or just "service" with domain from entity_id.
        let (domain, svc) = parse_service(entity_domain, service)?;
        validate_service_domain(&domain)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_entity_id_accepts_valid_ids() {
        let (domain, object_id) = parse_entity_id("light.living_room").unwrap();
        assert_eq!(domain, "light");
        assert_eq!(object_id, "living_room");
    }

    #[test]
    fn parse_entity_id_rejects_invalid_ids() {
        assert!(parse_entity_id("light").is_err());
        assert!(parse_entity_id("Light.LivingRoom").is_err());
        assert!(parse_entity_id("../etc/passwd").is_err());
    }

    #[test]
    fn parse_service_uses_entity_domain_when_missing_prefix() {
        let (domain, action) = parse_service("light", "turn_on").unwrap();
        assert_eq!(domain, "light");
        assert_eq!(action, "turn_on");
    }

    #[test]
    fn parse_service_rejects_invalid_names() {
        assert!(parse_service("light", "light.turn-on").is_err());
        assert!(parse_service("light", "shell command.run").is_err());
    }

    #[test]
    fn validate_service_domain_blocks_sensitive_domains() {
        assert!(validate_service_domain("shell_command").is_err());
        assert!(validate_service_domain("python_script").is_err());
        assert!(validate_service_domain("light").is_ok());
    }
}
