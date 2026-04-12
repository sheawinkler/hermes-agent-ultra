//! Skills management tools: list, view, and manage skills

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, Skill, SkillMeta, SkillProvider, ToolError, ToolHandler, ToolSchema, tool_schema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// SkillsListHandler
// ---------------------------------------------------------------------------

/// Tool for listing all available skills.
pub struct SkillsListHandler {
    provider: Arc<dyn SkillProvider>,
}

impl SkillsListHandler {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl ToolHandler for SkillsListHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let skills = self.provider.list_skills()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let result: Vec<Value> = skills.iter().map(|s| {
            json!({
                "name": s.name,
                "category": s.category,
                "description": s.description,
            })
        }).collect();

        Ok(serde_json::to_string_pretty(&result)
            .unwrap_or_else(|_| "[]".to_string()))
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "skills_list",
            "List all available skills with their metadata.",
            JsonSchema::new("object"),
        )
    }
}

// ---------------------------------------------------------------------------
// SkillViewHandler
// ---------------------------------------------------------------------------

/// Tool for viewing a specific skill's content.
pub struct SkillViewHandler {
    provider: Arc<dyn SkillProvider>,
}

impl SkillViewHandler {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl ToolHandler for SkillViewHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params.get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;

        match self.provider.get_skill(name).await {
            Ok(Some(skill)) => Ok(skill.content.clone()),
            Ok(None) => Err(ToolError::NotFound(format!("Skill '{}' not found", name))),
            Err(e) => Err(ToolError::ExecutionFailed(e.to_string())),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("name".into(), json!({
            "type": "string",
            "description": "Name of the skill to view"
        }));

        tool_schema(
            "skill_view",
            "View the full content of a skill by name.",
            JsonSchema::object(props, vec!["name".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// SkillManageHandler
// ---------------------------------------------------------------------------

/// Tool for creating, updating, and deleting skills.
pub struct SkillManageHandler {
    provider: Arc<dyn SkillProvider>,
}

impl SkillManageHandler {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl ToolHandler for SkillManageHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params.get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "create" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let content = params.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'content' parameter".into()))?;
                let category = params.get("category").and_then(|v| v.as_str());

                self.provider.create_skill(name, content, category)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' created successfully", name))
            }
            "update" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let content = params.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'content' parameter".into()))?;

                self.provider.update_skill(name, content)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' updated successfully", name))
            }
            "delete" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;

                self.provider.delete_skill(name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' deleted successfully", name))
            }
            other => Err(ToolError::InvalidParams(format!("Unknown action: '{}'. Use 'create', 'update', or 'delete'.", other))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("action".into(), json!({
            "type": "string",
            "description": "Action to perform: create, update, or delete",
            "enum": ["create", "update", "delete"]
        }));
        props.insert("name".into(), json!({
            "type": "string",
            "description": "Name of the skill"
        }));
        props.insert("content".into(), json!({
            "type": "string",
            "description": "Skill content (for create/update)"
        }));
        props.insert("category".into(), json!({
            "type": "string",
            "description": "Skill category (for create)"
        }));

        tool_schema(
            "skill_manage",
            "Create, update, or delete skills.",
            JsonSchema::object(props, vec!["action".into(), "name".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::AgentError;

    struct MockSkillProvider;
    #[async_trait]
    impl SkillProvider for MockSkillProvider {
        async fn create_skill(&self, name: &str, content: &str, category: Option<&str>) -> Result<Skill, AgentError> {
            Ok(Skill { name: name.into(), content: content.into(), category: category.map(String::from), description: None })
        }
        async fn get_skill(&self, name: &str) -> Result<Option<Skill>, AgentError> {
            Ok(Some(Skill { name: name.into(), content: "skill content".into(), category: None, description: None }))
        }
        async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError> {
            Ok(vec![SkillMeta { name: "test".into(), category: None, description: None }])
        }
        async fn update_skill(&self, name: &str, content: &str) -> Result<Skill, AgentError> {
            Ok(Skill { name: name.into(), content: content.into(), category: None, description: None })
        }
        async fn delete_skill(&self, _name: &str) -> Result<(), AgentError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_skills_list() {
        let handler = SkillsListHandler::new(Arc::new(MockSkillProvider));
        let result = handler.execute(json!({})).await.unwrap();
        assert!(result.contains("test"));
    }

    #[tokio::test]
    async fn test_skill_view() {
        let handler = SkillViewHandler::new(Arc::new(MockSkillProvider));
        let result = handler.execute(json!({"name": "test"})).await.unwrap();
        assert_eq!(result, "skill content");
    }

    #[tokio::test]
    async fn test_skill_manage_create() {
        let handler = SkillManageHandler::new(Arc::new(MockSkillProvider));
        let result = handler.execute(json!({"action": "create", "name": "new_skill", "content": "hello"})).await.unwrap();
        assert!(result.contains("created"));
    }
}