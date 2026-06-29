use serde_json::json;

pub fn computer_use_browser(params: serde_json::Value) -> Result<String, String> {
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("screenshot");
    Ok(json!({
        "action": action,
        "ok": false,
        "note": "browser driver CDP connection pending — use hermes-browser-driver when configured"
    })
    .to_string())
}
