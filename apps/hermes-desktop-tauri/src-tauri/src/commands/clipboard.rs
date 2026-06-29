use super::*;

// ============================================================================
// Clipboard
// ============================================================================

#[tauri::command]
pub async fn write_clipboard(text: String) -> Result<(), String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("Failed to access clipboard: {}", e))?;
    clipboard
        .set_text(text)
        .map_err(|e| format!("Failed to write clipboard: {}", e))
}
