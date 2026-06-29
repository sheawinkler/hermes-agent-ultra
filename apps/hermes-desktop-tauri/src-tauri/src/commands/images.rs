use super::*;

// Image Operations (placeholder)
// ============================================================================

#[tauri::command]
pub async fn save_image_from_url(
    url: String,
    suggested_name: Option<String>,
) -> Result<serde_json::Value, String> {
    let (buffer, fallback_name) = resource_buffer_from_url(&url).await?;
    let fallback = preferred_image_save_name(suggested_name.as_deref(), fallback_name.as_deref());
    let picked = rfd::FileDialog::new()
        .set_title("Save Image")
        .set_file_name(&fallback)
        .save_file();

    let Some(file_path) = picked else {
        return Ok(serde_json::json!(false));
    };

    fs::write(&file_path, buffer).map_err(|e| format!("Failed to save image: {}", e))?;
    Ok(serde_json::json!(true))
}

#[tauri::command]
pub async fn save_image_buffer(data: Vec<u8>, ext: String) -> Result<serde_json::Value, String> {
    let file_path = write_composer_image(&data, &ext)?;
    Ok(serde_json::json!(file_path.to_string_lossy()))
}

#[tauri::command]
pub async fn save_clipboard_image() -> Result<serde_json::Value, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("Failed to access clipboard: {}", e))?;
    let Ok(image) = clipboard.get_image() else {
        return Ok(serde_json::json!(""));
    };
    let path = write_png_from_rgba(
        image.bytes.into_owned(),
        image.width as u32,
        image.height as u32,
    )?;
    Ok(serde_json::json!(path.to_string_lossy()))
}
