//! Home Assistant tools: list entities, get state, list services, call service

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// HomeAssistantBackend trait
// ---------------------------------------------------------------------------

/// Backend for Home Assistant integration.
#[async_trait]
pub trait HomeAssistantBackend: Send + Sync {
    /// List all entities.
    async fn list_entities(&self, domain: Option<&str>) -> Result<String, ToolError>;
    /// Get the state of an entity.
    async fn get_state(&self, entity_id: &str) -> Result<String, ToolError>;
    /// List available services.
    async fn list_services(&self, domain: Option<&str>) -> Result<String, ToolError>;
    /// Call a service.
    async fn call_service(
        &self,
        service: &str,
        entity_id: &str,
        data: Option<&Value>,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// HaListEntitiesHandler
// ---------------------------------------------------------------------------

pub struct HaListEntitiesHandler {
    backend: Arc<dyn HomeAssistantBackend>,
}

impl HaListEntitiesHandler {
    pub fn new(backend: Arc<dyn HomeAssistantBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for HaListEntitiesHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let domain = params.get("domain").and_then(|v| v.as_str());
        self.backend.list_entities(domain).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "domain".into(),
            json!({
                "type": "string",
                "description": "Optional domain filter (e.g. 'light', 'switch', 'sensor')"
            }),
        );

        tool_schema(
            "ha_list_entities",
            "List Home Assistant entities, optionally filtered by domain.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// HaGetStateHandler
// ---------------------------------------------------------------------------

pub struct HaGetStateHandler {
    backend: Arc<dyn HomeAssistantBackend>,
}

impl HaGetStateHandler {
    pub fn new(backend: Arc<dyn HomeAssistantBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for HaGetStateHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let entity_id = params
            .get("entity_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'entity_id' parameter".into()))?;
        self.backend.get_state(entity_id).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "entity_id".into(),
            json!({
                "type": "string",
                "description": "Entity ID to get state for (e.g. 'light.living_room')"
            }),
        );

        tool_schema(
            "ha_get_state",
            "Get the current state of a Home Assistant entity.",
            JsonSchema::object(props, vec!["entity_id".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// HaListServicesHandler
// ---------------------------------------------------------------------------

pub struct HaListServicesHandler {
    backend: Arc<dyn HomeAssistantBackend>,
}

impl HaListServicesHandler {
    pub fn new(backend: Arc<dyn HomeAssistantBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for HaListServicesHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let domain = params.get("domain").and_then(|v| v.as_str());
        self.backend.list_services(domain).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "domain".into(),
            json!({
                "type": "string",
                "description": "Optional domain filter (e.g. 'light', 'switch')"
            }),
        );

        tool_schema(
            "ha_list_services",
            "List available Home Assistant services, optionally filtered by domain.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// HaCallServiceHandler
// ---------------------------------------------------------------------------

pub struct HaCallServiceHandler {
    backend: Arc<dyn HomeAssistantBackend>,
}

impl HaCallServiceHandler {
    pub fn new(backend: Arc<dyn HomeAssistantBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for HaCallServiceHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let service = params
            .get("service")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'service' parameter".into()))?;

        let entity_id = params
            .get("entity_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'entity_id' parameter".into()))?;

        let data = params.get("data");

        match data {
            Some(d) => self.backend.call_service(service, entity_id, Some(d)).await,
            None => self.backend.call_service(service, entity_id, None).await,
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "service".into(),
            json!({
                "type": "string",
                "description": "Service to call (e.g. 'turn_on', 'turn_off', 'toggle')"
            }),
        );
        props.insert(
            "entity_id".into(),
            json!({
                "type": "string",
                "description": "Entity ID to call the service on (e.g. 'light.living_room')"
            }),
        );
        props.insert(
            "data".into(),
            json!({
                "type": "object",
                "description": "Additional service data (e.g. brightness, color)"
            }),
        );

        tool_schema(
            "ha_call_service",
            "Call a Home Assistant service on an entity.",
            JsonSchema::object(props, vec!["service".into(), "entity_id".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockHABackend;
    #[async_trait]
    impl HomeAssistantBackend for MockHABackend {
        async fn list_entities(&self, domain: Option<&str>) -> Result<String, ToolError> {
            Ok(format!("Entities (domain: {:?})", domain))
        }
        async fn get_state(&self, entity_id: &str) -> Result<String, ToolError> {
            Ok(format!("State of {}: on", entity_id))
        }
        async fn list_services(&self, domain: Option<&str>) -> Result<String, ToolError> {
            Ok(format!("Services (domain: {:?})", domain))
        }
        async fn call_service(
            &self,
            service: &str,
            entity_id: &str,
            _data: Option<&Value>,
        ) -> Result<String, ToolError> {
            Ok(format!("Called {} on {}", service, entity_id))
        }
    }

    fn backend() -> Arc<dyn HomeAssistantBackend> {
        Arc::new(MockHABackend)
    }

    #[tokio::test]
    async fn test_ha_list_entities() {
        let handler = HaListEntitiesHandler::new(backend());
        let result = handler.execute(json!({"domain": "light"})).await.unwrap();
        assert!(result.contains("light"));
    }

    #[tokio::test]
    async fn test_ha_get_state() {
        let handler = HaGetStateHandler::new(backend());
        let result = handler
            .execute(json!({"entity_id": "light.living_room"}))
            .await
            .unwrap();
        assert!(result.contains("light.living_room"));
    }

    #[tokio::test]
    async fn test_ha_call_service() {
        let handler = HaCallServiceHandler::new(backend());
        let result = handler
            .execute(json!({"service": "turn_on", "entity_id": "light.living_room"}))
            .await
            .unwrap();
        assert!(result.contains("turn_on"));
    }

    #[tokio::test]
    async fn test_ha_schemas() {
        assert_eq!(
            HaListEntitiesHandler::new(backend()).schema().name,
            "ha_list_entities"
        );
        assert_eq!(
            HaGetStateHandler::new(backend()).schema().name,
            "ha_get_state"
        );
        assert_eq!(
            HaListServicesHandler::new(backend()).schema().name,
            "ha_list_services"
        );
        assert_eq!(
            HaCallServiceHandler::new(backend()).schema().name,
            "ha_call_service"
        );
    }
}
