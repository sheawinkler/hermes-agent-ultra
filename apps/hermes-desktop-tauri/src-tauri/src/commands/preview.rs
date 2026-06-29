use super::*;

// Preview (placeholder)
// ============================================================================

#[tauri::command]
pub async fn normalize_preview_target(
    target: String,
    base_dir: Option<String>,
) -> Result<serde_json::Value, String> {
    let normalized = normalize_preview_target_impl(&target, base_dir.as_deref().unwrap_or(""));
    Ok(match normalized {
        Some(value) => serde_json::to_value(value)
            .map_err(|e| format!("Failed to serialize preview target: {}", e))?,
        None => serde_json::Value::Null,
    })
}

#[tauri::command]
pub async fn watch_preview_file(
    url: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PreviewWatch, String> {
    watch_preview_file_impl(url, app, state).await
}

#[tauri::command]
pub async fn stop_preview_file_watch(
    id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    Ok(stop_preview_file_watch_impl(id, state).await)
}
