use indexmap::IndexMap;
use serde_json::json;

use hermes_core::{JsonSchema, ToolSchema, tool_schema};

pub fn computer_use_schema() -> ToolSchema {
    let mut props = IndexMap::new();
    props.insert(
        "action".into(),
        json!({"type":"string","enum":["capture","capture_to_file","click","double_click","right_click","middle_click","drag","scroll","type","key","set_value","wait","list_apps","focus_app"]}),
    );
    props.insert(
        "mode".into(),
        json!({"type":"string","enum":["som","vision","ax"]}),
    );
    props.insert("app".into(), json!({"type":"string"}));
    props.insert("element".into(), json!({"type":"integer"}));
    props.insert(
        "coordinate".into(),
        json!({"type":"array","items":{"type":"integer"},"minItems":2,"maxItems":2}),
    );
    props.insert(
        "button".into(),
        json!({"type":"string","enum":["left","right","middle"]}),
    );
    props.insert(
        "modifiers".into(),
        json!({"type":"array","items":{"type":"string","enum":["cmd","shift","option","alt","ctrl","fn"]}}),
    );
    props.insert(
        "direction".into(),
        json!({"type":"string","enum":["up","down","left","right"]}),
    );
    props.insert("amount".into(), json!({"type":"integer"}));
    props.insert("value".into(), json!({"type":"string"}));
    props.insert("text".into(), json!({"type":"string"}));
    props.insert("keys".into(), json!({"type":"string"}));
    props.insert("seconds".into(), json!({"type":"number"}));
    props.insert("raise_window".into(), json!({"type":"boolean"}));
    props.insert("capture_after".into(), json!({"type":"boolean"}));
    tool_schema(
        "computer_use",
        "Drive desktop in background. Preferred workflow: capture(mode='som') then click by element index. Use capture_to_file when you need to send screenshots as files via messaging tools.",
        JsonSchema::object(props, vec!["action".into()]),
    )
}
