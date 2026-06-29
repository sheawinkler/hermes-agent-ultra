use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct PushRegisterResult {
    pub ok: bool,
    pub platform: String,
}

#[tauri::command]
pub async fn register_push_token(
    _device_id: String,
    _token: String,
    platform: String,
    _manufacturer: Option<String>,
) -> Result<PushRegisterResult, String> {
    Ok(PushRegisterResult {
        ok: true,
        platform,
    })
}

#[tauri::command]
pub async fn unregister_push_token(_device_id: String) -> Result<(), String> {
    Ok(())
}
