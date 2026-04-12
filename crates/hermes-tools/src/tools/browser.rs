//! Browser automation tools
//!
//! All browser tools delegate to an injectable `BrowserBackend` trait,
//! allowing different browser implementations (Playwright, Chromium, etc.)
//! to be plugged in by the caller.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

// ---------------------------------------------------------------------------
// BrowserBackend trait
// ---------------------------------------------------------------------------

/// Injected backend for browser automation operations.
#[async_trait]
pub trait BrowserBackend: Send + Sync {
    async fn navigate(&self, url: &str) -> Result<String, ToolError>;
    async fn snapshot(&self) -> Result<String, ToolError>;
    async fn click(&self, selector: &str) -> Result<String, ToolError>;
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError>;
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError>;
    async fn go_back(&self) -> Result<String, ToolError>;
    async fn press(&self, key: &str) -> Result<String, ToolError>;
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError>;
    async fn vision(&self, instruction: &str) -> Result<String, ToolError>;
    async fn console(&self, action: &str) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// BrowserNavigateHandler
// ---------------------------------------------------------------------------

pub struct BrowserNavigateHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserNavigateHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserNavigateHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'url' parameter".into()))?;
        self.backend.navigate(url).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "url".into(),
            json!({
                "type": "string",
                "description": "The URL to navigate to"
            }),
        );
        tool_schema(
            "browser_navigate",
            "Navigate the browser to a URL.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserSnapshotHandler
// ---------------------------------------------------------------------------

pub struct BrowserSnapshotHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserSnapshotHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserSnapshotHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        self.backend.snapshot().await
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "browser_snapshot",
            "Take a snapshot of the current page state (accessibility tree).",
            JsonSchema::new("object"),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserClickHandler
// ---------------------------------------------------------------------------

pub struct BrowserClickHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserClickHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserClickHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let selector = params
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'selector' parameter".into()))?;
        self.backend.click(selector).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "selector".into(),
            json!({
                "type": "string",
                "description": "CSS selector or accessibility label to click"
            }),
        );
        tool_schema(
            "browser_click",
            "Click an element on the page.",
            JsonSchema::object(props, vec!["selector".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserTypeHandler
// ---------------------------------------------------------------------------

pub struct BrowserTypeHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserTypeHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserTypeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let selector = params
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'selector' parameter".into()))?;
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'text' parameter".into()))?;
        self.backend.r#type(selector, text).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "selector".into(),
            json!({
                "type": "string",
                "description": "CSS selector or accessibility label of the input field"
            }),
        );
        props.insert(
            "text".into(),
            json!({
                "type": "string",
                "description": "Text to type into the element"
            }),
        );
        tool_schema(
            "browser_type",
            "Type text into an element on the page.",
            JsonSchema::object(props, vec!["selector".into(), "text".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserScrollHandler
// ---------------------------------------------------------------------------

pub struct BrowserScrollHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserScrollHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserScrollHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let direction = params
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("down");
        let amount = params
            .get("amount")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        self.backend.scroll(direction, amount).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "direction".into(),
            json!({
                "type": "string",
                "description": "Scroll direction: up, down, left, right",
                "enum": ["up", "down", "left", "right"],
                "default": "down"
            }),
        );
        props.insert(
            "amount".into(),
            json!({
                "type": "integer",
                "description": "Number of pixels to scroll (default: 500)"
            }),
        );
        tool_schema(
            "browser_scroll",
            "Scroll the page in a direction.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserBackHandler
// ---------------------------------------------------------------------------

pub struct BrowserBackHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserBackHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserBackHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        self.backend.go_back().await
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "browser_back",
            "Navigate back in browser history.",
            JsonSchema::new("object"),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserPressHandler
// ---------------------------------------------------------------------------

pub struct BrowserPressHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserPressHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserPressHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'key' parameter".into()))?;
        self.backend.press(key).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "key".into(),
            json!({
                "type": "string",
                "description": "Key to press (e.g. 'Enter', 'Tab', 'Escape', 'ArrowDown')"
            }),
        );
        tool_schema(
            "browser_press",
            "Press a keyboard key.",
            JsonSchema::object(props, vec!["key".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserGetImagesHandler
// ---------------------------------------------------------------------------

pub struct BrowserGetImagesHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserGetImagesHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserGetImagesHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let selector = params.get("selector").and_then(|v| v.as_str());
        self.backend.get_images(selector).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "selector".into(),
            json!({
                "type": "string",
                "description": "Optional CSS selector to filter images"
            }),
        );
        tool_schema(
            "browser_get_images",
            "Get images from the current page.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserVisionHandler
// ---------------------------------------------------------------------------

pub struct BrowserVisionHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserVisionHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserVisionHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let instruction = params
            .get("instruction")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'instruction' parameter".into()))?;
        self.backend.vision(instruction).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "instruction".into(),
            json!({
                "type": "string",
                "description": "What to look for or analyze in the current page screenshot"
            }),
        );
        tool_schema(
            "browser_vision",
            "Use vision to analyze the current browser page.",
            JsonSchema::object(props, vec!["instruction".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserConsoleHandler
// ---------------------------------------------------------------------------

pub struct BrowserConsoleHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserConsoleHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserConsoleHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("read");
        self.backend.console(action).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("action".into(), json!({
            "type": "string",
            "description": "Console action: 'read' to get console output, 'clear' to clear console",
            "enum": ["read", "clear"],
            "default": "read"
        }));
        tool_schema(
            "browser_console",
            "Read or clear the browser console.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBrowserBackend;
    #[async_trait]
    impl BrowserBackend for MockBrowserBackend {
        async fn navigate(&self, url: &str) -> Result<String, ToolError> {
            Ok(format!("Navigated to {}", url))
        }
        async fn snapshot(&self) -> Result<String, ToolError> {
            Ok("Page snapshot".into())
        }
        async fn click(&self, sel: &str) -> Result<String, ToolError> {
            Ok(format!("Clicked {}", sel))
        }
        async fn r#type(&self, sel: &str, text: &str) -> Result<String, ToolError> {
            Ok(format!("Typed '{}' into {}", text, sel))
        }
        async fn scroll(&self, dir: &str, _amt: Option<u32>) -> Result<String, ToolError> {
            Ok(format!("Scrolled {}", dir))
        }
        async fn go_back(&self) -> Result<String, ToolError> {
            Ok("Went back".into())
        }
        async fn press(&self, key: &str) -> Result<String, ToolError> {
            Ok(format!("Pressed {}", key))
        }
        async fn get_images(&self, sel: Option<&str>) -> Result<String, ToolError> {
            Ok(format!("Images: {:?}", sel))
        }
        async fn vision(&self, inst: &str) -> Result<String, ToolError> {
            Ok(format!("Vision: {}", inst))
        }
        async fn console(&self, action: &str) -> Result<String, ToolError> {
            Ok(format!("Console: {}", action))
        }
    }

    fn backend() -> std::sync::Arc<dyn BrowserBackend> {
        std::sync::Arc::new(MockBrowserBackend)
    }

    #[tokio::test]
    async fn test_browser_navigate() {
        let handler = BrowserNavigateHandler::new(backend());
        let result = handler
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(result.contains("example.com"));
    }

    #[tokio::test]
    async fn test_browser_click() {
        let handler = BrowserClickHandler::new(backend());
        let result = handler.execute(json!({"selector": "#btn"})).await.unwrap();
        assert!(result.contains("#btn"));
    }

    #[tokio::test]
    async fn test_browser_snapshot() {
        let handler = BrowserSnapshotHandler::new(backend());
        let result = handler.execute(json!({})).await.unwrap();
        assert!(result.contains("snapshot"));
    }
}
