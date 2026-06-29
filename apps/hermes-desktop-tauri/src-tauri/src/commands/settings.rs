use super::*;

// Settings
// ============================================================================

#[tauri::command]
pub async fn get_default_project_dir() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!(default_project_dir_state()))
}

#[tauri::command]
pub async fn set_default_project_dir(dir: Option<String>) -> Result<serde_json::Value, String> {
    let next = dir
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    if let Some(path) = next.as_ref() {
        fs::create_dir_all(path).map_err(|e| format!("Could not create directory: {}", e))?;
    }

    write_default_project_dir(next.as_deref())?;
    Ok(serde_json::json!({ "dir": next }))
}

#[tauri::command]
pub async fn pick_default_project_dir() -> Result<PickDefaultProjectDirResult, String> {
    let default_dir = read_default_project_dir().or_else(|| dirs::home_dir());
    let picked = rfd::FileDialog::new()
        .set_title("Choose default project directory")
        .set_directory(default_dir.unwrap_or_else(|| PathBuf::from(".")))
        .pick_folder();

    Ok(match picked {
        Some(path) => PickDefaultProjectDirResult {
            canceled: false,
            dir: Some(path.to_string_lossy().to_string()),
        },
        None => PickDefaultProjectDirResult {
            canceled: true,
            dir: None,
        },
    })
}

#[tauri::command]
pub async fn get_ui_preferences() -> Result<UiPreferences, String> {
    Ok(read_ui_preferences_from_disk())
}

#[tauri::command]
pub async fn set_ui_language(language: Option<String>) -> Result<UiPreferences, String> {
    let mut preferences = read_ui_preferences_from_disk();
    preferences.language = normalize_ui_language(language);
    write_ui_preferences_to_disk(&preferences)
}
