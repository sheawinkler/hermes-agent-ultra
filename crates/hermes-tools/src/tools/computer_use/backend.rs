use async_trait::async_trait;
use serde_json::{Value, json};

use hermes_core::ToolError;

#[derive(Debug, Clone)]
pub struct UiElement {
    pub index: i64,
    pub role: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub mode: String,
    pub image_b64: Option<String>,
    pub image_mime: Option<String>,
    pub app: String,
    pub window_title: String,
    pub elements: Vec<UiElement>,
}

#[derive(Debug, Clone)]
pub struct ActionResult {
    pub ok: bool,
    pub action: String,
    pub message: String,
    pub meta: Value,
}

#[async_trait]
pub trait ComputerUseBackend: Send + Sync {
    async fn capture(&self, mode: &str, app: Option<&str>) -> Result<CaptureResult, ToolError>;
    async fn click(
        &self,
        element: Option<i64>,
        coordinate: Option<(i64, i64)>,
        button: &str,
        click_count: i64,
        modifiers: &[String],
    ) -> Result<ActionResult, ToolError>;
    async fn scroll(
        &self,
        direction: &str,
        amount: i64,
        element: Option<i64>,
        coordinate: Option<(i64, i64)>,
        modifiers: &[String],
    ) -> Result<ActionResult, ToolError>;
    async fn type_text(&self, text: &str) -> Result<ActionResult, ToolError>;
    async fn key(&self, keys: &str) -> Result<ActionResult, ToolError>;
    async fn set_value(&self, value: &str, element: Option<i64>)
    -> Result<ActionResult, ToolError>;
    async fn wait(&self, seconds: f64) -> Result<ActionResult, ToolError>;
    async fn list_apps(&self) -> Result<Value, ToolError>;
    async fn focus_app(&self, app: &str, raise_window: bool) -> Result<ActionResult, ToolError>;
}

pub fn unsupported_action(action: &str) -> ActionResult {
    ActionResult {
        ok: false,
        action: action.to_string(),
        message: "action not supported by fallback backend; install cua-driver-rs/cua-driver for full computer_use actions".to_string(),
        meta: json!({}),
    }
}
