use crate::hermes_backend;
use crate::hermes_ws_bridge::{HermesWsBridge, StreamId, StreamRouter};
use base64::Engine as _;
#[cfg(target_os = "macos")]
use block2::RcBlock;
use futures_util::StreamExt;
use notify::{RecursiveMode, Watcher, recommended_watcher};
#[cfg(target_os = "macos")]
use objc2::runtime::Bool as ObjcBool;
#[cfg(target_os = "macos")]
use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};
use portable_pty::{Child, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State, Window};
use tokio::sync::Mutex;

const DEFAULT_UPDATE_BRANCH: &str = "main";
const DESKTOP_UPDATE_CONFIG_PATH: &str = "updates.json";
const UPDATE_SOURCES_CONFIG_PATH: &str = "update-sources.json";
const UI_PREFERENCES_CONFIG_PATH: &str = "ui-preferences.json";
const SYSTEM_HERMES_SKIP_UPSTREAM_PROMPT_FILE: &str = ".skip_upstream_prompt";
const UPDATE_PROGRESS_EVENT: &str = "hermes:updates:progress";
const OPEN_UPDATES_EVENT: &str = "hermes:open-updates";
const CLOSE_PREVIEW_EVENT: &str = "hermes:close-preview-requested";
const WINDOW_STATE_EVENT: &str = "hermes:window-state-changed";
const BOOTSTRAP_EVENT: &str = "hermes:bootstrap:event";
const CONTEXT_SPELLING_SUGGESTION_PREFIX: &str = "context-spelling-suggestion-";
const DESKTOP_DOCS_URL: &str =
    "https://hermes-agent.nousresearch.com/docs/getting-started/installation";
const DESKTOP_RELEASES_URL: &str = "https://github.com/meespace/hermes-desktop-tauri/releases";
#[allow(dead_code)]
const WINDOWS_CREATE_NO_WINDOW: u32 = 0x08000000;
const BOOTSTRAP_LOG_RING_MAX: usize = 500;
const DEFAULT_FETCH_TIMEOUT_MS: u64 = 15_000;
const PREVIEW_WATCH_DEBOUNCE_MS: u64 = 120;
const DATA_URL_READ_MAX_BYTES: u64 = 16 * 1024 * 1024;
const TEXT_PREVIEW_SOURCE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const TEXT_PREVIEW_MAX_BYTES: u64 = 512 * 1024;
const LINK_TITLE_BYTE_BUDGET: usize = 96 * 1024;
const LINK_TITLE_TIMEOUT_MS: u64 = 5_000;
const LINK_TITLE_RENDER_TIMEOUT_MS: u64 = 8_000;
const LINK_TITLE_RENDER_GRACE_MS: u64 = 700;
const LINK_TITLE_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_6_0) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";
const AT_COOKIE_VARIANTS: &[&str] = &[
    "__Host-hermes_session_at",
    "__Secure-hermes_session_at",
    "hermes_session_at",
];
const RT_COOKIE_VARIANTS: &[&str] = &[
    "__Host-hermes_session_rt",
    "__Secure-hermes_session_rt",
    "hermes_session_rt",
];
const DOCK_PINNED_MARKER: &str = "dock-pinned.json";
const SAFE_ENV_SUFFIXES: &[&str] = &["dist", "example", "sample", "template"];
const SENSITIVE_EXTENSIONS: &[&str] = &[".kdbx", ".p12", ".pem", ".pfx"];
const FS_READDIR_HIDDEN: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".cache",
    ".next",
    ".turbo",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
    "target",
    "venv",
];
const DEFAULT_AGENT_GIT_URL: &str = "https://github.com/NousResearch/hermes-agent.git";
const GITEE_AGENT_GIT_URL: &str = "https://gitee.com/8187735/hermes-agent.git";
const GITCODE_AGENT_GIT_URL: &str = "https://gitcode.com/macaque_zhang/hermes-agent.git";
const DEFAULT_AGENT_GIT_BRANCH: &str = "main";
const DEFAULT_PYTHON_INDEX_URL: &str = "https://pypi.org/simple";
const ALIYUN_PYTHON_INDEX_URL: &str = "https://mirrors.aliyun.com/pypi/simple/";
const DEFAULT_NPM_REGISTRY_URL: &str = "https://registry.npmjs.org/";
const NPMMIRROR_REGISTRY_URL: &str = "https://registry.npmmirror.com/";
const LEGACY_DESKTOP_REPO_URL: &str = "https://github.com/zhangxingyu/hermes-desktop-tauri";
const DEFAULT_DESKTOP_REPO_URL: &str = "https://github.com/meespace/hermes-desktop-tauri";

// Keep the renderer-side fallback in sync with tauri.conf.json's
// `trafficLightPosition` so macOS native traffic lights and HTML titlebar
// controls line up as soon as the window boots.
#[cfg(target_os = "macos")]
const MACOS_WINDOW_BUTTON_POSITION: WindowButtonPosition = WindowButtonPosition { x: 14, y: 18 };
#[cfg(not(target_os = "macos"))]
const MACOS_WINDOW_BUTTON_POSITION: WindowButtonPosition = WindowButtonPosition { x: 14, y: 18 };
const NATIVE_OVERLAY_BUTTON_WIDTH: i32 = 144;

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ApiRequest {
    pub path: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default, rename = "timeoutMs")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LocalChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LocalModelChatCompletionRequest {
    pub api: String,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    pub model: String,
    pub messages: Vec<LocalChatMessage>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LocalModelChatCompletionResponse {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct LocalModelChatStreamDelta {
    pub delta: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConnectionConfig {
    pub mode: String,
    #[serde(default)]
    pub remote: Option<RemoteConfig>,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileRemoteConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DesktopConnectionConfigState {
    #[serde(rename = "envOverride")]
    pub env_override: bool,
    pub mode: String,
    pub profile: Option<String>,
    #[serde(rename = "remoteAuthMode")]
    pub remote_auth_mode: String,
    #[serde(rename = "remoteOauthConnected")]
    pub remote_oauth_connected: bool,
    #[serde(rename = "remoteTokenPreview")]
    pub remote_token_preview: Option<String>,
    #[serde(rename = "remoteTokenSet")]
    pub remote_token_set: bool,
    #[serde(rename = "remoteUrl")]
    pub remote_url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct RemoteConfig {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub token: Option<TokenValue>,
    #[serde(default, rename = "authMode")]
    pub auth_mode: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ProfileRemoteConfig {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub token: Option<TokenValue>,
    #[serde(default, rename = "authMode")]
    pub auth_mode: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TokenValue {
    pub value: String,
    #[serde(default)]
    pub encoding: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConnection {
    pub base_url: String,
    pub token: String,
    pub ws_url: String,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub logs: Vec<String>,
    #[serde(default)]
    pub is_fullscreen: bool,
    #[serde(default)]
    pub native_overlay_width: i32,
    #[serde(default)]
    pub window_button_position: Option<WindowButtonPosition>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DesktopAuthProvider {
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub supports_password: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DesktopConnectionProbeResult {
    pub auth_mode: String,
    pub base_url: String,
    pub error: Option<String>,
    pub providers: Vec<DesktopAuthProvider>,
    pub reachable: bool,
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DesktopOauthLoginResult {
    pub base_url: String,
    pub connected: bool,
    pub ok: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DesktopOauthLogoutResult {
    pub connected: bool,
    pub ok: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BootProgress {
    pub phase: String,
    pub message: String,
    pub progress: u32,
    pub running: bool,
    pub error: Option<String>,
    #[serde(rename = "fakeMode")]
    pub fake_mode: bool,
    pub timestamp: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DefaultProjectDirState {
    #[serde(rename = "defaultLabel")]
    pub default_label: String,
    pub dir: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct UiPreferences {
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PickDefaultProjectDirResult {
    pub canceled: bool,
    pub dir: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct PreviewWatchPayload {
    id: String,
    path: String,
    url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BootstrapStageResult {
    pub state: String,
    #[serde(rename = "durationMs")]
    pub duration_ms: Option<u64>,
    #[serde(rename = "startedAt")]
    pub started_at: Option<u64>,
    pub json: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BootstrapState {
    pub active: bool,
    pub manifest: Option<serde_json::Value>,
    pub stages: HashMap<String, BootstrapStageResult>,
    pub error: Option<String>,
    pub log: Vec<serde_json::Value>,
    #[serde(rename = "startedAt")]
    pub started_at: Option<u64>,
    #[serde(rename = "completedAt")]
    pub completed_at: Option<u64>,
    #[serde(rename = "unsupportedPlatform")]
    pub unsupported_platform: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WindowButtonPosition {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PreviewWatch {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WindowStatePayload {
    #[serde(rename = "isFullscreen")]
    pub is_fullscreen: bool,
    #[serde(rename = "nativeOverlayWidth")]
    pub native_overlay_width: i32,
    #[serde(rename = "windowButtonPosition")]
    pub window_button_position: Option<WindowButtonPosition>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ContextMenuEditFlags {
    #[serde(rename = "canCut", default)]
    pub can_cut: bool,
    #[serde(rename = "canCopy", default)]
    pub can_copy: bool,
    #[serde(rename = "canPaste", default)]
    pub can_paste: bool,
    #[serde(rename = "canSelectAll", default)]
    pub can_select_all: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ContextMenuRequest {
    #[serde(rename = "selectionText", default)]
    pub selection_text: String,
    #[serde(rename = "isEditable", default)]
    pub is_editable: bool,
    #[serde(rename = "linkUrl", default)]
    pub link_url: Option<String>,
    #[serde(rename = "imageUrl", default)]
    pub image_url: Option<String>,
    #[serde(rename = "editFlags", default)]
    pub edit_flags: ContextMenuEditFlags,
    #[serde(rename = "misspelledWord", default)]
    pub misspelled_word: Option<String>,
    #[serde(rename = "dictionarySuggestions", default)]
    pub dictionary_suggestions: Vec<String>,
    #[serde(rename = "suggestedName", default)]
    pub suggested_name: Option<String>,
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct UpdateApplyOptions {
    #[serde(rename = "dirtyStrategy", default)]
    pub dirty_strategy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TitlebarThemePayload {
    background: String,
    foreground: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PreviewTargetResult {
    kind: String,
    label: String,
    source: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    binary: Option<bool>,
    #[serde(rename = "byteSize", skip_serializing_if = "Option::is_none")]
    byte_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    large: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(rename = "previewKind", skip_serializing_if = "Option::is_none")]
    preview_kind: Option<String>,
    #[serde(rename = "renderMode", skip_serializing_if = "Option::is_none")]
    render_mode: Option<String>,
}

struct TerminalSession {
    master: StdMutex<Box<dyn portable_pty::MasterPty + Send>>,
    child: StdMutex<Box<dyn Child + Send>>,
    writer: StdMutex<Box<dyn Write + Send>>,
    event_target: String,
    alive: AtomicBool,
    exited: AtomicBool,
}

// ============================================================================
// State
// ============================================================================

pub struct AppState {
    pub connection: Mutex<Option<GatewayConnection>>,
    pub boot_progress: Mutex<BootProgress>,
    pub startup_lock: Mutex<()>,
    pub bootstrap_failure: Mutex<Option<String>>,
    pub backend_pid: StdMutex<Option<u32>>,
    pub bootstrap_state: StdMutex<BootstrapState>,
    pub window_zoom: StdMutex<f64>,
    pub context_menu_request: StdMutex<Option<ContextMenuRequest>>,
    pub preview_watches: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub preview_shortcut_active: AtomicBool,
    pub update_in_flight: AtomicBool,
    pub ws_router: Arc<StreamRouter>,
    pub ws_bridge: Mutex<Option<Arc<HermesWsBridge>>>,
    pub lazy_backend: hermes_backend::LazyBackendGate,
    terminal_sessions: Arc<StdMutex<HashMap<String, Arc<TerminalSession>>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            connection: Mutex::new(None),
            boot_progress: Mutex::new(BootProgress {
                phase: "idle".to_string(),
                message: "Waiting to start Hermes backend".to_string(),
                progress: 0,
                running: false,
                error: None,
                fake_mode: false,
                timestamp: chrono::Utc::now().timestamp_millis(),
            }),
            startup_lock: Mutex::new(()),
            bootstrap_failure: Mutex::new(None),
            backend_pid: StdMutex::new(None),
            bootstrap_state: StdMutex::new(initial_bootstrap_state()),
            window_zoom: StdMutex::new(1.0),
            context_menu_request: StdMutex::new(None),
            preview_watches: Mutex::new(HashMap::new()),
            preview_shortcut_active: AtomicBool::new(false),
            update_in_flight: AtomicBool::new(false),
            ws_router: StreamRouter::shared(),
            ws_bridge: Mutex::new(None),
            lazy_backend: hermes_backend::LazyBackendGate::new(),
            terminal_sessions: Arc::new(StdMutex::new(HashMap::new())),
        }
    }
}

fn clamp_boot_progress(value: u32) -> u32 {
    value.min(100)
}

async fn update_boot_progress(
    state: &AppState,
    phase: Option<&str>,
    message: Option<&str>,
    progress: Option<u32>,
    running: Option<bool>,
    error: Option<Option<String>>,
    allow_decrease: bool,
) {
    let mut snapshot = state.boot_progress.lock().await;
    let next_progress_raw = progress
        .map(clamp_boot_progress)
        .unwrap_or(snapshot.progress);
    let next_progress = if allow_decrease {
        next_progress_raw
    } else {
        snapshot.progress.max(next_progress_raw)
    };

    if let Some(value) = phase {
        snapshot.phase = value.to_string();
    }
    if let Some(value) = message {
        snapshot.message = value.to_string();
        append_desktop_log(&format!("[boot] {}\n", value));
    }
    if let Some(value) = running {
        snapshot.running = value;
    }
    if let Some(value) = error {
        snapshot.error = value;
    }

    snapshot.progress = next_progress;
    snapshot.timestamp = chrono::Utc::now().timestamp_millis();
}

async fn fail_boot_progress(state: &AppState, message: String) {
    update_boot_progress(
        state,
        Some("backend.error"),
        Some(&format!("Desktop boot failed: {}", message)),
        None,
        Some(false),
        Some(Some(message)),
        true,
    )
    .await;
}

pub(crate) fn resolve_hermes_home() -> PathBuf {
    if let Ok(home) = std::env::var("HERMES_HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    #[cfg(windows)]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let local = PathBuf::from(local_app_data).join("hermes");
            let legacy = dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".hermes");
            if !local.exists() && legacy.exists() {
                return legacy;
            }
            return local;
        }
    }

    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".hermes")
}

pub(crate) fn desktop_log_path() -> PathBuf {
    resolve_hermes_home().join("logs").join("desktop.log")
}

fn append_desktop_log(chunk: &str) {
    let log_path = desktop_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, chunk.as_bytes()));
}

fn update_bootstrap_state_with_event(state: &AppState, event: &serde_json::Value) {
    let Ok(mut snapshot) = state.bootstrap_state.lock() else {
        return;
    };

    let event_type = event
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    match event_type {
        "manifest" => {
            snapshot.manifest = Some(event.clone());
            snapshot.active = true;
            if snapshot.started_at.is_none() {
                snapshot.started_at = Some(chrono::Utc::now().timestamp_millis() as u64);
            }
            snapshot.stages.clear();
            if let Some(stages) = event.get("stages").and_then(|value| value.as_array()) {
                for stage in stages {
                    if let Some(name) = stage.get("name").and_then(|value| value.as_str()) {
                        snapshot.stages.insert(
                            name.to_string(),
                            BootstrapStageResult {
                                state: "pending".to_string(),
                                duration_ms: None,
                                started_at: None,
                                json: None,
                                error: None,
                            },
                        );
                    }
                }
            }
            snapshot.error = None;
            snapshot.unsupported_platform = None;
        }
        "stage" => {
            let Some(name) = event.get("name").and_then(|value| value.as_str()) else {
                return;
            };
            let current_started_at = snapshot.stages.get(name).and_then(|stage| stage.started_at);
            let next_state = event
                .get("state")
                .and_then(|value| value.as_str())
                .unwrap_or("pending")
                .to_string();
            snapshot.stages.insert(
                name.to_string(),
                BootstrapStageResult {
                    state: next_state.clone(),
                    duration_ms: event.get("durationMs").and_then(|value| value.as_u64()),
                    started_at: if next_state == "running" {
                        current_started_at
                            .or_else(|| Some(chrono::Utc::now().timestamp_millis() as u64))
                    } else {
                        current_started_at
                    },
                    json: event.get("json").cloned(),
                    error: event
                        .get("error")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string()),
                },
            );
        }
        "log" => {
            snapshot.log.push(serde_json::json!({
                "ts": chrono::Utc::now().timestamp_millis(),
                "stage": event.get("stage").and_then(|value| value.as_str()),
                "line": event.get("line").and_then(|value| value.as_str()).unwrap_or_default(),
            }));
            if snapshot.log.len() > BOOTSTRAP_LOG_RING_MAX {
                let drain = snapshot.log.len() - BOOTSTRAP_LOG_RING_MAX;
                snapshot.log.drain(0..drain);
            }
        }
        "complete" => {
            snapshot.active = false;
            snapshot.completed_at = Some(chrono::Utc::now().timestamp_millis() as u64);
            snapshot.error = None;
            snapshot.unsupported_platform = None;
        }
        "failed" => {
            snapshot.active = false;
            snapshot.error = event
                .get("error")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
                .or_else(|| Some("unknown error".to_string()));
        }
        "unsupported-platform" => {
            snapshot.active = false;
            snapshot.unsupported_platform = Some(serde_json::json!({
                "platform": event.get("platform").and_then(|value| value.as_str()).unwrap_or(std::env::consts::OS),
                "activeRoot": event.get("activeRoot").and_then(|value| value.as_str()).unwrap_or_default(),
                "installCommand": event.get("installCommand").and_then(|value| value.as_str()).unwrap_or_default(),
                "docsUrl": event.get("docsUrl").and_then(|value| value.as_str()).unwrap_or(DESKTOP_DOCS_URL),
            }));
        }
        _ => {}
    }
}

fn emit_bootstrap_event(app: &AppHandle, state: &AppState, event: serde_json::Value) {
    update_bootstrap_state_with_event(state, &event);
    let _ = app.emit_to("main", BOOTSTRAP_EVENT, event);
}
// Helpers
// ============================================================================

fn get_connection_config_path() -> PathBuf {
    desktop_app_data_dir().join("connection.json")
}

fn desktop_app_data_dir() -> PathBuf {
    dirs::data_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Hermes")
}

fn default_project_dir_config_path() -> PathBuf {
    desktop_app_data_dir().join("project-dir.json")
}

fn ui_preferences_config_path() -> PathBuf {
    desktop_app_data_dir().join(UI_PREFERENCES_CONFIG_PATH)
}

fn normalize_ui_language(value: Option<String>) -> Option<String> {
    let language = value?.trim().to_lowercase().replace('_', "-");
    match language.as_str() {
        "en" | "en-us" | "en-gb" => Some("en".to_string()),
        "zh" | "zh-cn" | "zh-hans" | "zh-sg" => Some("zh".to_string()),
        "zh-hant" | "zh-hant-hk" | "zh-hant-tw" | "zh-hk" | "zh-mo" | "zh-tw" => {
            Some("zh-hant".to_string())
        }
        "ja" | "ja-jp" => Some("ja".to_string()),
        _ => None,
    }
}

fn read_ui_preferences_from_disk() -> UiPreferences {
    match fs::read_to_string(ui_preferences_config_path()) {
        Ok(content) => serde_json::from_str::<UiPreferences>(&content)
            .map(|mut prefs| {
                prefs.language = normalize_ui_language(prefs.language);
                prefs
            })
            .unwrap_or_default(),
        Err(_) => UiPreferences::default(),
    }
}

fn write_ui_preferences_to_disk(preferences: &UiPreferences) -> Result<UiPreferences, String> {
    let mut normalized = preferences.clone();
    normalized.language = normalize_ui_language(normalized.language);

    let path = ui_preferences_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create dir: {}", e))?;
    }

    let content = serde_json::to_string_pretty(&normalized)
        .map_err(|e| format!("Failed to serialize UI preferences: {}", e))?;
    fs::write(&path, content).map_err(|e| format!("Failed to write UI preferences: {}", e))?;
    Ok(normalized)
}

fn default_project_dir_state() -> DefaultProjectDirState {
    DefaultProjectDirState {
        default_label: dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("hermes-projects")
            .to_string_lossy()
            .to_string(),
        dir: read_default_project_dir().map(|path| path.to_string_lossy().to_string()),
    }
}

fn read_default_project_dir() -> Option<PathBuf> {
    let content = fs::read_to_string(default_project_dir_config_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let path = value.get("dir")?.as_str()?.trim();
    if path.is_empty() {
        return None;
    }

    let resolved = PathBuf::from(path);
    if resolved.is_dir() {
        Some(resolved)
    } else {
        None
    }
}

fn write_default_project_dir(dir: Option<&str>) -> Result<(), String> {
    let path = default_project_dir_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create dir: {}", e))?;
    }

    let payload = match dir
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        Some(value) => {
            serde_json::json!({ "dir": PathBuf::from(value).to_string_lossy().to_string() })
        }
        None => serde_json::json!({}),
    };

    fs::write(
        &path,
        serde_json::to_string_pretty(&payload)
            .map_err(|e| format!("Failed to serialize config: {}", e))?
            + "\n",
    )
    .map_err(|e| format!("Failed to write config: {}", e))
}

fn resolve_timeout_ms(timeout_ms: Option<u64>, fallback_ms: u64) -> u64 {
    let fallback = if fallback_ms > 0 {
        fallback_ms
    } else {
        DEFAULT_FETCH_TIMEOUT_MS
    };

    match timeout_ms {
        Some(value) if value > 0 => value,
        _ => fallback,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionRenameRequest {
    session_id: String,
    session_path: String,
    title: String,
}

fn parse_session_rename_request(request: &ApiRequest) -> Option<SessionRenameRequest> {
    let method = request.method.as_deref().unwrap_or("GET");
    if method != "PATCH" {
        return None;
    }

    let parsed = reqwest::Url::parse(&format!("http://127.0.0.1{}", request.path)).ok()?;
    let segments = parsed.path_segments()?.collect::<Vec<_>>();
    if segments.len() != 3 || segments[0] != "api" || segments[1] != "sessions" {
        return None;
    }

    let body = request.body.as_ref()?.as_object()?;
    let title = match body.get("title") {
        Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(value)) => value.clone(),
        _ => return None,
    };

    Some(SessionRenameRequest {
        session_id: segments[2].to_string(),
        session_path: parsed.path().to_string(),
        title,
    })
}

fn rename_title_fallback(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn try_handle_local_session_rename(
    request: &ApiRequest,
    base_url: &str,
    token: &str,
    mode: &str,
) -> Result<Option<serde_json::Value>, String> {
    if mode != "local" {
        return Ok(None);
    }

    let Some(rename) = parse_session_rename_request(request) else {
        return Ok(None);
    };

    let _ = (rename, base_url, token);
    Ok(None)
}

fn parse_hermes_api_response(
    url: &str,
    status: reqwest::StatusCode,
    content_type: Option<&str>,
    text: &str,
) -> Result<serde_json::Value, String> {
    if status.as_u16() >= 400 {
        let message = if text.trim().is_empty() {
            status
                .canonical_reason()
                .unwrap_or("Request failed")
                .to_string()
        } else {
            text.to_string()
        };
        return Err(format!("{}: {}", status.as_u16(), message));
    }

    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }

    let content_type = content_type.unwrap_or_default();
    let looks_html = regex::Regex::new(r"^\s*<(?:!doctype|html)")
        .ok()
        .map(|re| re.is_match(text))
        .unwrap_or(false);
    if looks_html || content_type.contains("text/html") {
        return Err(format!(
            "Expected JSON from {} but got HTML (status {}). The endpoint is likely missing on the Hermes backend.",
            url,
            status.as_u16()
        ));
    }

    serde_json::from_str(text).map_err(|_| {
        format!(
            "Invalid JSON from {} (status {}): {}",
            url,
            status.as_u16(),
            text.chars().take(200).collect::<String>()
        )
    })
}

fn sensitive_file_block_reason(file_path: &Path) -> Option<String> {
    let normalized = file_path
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase();
    let basename = file_path.file_name()?.to_string_lossy().to_lowercase();
    let ext = Path::new(&basename)
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy().to_lowercase()))
        .unwrap_or_default();

    if normalized.contains("/.ssh/") {
        return Some("SSH key/config files are blocked.".to_string());
    }
    if normalized.contains("/.gnupg/") {
        return Some("GPG key material is blocked.".to_string());
    }
    if normalized.ends_with("/.aws/credentials") {
        return Some("AWS credential files are blocked.".to_string());
    }
    if basename == ".env" {
        return Some(".env files are blocked because they commonly contain secrets.".to_string());
    }
    if let Some(suffix) = basename.strip_prefix(".env.") {
        if !SAFE_ENV_SUFFIXES.contains(&suffix) {
            return Some(format!(
                "{} is blocked because it appears to contain environment secrets.",
                basename
            ));
        }
    }
    if regex::Regex::new(r"^id_(rsa|dsa|ecdsa|ed25519)(?:\..+)?$")
        .ok()
        .map(|re| re.is_match(&basename))
        .unwrap_or(false)
        && !basename.ends_with(".pub")
    {
        return Some("SSH private key files are blocked.".to_string());
    }
    if SENSITIVE_EXTENSIONS.contains(&ext.as_str()) {
        return Some(format!("{} key/certificate files are blocked.", ext));
    }
    if matches!(basename.as_str(), ".npmrc" | ".netrc" | ".pypirc") {
        return Some(format!(
            "{} is blocked because it may include auth credentials.",
            basename
        ));
    }

    None
}

fn resolve_requested_file_path(
    file_path: &str,
    base_dir: Option<&Path>,
    purpose: &str,
) -> Result<PathBuf, String> {
    let raw = file_path.trim();
    if raw.is_empty() {
        return Err(format!("{} failed: file path is required.", purpose));
    }
    if raw.contains('\0') {
        return Err(format!("{} failed: file path is invalid.", purpose));
    }
    if raw.to_ascii_lowercase().starts_with("file:") {
        let parsed = reqwest::Url::parse(raw)
            .map_err(|_| format!("{} failed: file URL is invalid.", purpose))?;
        return parsed
            .to_file_path()
            .map_err(|_| format!("{} failed: file URL is invalid.", purpose));
    }

    let resolved_base = base_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    Ok(if PathBuf::from(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        resolved_base.join(raw)
    })
}

struct ResolveReadableFileOptions<'a> {
    base_dir: Option<&'a Path>,
    block_sensitive: bool,
    max_bytes: Option<u64>,
    purpose: &'a str,
}

impl Default for ResolveReadableFileOptions<'_> {
    fn default() -> Self {
        Self {
            base_dir: None,
            block_sensitive: true,
            max_bytes: None,
            purpose: "",
        }
    }
}

fn resolve_readable_file_for_ipc(
    file_path: &str,
    options: ResolveReadableFileOptions<'_>,
) -> Result<(PathBuf, fs::Metadata), String> {
    let purpose = if options.purpose.trim().is_empty() {
        "File read"
    } else {
        options.purpose
    };
    let resolved_path = resolve_requested_file_path(file_path, options.base_dir, purpose)?;
    if options.block_sensitive && !matches!(options.purpose, "Media stream") {
        if let Some(reason) = sensitive_file_block_reason(&resolved_path) {
            return Err(format!(
                "{} blocked for sensitive file: {}",
                purpose, reason
            ));
        }
    }

    let stat = fs::metadata(&resolved_path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => format!("{} failed: file does not exist.", purpose),
        _ => format!("{} failed: {}", purpose, error),
    })?;

    if stat.is_dir() {
        return Err(format!("{} failed: path points to a directory.", purpose));
    }
    if !stat.is_file() {
        return Err(format!(
            "{} failed: only regular files can be read.",
            purpose
        ));
    }
    if let Some(max_bytes) = options.max_bytes.filter(|value| *value > 0) {
        if stat.len() > max_bytes {
            return Err(format!(
                "{} failed: file is too large ({} bytes; limit {} bytes).",
                purpose,
                stat.len(),
                max_bytes
            ));
        }
    }
    fs::File::open(&resolved_path)
        .and_then(|file| file.metadata())
        .map_err(|_| format!("{} failed: file is not readable.", purpose))?;

    Ok((resolved_path, stat))
}

fn resolve_dir_path(path: &str) -> PathBuf {
    let raw = path.trim();
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if raw.is_empty() {
        base
    } else {
        let candidate = PathBuf::from(raw);
        if candidate.is_absolute() {
            candidate
        } else {
            base.join(candidate)
        }
    }
}

fn io_error_code(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".to_string(),
        std::io::ErrorKind::PermissionDenied => "EACCES".to_string(),
        std::io::ErrorKind::AlreadyExists => "EEXIST".to_string(),
        std::io::ErrorKind::InvalidInput => "EINVAL".to_string(),
        std::io::ErrorKind::TimedOut => "ETIMEDOUT".to_string(),
        _ => error
            .raw_os_error()
            .map(|code| format!("OS-{}", code))
            .unwrap_or_else(|| "read-error".to_string()),
    }
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut dir = PathBuf::from(start);
    for _ in 0..50 {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        let parent = dir.parent().map(PathBuf::from)?;
        if parent == dir {
            return None;
        }
        dir = parent;
    }
    None
}

fn decode_data_url_text(data: &str) -> Result<Vec<u8>, String> {
    let bytes = data.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err("Failed to decode data URL: incomplete percent escape".to_string());
            }

            let hi = bytes[index + 1];
            let lo = bytes[index + 2];
            let hex = [hi, lo];
            let value = std::str::from_utf8(&hex)
                .ok()
                .and_then(|value| u8::from_str_radix(value, 16).ok())
                .ok_or_else(|| "Failed to decode data URL: invalid percent escape".to_string())?;

            decoded.push(value);
            index += 3;
            continue;
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    Ok(decoded)
}

fn filename_from_url(raw_url: &str, fallback: &str) -> String {
    let Ok(parsed) = reqwest::Url::parse(raw_url) else {
        return fallback.to_string();
    };

    let encoded = parsed
        .path()
        .rsplit('/')
        .next()
        .map(str::trim)
        .unwrap_or_default();
    if encoded.is_empty() {
        return fallback.to_string();
    }

    let decoded = decode_data_url_text(encoded)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_else(|| encoded.to_string());

    if decoded.contains('.') {
        decoded
    } else {
        fallback.to_string()
    }
}

fn preferred_image_save_name(suggested_name: Option<&str>, fallback_name: Option<&str>) -> String {
    let suggested = suggested_name
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(value) = suggested {
        return value.to_string();
    }

    fallback_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("image.png")
        .to_string()
}

async fn resource_buffer_from_url(raw_url: &str) -> Result<(Vec<u8>, Option<String>), String> {
    let url = raw_url.trim();
    if url.is_empty() {
        return Err("Missing URL".to_string());
    }

    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest
            .split_once(',')
            .ok_or_else(|| "Invalid data URL".to_string())?;
        let mime = meta.split(';').next().unwrap_or("application/octet-stream");
        let bytes = if meta.contains(";base64") {
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| format!("Failed to decode data URL: {}", e))?
        } else {
            decode_data_url_text(data)?
        };
        return Ok((bytes, Some(default_image_name_from_mime(mime))));
    }

    if let Ok(parsed) = reqwest::Url::parse(url) {
        if parsed.scheme() == "file" {
            let path = parsed
                .to_file_path()
                .map_err(|_| "Invalid file URL".to_string())?;
            let bytes = fs::read(&path).map_err(|e| format!("Failed to read file: {}", e))?;
            return Ok((
                bytes,
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string()),
            ));
        }
    }

    let response = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch image: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("Failed to fetch image: HTTP {}", status.as_u16()));
    }

    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|s| s.to_string());
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read image: {}", e))?
        .to_vec();
    let fallback = mime
        .as_deref()
        .map(default_image_name_from_mime)
        .unwrap_or_else(|| "image.png".to_string());
    Ok((bytes, Some(filename_from_url(url, &fallback))))
}

fn default_image_name_from_mime(mime: &str) -> String {
    let normalized = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
    let ext = mime_guess::get_mime_extensions_str(&normalized)
        .and_then(|items| items.first())
        .copied()
        .unwrap_or("png");
    format!("image.{}", ext)
}

fn write_composer_image(buffer: &[u8], ext: &str) -> Result<PathBuf, String> {
    let dir = desktop_app_data_dir().join("composer-images");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create dir: {}", e))?;
    let normalized = ext.trim().to_lowercase();
    let safe_ext = if normalized.starts_with('.') {
        normalized
    } else {
        format!(".{}", normalized)
    };
    let safe_ext = if safe_ext
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && safe_ext.len() <= 8
    {
        safe_ext
    } else {
        ".png".to_string()
    };

    let stamp = chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let random = format!("{:06x}", rand::random::<u32>() & 0x00ff_ffff);
    let path = dir.join(format!("composer_{}_{}{}", stamp, random, safe_ext));
    fs::write(&path, buffer).map_err(|e| format!("Failed to write image: {}", e))?;
    Ok(path)
}

fn write_png_from_rgba(bytes: Vec<u8>, width: u32, height: u32) -> Result<PathBuf, String> {
    let dir = desktop_app_data_dir().join("composer-images");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create dir: {}", e))?;
    let stamp = chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let random = format!("{:06x}", rand::random::<u32>() & 0x00ff_ffff);
    let path = dir.join(format!("composer_{}_{}.png", stamp, random));
    let file = fs::File::create(&path).map_err(|e| format!("Failed to create image: {}", e))?;
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("Failed to start PNG write: {}", e))?;
    writer
        .write_image_data(&bytes)
        .map_err(|e| format!("Failed to encode PNG: {}", e))?;
    Ok(path)
}

fn preview_language_for_ext(ext: &str) -> Option<String> {
    Some(
        match ext.to_lowercase().as_str() {
            ".c" => "c",
            ".conf" => "ini",
            ".cpp" => "cpp",
            ".css" => "css",
            ".csv" => "csv",
            ".go" => "go",
            ".graphql" => "graphql",
            ".h" => "c",
            ".hpp" => "cpp",
            ".html" => "html",
            ".java" => "java",
            ".js" => "javascript",
            ".json" => "json",
            ".jsx" => "jsx",
            ".kt" => "kotlin",
            ".lua" => "lua",
            ".md" => "markdown",
            ".mjs" => "javascript",
            ".py" => "python",
            ".rb" => "ruby",
            ".rs" => "rust",
            ".sh" => "shell",
            ".sql" => "sql",
            ".svg" => "xml",
            ".toml" => "toml",
            ".ts" => "typescript",
            ".tsx" => "tsx",
            ".txt" => "text",
            ".xml" => "xml",
            ".yaml" => "yaml",
            ".yml" => "yaml",
            ".zsh" => "shell",
            _ => return None,
        }
        .to_string(),
    )
}

fn looks_binary(buffer: &[u8]) -> bool {
    if buffer.is_empty() {
        return false;
    }

    let mut suspicious = 0usize;
    for byte in buffer {
        if *byte == 0 {
            return true;
        }
        if *byte < 32 && *byte != 9 && *byte != 10 && *byte != 13 {
            suspicious += 1;
        }
    }

    (suspicious as f64 / buffer.len() as f64) > 0.12
}

fn resolve_hermes_cwd() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
}

fn expand_user_path(file_path: &str) -> PathBuf {
    let value = file_path.trim();
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

    if value == "~" {
        return home;
    }

    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return home.join(rest);
    }

    PathBuf::from(value)
}

fn resolve_preview_base_dir(base_dir: &str) -> PathBuf {
    let candidate = expand_user_path(base_dir);
    if candidate.as_os_str().is_empty() {
        return resolve_hermes_cwd();
    }
    if candidate.is_absolute() {
        candidate
    } else {
        resolve_hermes_cwd().join(candidate)
    }
}

fn preview_url_host_label(url: &reqwest::Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{}:{}", host, port),
        None => host.to_string(),
    }
}

fn preview_file_target(raw_target: &str, base_dir: &str) -> Option<PreviewTargetResult> {
    let raw = raw_target.trim();
    if raw.is_empty() {
        return None;
    }

    if raw.starts_with("http://") || raw.starts_with("https://") {
        let mut url = reqwest::Url::parse(raw).ok()?;
        let host = url.host_str()?.to_lowercase();
        if !matches!(host.as_str(), "0.0.0.0" | "127.0.0.1" | "::1" | "localhost") {
            return None;
        }
        if host == "0.0.0.0" {
            url.set_host(Some("127.0.0.1")).ok()?;
        }

        let host_label = preview_url_host_label(&url);
        let label = if url.path() == "/" {
            host_label
        } else {
            format!("{}{}", host_label, url.path())
        };

        return Some(PreviewTargetResult {
            kind: "url".to_string(),
            label,
            source: raw.to_string(),
            url: url.to_string(),
            binary: None,
            byte_size: None,
            large: None,
            language: None,
            mime_type: None,
            path: None,
            preview_kind: None,
            render_mode: None,
        });
    }

    let base = if base_dir.trim().is_empty() {
        resolve_hermes_cwd()
    } else {
        resolve_preview_base_dir(base_dir.trim())
    };

    let mut resolved = if raw.starts_with("file://") {
        reqwest::Url::parse(raw).ok()?.to_file_path().ok()?
    } else {
        let candidate = expand_user_path(raw);
        if candidate.is_absolute() {
            candidate
        } else {
            base.join(candidate)
        }
    };

    if resolved.is_dir() {
        resolved = resolved.join("index.html");
    }
    if !resolved.exists() {
        return None;
    }

    let ext = resolved
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_lowercase()))
        .unwrap_or_default();
    let mime = mime_guess::from_path(&resolved)
        .first_or_octet_stream()
        .to_string();
    let stat = fs::metadata(&resolved).ok()?;
    let bytes = fs::read(&resolved).ok()?;
    let binary = looks_binary(&bytes[..bytes.len().min(4096)]);
    let preview_kind = if matches!(ext.as_str(), ".html" | ".htm") {
        "html"
    } else if mime.starts_with("image/") {
        "image"
    } else if binary {
        "binary"
    } else {
        "text"
    };

    Some(PreviewTargetResult {
        kind: "file".to_string(),
        label: resolved
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_string(),
        source: raw.to_string(),
        url: format!("file://{}", resolved.to_string_lossy().replace('\\', "/")),
        binary: Some(binary),
        byte_size: Some(stat.len()),
        large: Some(stat.len() > 1024 * 1024),
        language: preview_language_for_ext(&ext),
        mime_type: Some(mime),
        path: Some(resolved.to_string_lossy().to_string()),
        preview_kind: Some(preview_kind.to_string()),
        render_mode: if preview_kind == "html" {
            Some("preview".to_string())
        } else {
            None
        },
    })
}

fn normalize_preview_target_impl(target: &str, base_dir: &str) -> Option<PreviewTargetResult> {
    preview_file_target(target, base_dir)
}

fn start_preview_file_watcher(
    watch_dir: &Path,
    tx: std::sync::mpsc::Sender<notify::Result<notify::Event>>,
) -> Result<notify::RecommendedWatcher, String> {
    let mut watcher = recommended_watcher(move |result| {
        let _ = tx.send(result);
    })
    .map_err(|e| format!("Failed to watch preview file: {}", e))?;

    watcher
        .watch(watch_dir, RecursiveMode::NonRecursive)
        .map_err(|e| format!("Failed to watch preview file: {}", e))?;

    Ok(watcher)
}

async fn watch_preview_file_impl(
    url: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PreviewWatch, String> {
    let file_path = file_path_from_preview_url(&url)?;
    let watch_dir = file_path
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| "Preview file has no parent directory".to_string())?;
    let target_name = file_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string)
        .ok_or_else(|| "Preview file has no file name".to_string())?;
    let watched_path = file_path.clone();
    let id = generate_token();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let event_id = id.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let watcher = start_preview_file_watcher(&watch_dir, tx)?;
    {
        let mut watchers = state.preview_watches.lock().await;
        watchers.insert(id.clone(), stop_flag.clone());
    }

    thread::spawn(move || {
        let _watcher = watcher;

        let mut pending_emit = None::<std::time::Instant>;

        loop {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }

            match rx.recv_timeout(Duration::from_millis(60)) {
                Ok(Ok(event)) => {
                    if preview_watch_matches_target(&event.paths, &target_name) {
                        pending_emit = Some(std::time::Instant::now());
                    }
                }
                Ok(Err(_)) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }

            if let Some(started_at) = pending_emit {
                if started_at.elapsed() >= Duration::from_millis(PREVIEW_WATCH_DEBOUNCE_MS) {
                    pending_emit = None;
                    if !watched_path.is_file() {
                        continue;
                    }

                    let _ = app.emit(
                        "hermes:preview-file-changed",
                        PreviewWatchPayload {
                            id: event_id.clone(),
                            path: watched_path.to_string_lossy().to_string(),
                            url: format!(
                                "file://{}",
                                watched_path.to_string_lossy().replace('\\', "/")
                            ),
                        },
                    );
                }
            }
        }
    });

    Ok(PreviewWatch {
        id,
        path: file_path.to_string_lossy().to_string(),
    })
}

async fn stop_preview_file_watch_impl(id: String, state: State<'_, AppState>) -> bool {
    let stop_flag = {
        let mut watchers = state.preview_watches.lock().await;
        watchers.remove(&id)
    };

    if let Some(flag) = stop_flag {
        flag.store(true, Ordering::Relaxed);
        true
    } else {
        false
    }
}

async fn stop_all_preview_file_watches_impl(state: &AppState) -> usize {
    let stop_flags = {
        let mut watchers = state.preview_watches.lock().await;
        watchers.drain().map(|(_, flag)| flag).collect::<Vec<_>>()
    };

    let count = stop_flags.len();
    for flag in stop_flags {
        flag.store(true, Ordering::Relaxed);
    }

    count
}

fn preview_watch_matches_target(paths: &[PathBuf], target_name: &str) -> bool {
    if paths.is_empty() {
        return true;
    }

    paths.iter().any(|path| {
        path.file_name()
            .and_then(|value| value.to_str())
            .map(|value| value == target_name)
            .unwrap_or(false)
    })
}

fn file_path_from_preview_url(raw_url: &str) -> Result<PathBuf, String> {
    let url = reqwest::Url::parse(raw_url.trim())
        .map_err(|_| "Preview file is not readable".to_string())?;
    if url.scheme() != "file" {
        return Err("Preview file is not readable".to_string());
    }

    let path = url
        .to_file_path()
        .map_err(|_| "Preview file is not readable".to_string())?;
    if path.exists() {
        Ok(path)
    } else {
        Err("Preview file is not readable".to_string())
    }
}

fn initial_bootstrap_state() -> BootstrapState {
    BootstrapState {
        active: false,
        manifest: None,
        stages: HashMap::new(),
        error: None,
        log: Vec::new(),
        started_at: None,
        completed_at: None,
        unsupported_platform: None,
    }
}

fn terminal_shell_command() -> (String, Vec<String>, String) {
    #[cfg(windows)]
    {
        let command = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        let shell_name = PathBuf::from(&command)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("cmd.exe")
            .to_string();
        return (command, Vec::new(), shell_name);
    }

    #[cfg(not(windows))]
    {
        let configured_shell = std::env::var("SHELL").unwrap_or_default();
        let shell_path = if PathBuf::from(&configured_shell).is_absolute()
            && PathBuf::from(&configured_shell).exists()
        {
            configured_shell
        } else {
            ["/bin/zsh", "/bin/bash", "/bin/sh"]
                .iter()
                .find(|candidate| PathBuf::from(candidate).exists())
                .map(|value| value.to_string())
                .unwrap_or_else(|| "/bin/sh".to_string())
        };

        let shell_name = PathBuf::from(&shell_path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("sh")
            .to_string();
        let args = if shell_name.contains("zsh") || shell_name.contains("bash") {
            vec!["-il".to_string()]
        } else {
            vec!["-i".to_string()]
        };

        (shell_path, args, shell_name)
    }
}

fn safe_terminal_cwd(cwd: Option<&str>) -> PathBuf {
    let fallback = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let candidate = cwd
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| fallback.clone())
                    .join(path)
            }
        })
        .unwrap_or_else(|| fallback.clone());

    match fs::metadata(&candidate) {
        Ok(metadata) if metadata.is_dir() => candidate,
        Ok(_) => candidate.parent().map(PathBuf::from).unwrap_or(fallback),
        Err(_) => fallback,
    }
}

fn configure_terminal_env(builder: &mut CommandBuilder) {
    let keys_to_remove = builder
        .iter_full_env_as_str()
        .filter_map(|(key, _)| {
            if key == "npm_config_prefix"
                || key.starts_with("npm_config_")
                || key.starts_with("npm_package_")
                || key == "NO_COLOR"
                || key == "FORCE_COLOR"
                || key == "COLORFGBG"
            {
                Some(key.to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    for key in keys_to_remove {
        builder.env_remove(key);
    }

    builder.env("COLORTERM", "truecolor");
    let has_lc_ctype = builder
        .get_env("LC_CTYPE")
        .and_then(|value| value.to_str())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if !has_lc_ctype {
        builder.env("LC_CTYPE", "UTF-8");
    }
    builder.env("TERM", "xterm-256color");
    builder.env("TERM_PROGRAM", "Hermes");
    builder.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
}

fn terminal_event_target(label: Option<&str>) -> &str {
    label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("main")
}

fn terminal_channel(id: &str, suffix: &str) -> String {
    format!("hermes:terminal:{}:{}", id, suffix)
}

fn spawn_terminal_reader(
    app: AppHandle,
    id: String,
    mut reader: Box<dyn Read + Send>,
    terminal_sessions: Arc<StdMutex<HashMap<String, Arc<TerminalSession>>>>,
    session: Arc<TerminalSession>,
) {
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];

        loop {
            if !session.alive.load(Ordering::Relaxed) {
                break;
            }

            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(length) => {
                    if session.alive.load(Ordering::Relaxed) {
                        let text = String::from_utf8_lossy(&buffer[..length]).to_string();
                        let _ = app.emit_to(
                            session.event_target.as_str(),
                            &terminal_channel(&id, "data"),
                            text,
                        );
                    }
                }
                Err(_) => break,
            }
        }

        let exit_status = session
            .child
            .lock()
            .ok()
            .and_then(|mut child| child.wait().ok());

        if session.exited.swap(true, Ordering::Relaxed) {
            let _ = terminal_sessions
                .lock()
                .map(|mut sessions| sessions.remove(&id));
            return;
        }

        let payload = if let Some(status) = exit_status {
            serde_json::json!({
                "code": status.exit_code(),
                "signal": status.signal().map(|value| value.to_string()),
            })
        } else {
            serde_json::json!({ "code": null, "signal": null })
        };

        let _ = app.emit_to(
            session.event_target.as_str(),
            &terminal_channel(&id, "exit"),
            payload,
        );
        let _ = terminal_sessions
            .lock()
            .map(|mut sessions| sessions.remove(&id));
    });
}

fn dispose_terminal_session_impl(session: &TerminalSession) {
    session.alive.store(false, Ordering::Relaxed);

    if let Ok(mut child) = session.child.lock() {
        let _ = child.kill();
    }
}

fn dispose_all_terminal_sessions_impl(state: &AppState) -> usize {
    let sessions = {
        let Ok(mut sessions) = state.terminal_sessions.lock() else {
            return 0;
        };
        sessions
            .drain()
            .map(|(_, session)| session)
            .collect::<Vec<_>>()
    };

    let count = sessions.len();
    for session in sessions {
        dispose_terminal_session_impl(session.as_ref());
    }

    count
}

fn spawn_backend_exit_monitor(app: AppHandle, mut child: std::process::Child) {
    thread::spawn(move || {
        let pid = child.id();
        if let Ok(status) = child.wait() {
            if let Ok(mut tracked_pid) = app.state::<AppState>().backend_pid.lock() {
                if tracked_pid.as_ref() == Some(&pid) {
                    *tracked_pid = None;
                }
            }
            let code = status.code();
            let signal = None::<String>;
            let _ = app.emit(
                "hermes:backend-exit",
                serde_json::json!({ "code": code, "signal": signal }),
            );
        }
    });
}

#[allow(dead_code)]
pub(crate) fn desktop_command_creation_flags_for(platform: &str) -> u32 {
    if platform.eq_ignore_ascii_case("windows") {
        WINDOWS_CREATE_NO_WINDOW
    } else {
        0
    }
}

pub(crate) fn configure_desktop_command(command: &mut StdCommand) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        command.creation_flags(desktop_command_creation_flags_for("windows"));
    }

    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

pub(crate) fn desktop_command(program: impl AsRef<OsStr>) -> StdCommand {
    let mut command = StdCommand::new(program);
    configure_desktop_command(&mut command);
    command
}

pub fn terminate_tracked_backend(state: &AppState) {
    let pid = state
        .backend_pid
        .lock()
        .ok()
        .and_then(|tracked_pid| *tracked_pid);

    let Some(pid) = pid else {
        return;
    };

    #[cfg(unix)]
    let _ = desktop_command("kill")
        .args(["-TERM", &pid.to_string()])
        .status();

    #[cfg(windows)]
    let _ = desktop_command("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status();

    if let Ok(mut tracked_pid) = state.backend_pid.lock() {
        if tracked_pid.as_ref() == Some(&pid) {
            *tracked_pid = None;
        }
    }
}

pub async fn stop_all_preview_file_watches(state: &AppState) {
    let _ = stop_all_preview_file_watches_impl(state).await;
}

pub fn dispose_all_terminal_sessions(state: &AppState) {
    let _ = dispose_all_terminal_sessions_impl(state);
}

fn find_free_port() -> Option<u16> {
    for port in 8787..8867 {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Some(port);
        }
    }
    None
}

fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect()
}

mod backend;
mod clipboard;
mod images;
mod preview;
mod settings;
mod terminal;

pub use backend::*;
pub use clipboard::*;
pub use images::*;
pub use preview::*;
pub use settings::*;
pub use terminal::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEST_PROCESS_ENV_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();

    struct TempDirGuard {
        path: PathBuf,
    }

    impl TempDirGuard {
        fn new(label: &str) -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "hermes-desktop-community-{}-{}-{}",
                label,
                std::process::id(),
                stamp
            ));
            fs::create_dir_all(&path).expect("temp dir should be created");
            Self { path }
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    struct CurrentDirGuard {
        previous: PathBuf,
    }

    impl CurrentDirGuard {
        fn enter(path: &Path) -> Self {
            let previous = std::env::current_dir().expect("cwd should be readable");
            std::env::set_current_dir(path).expect("cwd should be updated");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: std::ffi::OsString) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn run_git_test(args: &[&str], cwd: &Path) {
        let status = StdCommand::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git command should start");
        assert!(status.success(), "git {:?} should succeed", args);
    }

    fn init_git_repo(path: &Path) {
        run_git_test(&["init"], path);
        run_git_test(&["config", "user.email", "tests@example.com"], path);
        run_git_test(&["config", "user.name", "Hermes Tests"], path);
        fs::write(path.join("README.md"), "hello\n").expect("repo seed file should be written");
        run_git_test(&["add", "README.md"], path);
        run_git_test(&["commit", "-m", "init"], path);
    }

    fn git_stdout(args: &[&str], cwd: &Path) -> String {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git command should start");
        assert!(output.status.success(), "git {:?} should succeed", args);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn file_url_for(path: &Path) -> String {
        reqwest::Url::from_file_path(path)
            .expect("path should convert to file url")
            .to_string()
    }

    fn lock_test_process_env() -> std::sync::MutexGuard<'static, ()> {
        TEST_PROCESS_ENV_LOCK
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .expect("test process env lock should acquire")
    }

    fn spawn_test_terminal_session() -> Arc<TerminalSession> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("test PTY should open");

        #[cfg(windows)]
        let builder = {
            let mut builder = CommandBuilder::new("cmd.exe");
            builder.args(["/C", "ping", "-n", "30", "127.0.0.1"]);
            builder
        };

        #[cfg(not(windows))]
        let builder = {
            let mut builder = CommandBuilder::new("/bin/sh");
            builder.args(["-c", "sleep 30"]);
            builder
        };

        let master = pair.master;
        let writer = master.take_writer().expect("test PTY writer should open");
        let child = pair
            .slave
            .spawn_command(builder)
            .expect("test PTY child should spawn");

        Arc::new(TerminalSession {
            master: StdMutex::new(master),
            child: StdMutex::new(child),
            writer: StdMutex::new(writer),
            event_target: "main".to_string(),
            alive: AtomicBool::new(true),
            exited: AtomicBool::new(false),
        })
    }

    #[test]
    fn token_preview_matches_desktop_behavior() {
        assert_eq!(token_preview(""), None);
        assert_eq!(token_preview("12345678"), Some("set".to_string()));
        assert_eq!(token_preview("123456789"), Some("...456789".to_string()));
    }

    #[test]
    fn selected_agent_git_url_honors_explicit_source_choice() {
        let github = UpdateSourceConfig {
            agent_git_source: "github".to_string(),
            agent_git_custom_url: String::new(),
            python_source: "pypi".to_string(),
            python_custom_url: String::new(),
            npm_source: "npmjs".to_string(),
            npm_custom_url: String::new(),
            desktop_repo_url: DEFAULT_DESKTOP_REPO_URL.to_string(),
        };
        assert_eq!(
            selected_agent_git_url(&github),
            DEFAULT_AGENT_GIT_URL.to_string()
        );

        let gitee = UpdateSourceConfig {
            agent_git_source: "gitee".to_string(),
            ..github.clone()
        };
        assert_eq!(
            selected_agent_git_url(&gitee),
            GITEE_AGENT_GIT_URL.to_string()
        );

        let gitcode = UpdateSourceConfig {
            agent_git_source: "gitcode".to_string(),
            ..github.clone()
        };
        assert_eq!(
            selected_agent_git_url(&gitcode),
            GITCODE_AGENT_GIT_URL.to_string()
        );

        let custom = UpdateSourceConfig {
            agent_git_source: "custom".to_string(),
            agent_git_custom_url: "https://mirror.example.com/hermes-agent.git".to_string(),
            ..github
        };
        assert_eq!(
            selected_agent_git_url(&custom),
            "https://mirror.example.com/hermes-agent.git".to_string()
        );
    }

    #[test]
    fn tsinghua_python_source_falls_back_to_pypi() {
        let normalized = normalize_update_source_config(UpdateSourceConfig {
            agent_git_source: "github".to_string(),
            agent_git_custom_url: String::new(),
            python_source: "tsinghua".to_string(),
            python_custom_url: String::new(),
            npm_source: "npmjs".to_string(),
            npm_custom_url: String::new(),
            desktop_repo_url: DEFAULT_DESKTOP_REPO_URL.to_string(),
        });

        assert_eq!(normalized.python_source, "pypi".to_string());
        assert_eq!(
            selected_python_index_url(&normalized),
            Some(DEFAULT_PYTHON_INDEX_URL.to_string())
        );
    }

    #[test]
    fn managed_update_stash_round_trips_local_changes() {
        let temp = TempDirGuard::new("managed-update-stash");
        init_git_repo(&temp.path);
        fs::write(temp.path.join("README.md"), "changed locally\n")
            .expect("dirty tracked file should be written");
        fs::write(temp.path.join("local.txt"), "untracked\n")
            .expect("dirty untracked file should be written");

        let stash_ref = stash_managed_update_changes(&temp.path)
            .expect("stashing should succeed")
            .expect("dirty repo should produce a stash ref");
        assert_eq!(git_stdout(&["status", "--porcelain"], &temp.path), "");

        run_git_test(&["merge", "--ff-only", "HEAD"], &temp.path);

        restore_managed_update_stash(&temp.path, &stash_ref).expect("stash restore should succeed");

        let status = git_stdout(&["status", "--porcelain"], &temp.path);
        assert!(status.contains("M README.md"), "status was: {}", status);
        assert!(status.contains("?? local.txt"), "status was: {}", status);
    }

    #[test]
    fn managed_update_stash_is_noop_for_clean_repo() {
        let temp = TempDirGuard::new("managed-update-clean");
        init_git_repo(&temp.path);

        let stash_ref =
            stash_managed_update_changes(&temp.path).expect("clean repo status should succeed");

        assert!(stash_ref.is_none());
        assert_eq!(git_stdout(&["stash", "list"], &temp.path), "");
    }

    #[test]
    fn normalize_remote_base_url_strips_query_hash_and_trailing_slash() {
        let normalized = normalize_remote_base_url("https://example.com/hermes/?foo=bar#frag")
            .expect("url should normalize");

        assert_eq!(normalized, "https://example.com/hermes");
    }

    #[test]
    fn resolve_hermes_web_dist_dir_returns_dist_when_index_exists() {
        let temp = TempDirGuard::new("hermes-web-dist");
        let dist_dir = temp.path.join("hermes_cli").join("web_dist");
        fs::create_dir_all(&dist_dir).expect("web dist dir should be created");
        fs::write(dist_dir.join("index.html"), "<html></html>")
            .expect("index.html should be written");

        let resolved = resolve_hermes_web_dist_dir(&temp.path);

        assert_eq!(resolved, Some(dist_dir));
    }

    #[test]
    fn resolve_hermes_web_dist_dir_returns_none_without_index() {
        let temp = TempDirGuard::new("hermes-web-dist-missing");
        let dist_dir = temp.path.join("hermes_cli").join("web_dist");
        fs::create_dir_all(&dist_dir).expect("web dist dir should be created");

        let resolved = resolve_hermes_web_dist_dir(&temp.path);

        assert_eq!(resolved, None);
    }

    #[test]
    fn gateway_connection_serializes_with_camel_case_keys() {
        let value = serde_json::to_value(GatewayConnection {
            base_url: "http://127.0.0.1:9120".to_string(),
            token: "secret".to_string(),
            ws_url: "ws://127.0.0.1:9120/api/ws?token=secret".to_string(),
            mode: "local".to_string(),
            auth_mode: Some("token".to_string()),
            profile: None,
            source: Some("local".to_string()),
            logs: vec!["[hermes] ready".to_string()],
            is_fullscreen: false,
            native_overlay_width: 144,
            window_button_position: Some(WindowButtonPosition { x: 24, y: 10 }),
        })
        .expect("gateway connection should serialize");

        assert_eq!(
            value.get("baseUrl").and_then(|v| v.as_str()),
            Some("http://127.0.0.1:9120")
        );
        assert_eq!(
            value.get("wsUrl").and_then(|v| v.as_str()),
            Some("ws://127.0.0.1:9120/api/ws?token=secret")
        );
        assert_eq!(value.get("source").and_then(|v| v.as_str()), Some("local"));
        assert_eq!(
            value
                .get("logs")
                .and_then(|v| v.as_array())
                .map(|v| v.len()),
            Some(1)
        );
        assert_eq!(
            value.get("nativeOverlayWidth").and_then(|v| v.as_i64()),
            Some(144)
        );
        assert!(value.get("base_url").is_none());
        assert!(value.get("ws_url").is_none());
    }

    #[test]
    fn preview_watch_serializes_with_expected_shape() {
        let value = serde_json::to_value(PreviewWatch {
            id: "watch-1".to_string(),
            path: "/tmp/index.html".to_string(),
        })
        .expect("preview watch should serialize");

        assert_eq!(value.get("id").and_then(|v| v.as_str()), Some("watch-1"));
        assert_eq!(
            value.get("path").and_then(|v| v.as_str()),
            Some("/tmp/index.html")
        );
    }

    #[test]
    fn preview_watch_matches_target_accepts_empty_paths_and_matching_names() {
        assert!(preview_watch_matches_target(&[], "index.html"));
        assert!(preview_watch_matches_target(
            &[PathBuf::from("/tmp/project/index.html")],
            "index.html"
        ));
        assert!(!preview_watch_matches_target(
            &[PathBuf::from("/tmp/project/other.html")],
            "index.html"
        ));
    }

    #[tokio::test]
    async fn stop_all_preview_file_watches_marks_flags_and_drains_state() {
        let state = AppState::new();
        let flag_a = Arc::new(AtomicBool::new(false));
        let flag_b = Arc::new(AtomicBool::new(false));

        {
            let mut watchers = state.preview_watches.lock().await;
            watchers.insert("watch-a".to_string(), flag_a.clone());
            watchers.insert("watch-b".to_string(), flag_b.clone());
        }

        let stopped = stop_all_preview_file_watches_impl(&state).await;

        assert_eq!(stopped, 2);
        assert!(flag_a.load(Ordering::Relaxed));
        assert!(flag_b.load(Ordering::Relaxed));
        assert!(state.preview_watches.lock().await.is_empty());
    }

    #[test]
    fn dispose_all_terminal_sessions_marks_sessions_dead_and_drains_state() {
        let state = AppState::new();
        let session = spawn_test_terminal_session();

        {
            let mut sessions = state
                .terminal_sessions
                .lock()
                .expect("terminal sessions should lock");
            sessions.insert("term-1".to_string(), session.clone());
        }

        let disposed = dispose_all_terminal_sessions_impl(&state);

        assert_eq!(disposed, 1);
        assert!(!session.alive.load(Ordering::Relaxed));
        assert!(
            state
                .terminal_sessions
                .lock()
                .expect("terminal sessions should lock")
                .is_empty()
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let exited = session
                .child
                .lock()
                .expect("terminal child should lock")
                .try_wait()
                .expect("terminal child status should be readable");
            if exited.is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "disposed terminal child should exit promptly"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn bootstrap_state_tracks_unsupported_platform_events() {
        let state = AppState::new();

        update_bootstrap_state_with_event(
            &state,
            &serde_json::json!({
                "type": "unsupported-platform",
                "platform": "darwin",
                "activeRoot": "/tmp/hermes-agent",
                "installCommand": "curl -fsSL install.sh | bash",
                "docsUrl": DESKTOP_DOCS_URL,
            }),
        );

        {
            let snapshot = state
                .bootstrap_state
                .lock()
                .expect("bootstrap state should lock");
            let unsupported = snapshot
                .unsupported_platform
                .as_ref()
                .expect("unsupported platform payload should be stored");
            assert!(!snapshot.active);
            assert_eq!(
                unsupported.get("platform").and_then(|value| value.as_str()),
                Some("darwin")
            );
            assert_eq!(
                unsupported
                    .get("activeRoot")
                    .and_then(|value| value.as_str()),
                Some("/tmp/hermes-agent")
            );
        }

        update_bootstrap_state_with_event(
            &state,
            &serde_json::json!({
                "type": "complete",
                "marker": {},
            }),
        );

        let snapshot = state
            .bootstrap_state
            .lock()
            .expect("bootstrap state should lock");
        assert!(snapshot.unsupported_platform.is_none());
        assert!(snapshot.error.is_none());
    }

    #[test]
    fn desktop_command_creation_flags_match_platform_policy() {
        assert_eq!(desktop_command_creation_flags_for("windows"), 0x08000000);
        assert_eq!(desktop_command_creation_flags_for("darwin"), 0);
        assert_eq!(desktop_command_creation_flags_for("linux"), 0);
    }

    #[tokio::test]
    async fn read_dir_filters_hidden_entries_and_sorts_directories_first() {
        let temp = TempDirGuard::new("read-dir");
        fs::create_dir_all(temp.path.join(".git")).expect("hidden dir should be created");
        fs::create_dir_all(temp.path.join("node_modules")).expect("hidden dir should be created");
        fs::create_dir_all(temp.path.join("zeta")).expect("dir should be created");
        fs::create_dir_all(temp.path.join("alpha")).expect("dir should be created");
        fs::write(temp.path.join("b.txt"), "b").expect("file should be written");
        fs::write(temp.path.join("a.txt"), "a").expect("file should be written");

        let result = read_dir(temp.path.to_string_lossy().to_string())
            .await
            .expect("read_dir should succeed");

        let names = result
            .entries
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["alpha", "zeta", "a.txt", "b.txt"]);
        assert_eq!(result.error, None);
    }

    #[tokio::test]
    async fn read_dir_returns_error_payload_for_missing_paths() {
        let temp = TempDirGuard::new("read-dir-missing");
        let result = read_dir(temp.path.join("missing").to_string_lossy().to_string())
            .await
            .expect("read_dir should return an error payload");

        assert!(result.entries.is_empty());
        assert_eq!(result.error.as_deref(), Some("ENOENT"));
    }

    #[tokio::test]
    async fn read_file_text_blocks_sensitive_env_files() {
        let temp = TempDirGuard::new("read-file-block");
        let blocked = temp.path.join(".env");
        fs::write(&blocked, "API_KEY=secret\n").expect("file should be written");

        let error = read_file_text(blocked.to_string_lossy().to_string())
            .await
            .expect_err("sensitive env files should be blocked");

        assert!(error.contains(".env files are blocked"));
    }

    #[tokio::test]
    async fn read_file_text_accepts_file_urls_and_resolves_real_paths() {
        let temp = TempDirGuard::new("read-file-url");
        let file_path = temp.path.join("notes.unknown");
        fs::write(&file_path, "hello from file url\n").expect("file should be written");

        let result = read_file_text(file_url_for(&file_path))
            .await
            .expect("file url should be readable");

        assert_eq!(result.path, file_path.to_string_lossy());
        assert_eq!(result.text, "hello from file url\n");
        assert_eq!(result.language.as_deref(), Some("text"));
        assert_eq!(result.truncated, Some(false));
    }

    #[tokio::test]
    async fn git_root_accepts_nested_file_paths() {
        let temp = TempDirGuard::new("git-root");
        fs::create_dir_all(temp.path.join(".git")).expect("git dir should be created");
        fs::create_dir_all(temp.path.join("nested")).expect("nested dir should be created");
        let file_path = temp.path.join("nested").join("main.rs");
        fs::write(&file_path, "fn main() {}\n").expect("file should be written");

        let root = git_root(file_path.to_string_lossy().to_string())
            .await
            .expect("git root should resolve");

        assert_eq!(root, Some(temp.path.to_string_lossy().to_string()));
    }

    #[tokio::test]
    async fn resource_buffer_from_url_decodes_percent_encoded_data_urls() {
        let (buffer, fallback_name) = resource_buffer_from_url(
            "data:image/svg+xml,%3Csvg%20xmlns%3D%22http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%22%3Ehi%3C%2Fsvg%3E",
        )
        .await
        .expect("percent-encoded data urls should decode");

        assert_eq!(
            String::from_utf8(buffer).expect("svg bytes should be utf8"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\">hi</svg>"
        );
        assert_eq!(fallback_name.as_deref(), Some("image.svg"));
    }

    #[test]
    fn parse_open_external_target_rejects_blank_and_malformed_urls() {
        assert!(parse_open_external_target("   ").is_err());
        assert!(parse_open_external_target("not a url").is_err());
    }

    #[test]
    fn parse_open_external_target_accepts_http_and_file_urls() {
        assert_eq!(
            parse_open_external_target("https://example.com/path").expect("http urls should parse"),
            OpenExternalTarget::Url("https://example.com/path".to_string())
        );

        let temp = TempDirGuard::new("open-external-target");
        let file_path = temp.path.join("notes.txt");
        fs::write(&file_path, "hello").expect("file should be written");
        let file_url = file_url_for(&file_path);

        assert_eq!(
            parse_open_external_target(&file_url).expect("file urls should parse"),
            OpenExternalTarget::File(file_path)
        );
    }

    #[test]
    fn start_preview_file_watcher_rejects_missing_directories() {
        let temp = TempDirGuard::new("preview-watch-start");
        let missing_dir = temp.path.join("missing");
        let (tx, _rx) = std::sync::mpsc::channel();

        let error = start_preview_file_watcher(&missing_dir, tx)
            .expect_err("missing directories should fail before registration");

        assert!(error.contains("Failed to watch preview file"));
    }

    #[test]
    fn safe_terminal_cwd_resolves_relative_file_paths_to_absolute_parent_dirs() {
        let _env_lock = lock_test_process_env();
        let temp = TempDirGuard::new("terminal-cwd");
        fs::create_dir_all(temp.path.join("nested")).expect("nested dir should be created");
        fs::write(temp.path.join("nested").join("script.sh"), "echo hi\n")
            .expect("file should be written");
        let _cwd = CurrentDirGuard::enter(&temp.path);

        let cwd = safe_terminal_cwd(Some("nested/script.sh"));

        let expected =
            fs::canonicalize(temp.path.join("nested")).expect("expected path should canonicalize");
        let actual = fs::canonicalize(cwd).expect("actual path should canonicalize");
        assert_eq!(actual, expected);
    }

    #[test]
    fn configure_terminal_env_strips_problematic_vars_and_preserves_existing_lc_ctype() {
        let mut builder = CommandBuilder::new("sh");
        builder.env("NO_COLOR", "1");
        builder.env("FORCE_COLOR", "0");
        builder.env("COLORFGBG", "15;0");
        builder.env("npm_config_prefix", "/tmp/npm");
        builder.env("npm_config_user_agent", "npm/test");
        builder.env("npm_package_name", "hermes");
        builder.env("LC_CTYPE", "zh_CN.UTF-8");

        configure_terminal_env(&mut builder);

        assert!(builder.get_env("NO_COLOR").is_none());
        assert!(builder.get_env("FORCE_COLOR").is_none());
        assert!(builder.get_env("COLORFGBG").is_none());
        assert!(builder.get_env("npm_config_prefix").is_none());
        assert!(builder.get_env("npm_config_user_agent").is_none());
        assert!(builder.get_env("npm_package_name").is_none());
        assert_eq!(
            builder.get_env("LC_CTYPE").and_then(|value| value.to_str()),
            Some("zh_CN.UTF-8")
        );
        assert_eq!(
            builder
                .get_env("COLORTERM")
                .and_then(|value| value.to_str()),
            Some("truecolor")
        );
        assert_eq!(
            builder.get_env("TERM").and_then(|value| value.to_str()),
            Some("xterm-256color")
        );
        assert_eq!(
            builder
                .get_env("TERM_PROGRAM")
                .and_then(|value| value.to_str()),
            Some("Hermes")
        );
    }

    #[test]
    fn configure_terminal_env_sets_utf8_lc_ctype_when_missing() {
        let mut builder = CommandBuilder::new("sh");
        builder.env_remove("LC_CTYPE");

        configure_terminal_env(&mut builder);

        assert_eq!(
            builder.get_env("LC_CTYPE").and_then(|value| value.to_str()),
            Some("UTF-8")
        );
    }

    #[test]
    fn resolve_updater_binary_uses_hermes_home_when_staged_updater_exists() {
        let _env_lock = lock_test_process_env();
        let temp = TempDirGuard::new("packaged-updater");
        let updater = temp.path.join(if cfg!(windows) {
            "hermes-setup.exe"
        } else {
            "hermes-setup"
        });
        fs::write(&updater, "stub").expect("stub updater should be written");

        let _home = EnvVarGuard::set("HERMES_HOME", temp.path.clone().into_os_string());

        let resolved =
            resolve_updater_binary().expect("staged updater should resolve from HERMES_HOME");

        assert_eq!(resolved, updater);
    }

    #[test]
    fn manual_update_command_uses_current_checkout_branch_for_non_main_repos() {
        let temp = TempDirGuard::new("manual-update-branch");
        init_git_repo(&temp.path);
        run_git_test(&["checkout", "-b", "feature/gui-parity"], &temp.path);

        let command = manual_update_command(&temp.path);

        assert_eq!(command, "hermes update --branch feature/gui-parity");
    }

    #[test]
    fn desktop_manual_update_command_uses_windows_powershell_for_source_checkouts() {
        let config = read_update_source_config();
        let command = desktop_manual_update_command_for(
            "windows",
            &PathBuf::from("C:\\repo\\hermes-tauri"),
            &config,
            true,
        );

        assert!(command.contains("powershell -NoProfile -ExecutionPolicy Bypass -Command"));
        assert!(command.contains("Set-Location -LiteralPath 'C:\\repo\\hermes-tauri'"));
        assert!(command.contains("git pull --ff-only"));
        assert!(command.contains("npm.cmd install"));
        assert!(command.contains("npm.cmd run tauri:build"));
    }

    #[test]
    fn desktop_manual_update_command_uses_release_page_for_packaged_builds() {
        let config = read_update_source_config();
        let command = desktop_manual_update_command_for(
            "linux",
            &PathBuf::from("/opt/Hermes"),
            &config,
            false,
        );

        assert_eq!(
            command,
            format!(
                "Open {} and reinstall the latest package for your platform.",
                DESKTOP_RELEASES_URL
            )
        );
    }

    #[test]
    fn sync_system_hermes_update_source_for_updates_origin_and_marks_non_official_source() {
        let _env_lock = lock_test_process_env();
        let temp = TempDirGuard::new("system-hermes-source-sync");
        let hermes_home = temp.path.join("home");
        let repo = temp.path.join("repo");
        fs::create_dir_all(&repo).expect("repo dir should be created");
        init_git_repo(&repo);
        run_git_test(
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/NousResearch/hermes-agent.git",
            ],
            &repo,
        );
        let _home = EnvVarGuard::set("HERMES_HOME", hermes_home.clone().into_os_string());

        let config = UpdateSourceConfig {
            agent_git_source: "gitee".to_string(),
            agent_git_custom_url: String::new(),
            python_source: "pypi".to_string(),
            python_custom_url: String::new(),
            npm_source: "npmjs".to_string(),
            npm_custom_url: String::new(),
            desktop_repo_url: DEFAULT_DESKTOP_REPO_URL.to_string(),
        };

        sync_system_hermes_update_source_for(&repo, &config).expect("source sync should succeed");

        let origin = git_stdout(&["remote", "get-url", "origin"], &repo);
        assert_eq!(origin, GITEE_AGENT_GIT_URL);
        assert!(system_hermes_skip_upstream_prompt_path().is_file());
    }

    #[test]
    fn sync_system_hermes_update_source_for_clears_skip_marker_for_official_source() {
        let _env_lock = lock_test_process_env();
        let temp = TempDirGuard::new("system-hermes-source-sync-official");
        let hermes_home = temp.path.join("home");
        let repo = temp.path.join("repo");
        fs::create_dir_all(&repo).expect("repo dir should be created");
        init_git_repo(&repo);
        run_git_test(
            &[
                "remote",
                "add",
                "origin",
                "https://gitee.com/8187735/hermes-agent.git",
            ],
            &repo,
        );
        let _home = EnvVarGuard::set("HERMES_HOME", hermes_home.clone().into_os_string());
        fs::create_dir_all(&hermes_home).expect("hermes home should be created");
        fs::write(
            system_hermes_skip_upstream_prompt_path(),
            "desktop-managed\n",
        )
        .expect("skip marker should be created");

        let config = UpdateSourceConfig {
            agent_git_source: "github".to_string(),
            agent_git_custom_url: String::new(),
            python_source: "pypi".to_string(),
            python_custom_url: String::new(),
            npm_source: "npmjs".to_string(),
            npm_custom_url: String::new(),
            desktop_repo_url: DEFAULT_DESKTOP_REPO_URL.to_string(),
        };

        sync_system_hermes_update_source_for(&repo, &config).expect("source sync should succeed");

        let origin = git_stdout(&["remote", "get-url", "origin"], &repo);
        assert_eq!(origin, DEFAULT_AGENT_GIT_URL);
        assert!(!system_hermes_skip_upstream_prompt_path().exists());
    }

    #[test]
    fn posix_update_restart_fallback_payload_marks_backend_updated() {
        let payload = posix_update_restart_fallback_payload(Some(Path::new("/tmp/Hermes.app")));

        assert_eq!(
            payload.get("ok").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            payload
                .get("backendUpdated")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            payload.get("rebuiltApp").and_then(|value| value.as_str()),
            Some("/tmp/Hermes.app")
        );
    }

    #[test]
    fn packaged_updater_status_matches_desktop_updates_overlay_contract() {
        let update_root = PathBuf::from("/tmp/hermes");
        let updater = PathBuf::from("/tmp/.hermes/hermes-setup");

        let payload = packaged_updater_status(&update_root, "main", &updater);

        assert_eq!(
            payload.get("supported").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            payload.get("reason").and_then(|value| value.as_str()),
            Some("packaged-updater")
        );
        assert_eq!(
            payload.get("branch").and_then(|value| value.as_str()),
            Some("main")
        );
        assert_eq!(
            payload.get("updater").and_then(|value| value.as_str()),
            Some("/tmp/.hermes/hermes-setup")
        );
        assert_eq!(
            payload.get("hermesRoot").and_then(|value| value.as_str()),
            Some("/tmp/hermes")
        );
    }

    #[test]
    fn context_text_action_mode_matches_desktop_menu_fallbacks() {
        let empty = ContextMenuRequest::default();
        assert_eq!(
            context_text_action_mode(&empty, false),
            ContextTextActionMode::FallbackSelectAll
        );
        assert_eq!(
            context_text_action_mode(&empty, true),
            ContextTextActionMode::None
        );

        let selection = ContextMenuRequest {
            selection_text: "selected".to_string(),
            ..Default::default()
        };
        assert_eq!(
            context_text_action_mode(&selection, false),
            ContextTextActionMode::NonEditableSelection
        );

        let editable = ContextMenuRequest {
            is_editable: true,
            ..Default::default()
        };
        assert_eq!(
            context_text_action_mode(&editable, false),
            ContextTextActionMode::Editable
        );
    }

    #[test]
    fn context_menu_spellcheck_suggestions_match_electron_rules() {
        let non_editable = ContextMenuRequest {
            misspelled_word: Some("teh".to_string()),
            dictionary_suggestions: vec!["the".to_string()],
            ..Default::default()
        };
        assert!(context_menu_spellcheck_suggestions(&non_editable).is_empty());

        let editable = ContextMenuRequest {
            is_editable: true,
            misspelled_word: Some("teh".to_string()),
            dictionary_suggestions: vec![
                "the".to_string(),
                "tech".to_string(),
                "ten".to_string(),
                "tea".to_string(),
                "Ted".to_string(),
                "then".to_string(),
            ],
            ..Default::default()
        };
        assert_eq!(
            context_menu_spellcheck_suggestions(&editable),
            vec![
                "the".to_string(),
                "tech".to_string(),
                "ten".to_string(),
                "tea".to_string(),
                "Ted".to_string(),
            ]
        );
    }

    #[test]
    fn can_open_context_image_url_blocks_data_urls() {
        assert!(can_open_context_image_url(Some(
            "https://example.com/test.png"
        )));
        assert!(can_open_context_image_url(Some("  file:///tmp/test.png  ")));
        assert!(!can_open_context_image_url(Some(
            "data:image/png;base64,AAAA"
        )));
        assert!(!can_open_context_image_url(Some("   ")));
        assert!(!can_open_context_image_url(None));
    }

    #[test]
    fn parse_context_open_target_reuses_external_open_rules() {
        let temp = TempDirGuard::new("context-open-target");
        let file_path = temp.path.join("image.png");
        fs::write(&file_path, "png").expect("file should be written");
        let file_url = file_url_for(&file_path);

        assert_eq!(
            parse_context_open_target(Some(&file_url), true).expect("file urls should be accepted"),
            OpenExternalTarget::File(file_path)
        );
        assert_eq!(
            parse_context_open_target(Some("https://example.com/test.png"), true)
                .expect("http urls should be accepted"),
            OpenExternalTarget::Url("https://example.com/test.png".to_string())
        );
        assert!(parse_context_open_target(Some("data:image/png;base64,AAAA"), true).is_none());
        assert!(parse_context_open_target(Some("not a url"), false).is_none());
    }

    #[test]
    fn expand_user_path_matches_desktop_rules() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        assert_eq!(expand_user_path("~"), home);
        assert_eq!(
            expand_user_path("~/preview/index.html"),
            home.join("preview/index.html")
        );
    }

    #[test]
    fn normalize_preview_target_impl_normalizes_local_preview_hosts_and_ports() {
        let normalized = normalize_preview_target_impl("http://0.0.0.0:4173/nested/index.html", "")
            .expect("local preview urls should normalize");

        assert_eq!(normalized.kind, "url");
        assert_eq!(normalized.label, "127.0.0.1:4173/nested/index.html");
        assert_eq!(normalized.url, "http://127.0.0.1:4173/nested/index.html");
    }

    #[test]
    fn microphone_access_action_matches_desktop_permission_states() {
        assert_eq!(
            microphone_access_action(0),
            MicrophoneAccessAction::RequestSystemPrompt
        );
        assert_eq!(
            microphone_access_action(1),
            MicrophoneAccessAction::Return(false)
        );
        assert_eq!(
            microphone_access_action(2),
            MicrophoneAccessAction::Return(false)
        );
        assert_eq!(
            microphone_access_action(3),
            MicrophoneAccessAction::Return(true)
        );
        assert_eq!(
            microphone_access_action(99),
            MicrophoneAccessAction::Return(true)
        );
    }

    #[test]
    fn filename_from_url_matches_desktop_save_image_defaults() {
        assert_eq!(
            filename_from_url(
                "https://example.com/images/Hermes%20Logo.png?download=1",
                "image.png"
            ),
            "Hermes Logo.png"
        );
        assert_eq!(
            filename_from_url("https://example.com/images/latest", "image.webp"),
            "image.webp"
        );
        assert_eq!(filename_from_url("not a url", "image.gif"), "image.gif");
    }

    #[test]
    fn sanitize_link_title_filters_known_block_pages() {
        assert_eq!(sanitize_link_title("  Example Title  "), "Example Title");
        assert!(sanitize_link_title("Just a moment...").is_empty());
        assert!(sanitize_link_title("GetYourGuide - Error").is_empty());
    }

    #[test]
    fn terminal_event_target_matches_origin_window_rules() {
        assert_eq!(terminal_event_target(Some("preview")), "preview");
        assert_eq!(terminal_event_target(Some("  ")), "main");
        assert_eq!(terminal_event_target(None), "main");
    }

    #[test]
    fn dock_tile_file_url_uses_trailing_slash_like_electron() {
        let bundle = Path::new("/Applications/Hermes.app");

        assert_eq!(
            dock_tile_file_url(bundle),
            "file:///Applications/Hermes.app/"
        );
    }

    #[test]
    fn applications_bundle_target_points_at_canonical_system_applications_copy() {
        let bundle = Path::new("/Users/demo/Downloads/Hermes.app");

        assert_eq!(
            applications_bundle_target(bundle),
            PathBuf::from("/Applications/Hermes.app")
        );
    }

    #[test]
    fn resolve_timeout_ms_matches_desktop_fallback_behavior() {
        assert_eq!(
            resolve_timeout_ms(None, DEFAULT_FETCH_TIMEOUT_MS),
            DEFAULT_FETCH_TIMEOUT_MS
        );
        assert_eq!(
            resolve_timeout_ms(Some(0), DEFAULT_FETCH_TIMEOUT_MS),
            DEFAULT_FETCH_TIMEOUT_MS
        );
        assert_eq!(
            resolve_timeout_ms(Some(7_500), DEFAULT_FETCH_TIMEOUT_MS),
            7_500
        );
    }

    #[test]
    fn parse_session_rename_request_matches_patch_session_title_updates() {
        let request = ApiRequest {
            path: "/api/sessions/session_123".to_string(),
            method: Some("PATCH".to_string()),
            body: Some(serde_json::json!({ "title": "  hello   world  " })),
            profile: None,
            timeout_ms: None,
        };

        let parsed = parse_session_rename_request(&request)
            .expect("session rename payload should be detected");

        assert_eq!(
            parsed,
            SessionRenameRequest {
                session_id: "session_123".to_string(),
                session_path: "/api/sessions/session_123".to_string(),
                title: "  hello   world  ".to_string(),
            }
        );
    }

    #[test]
    fn parse_session_rename_request_ignores_non_session_title_routes() {
        let request = ApiRequest {
            path: "/api/sessions/session_123/messages".to_string(),
            method: Some("PATCH".to_string()),
            body: Some(serde_json::json!({ "title": "hello" })),
            profile: None,
            timeout_ms: None,
        };

        assert!(parse_session_rename_request(&request).is_none());
    }

    #[test]
    fn local_dashboard_command_args_do_not_pass_invalid_subcommand_flags() {
        assert_eq!(
            local_dashboard_command_args(9120),
            vec![
                "dashboard".to_string(),
                "--no-open".to_string(),
                "--skip-build".to_string(),
                "--host".to_string(),
                "127.0.0.1".to_string(),
                "--port".to_string(),
                "9120".to_string(),
            ]
        );
    }

    #[test]
    fn desktop_openapi_has_required_routes_checks_audio_and_session_patch_support() {
        let compatible = serde_json::json!({
            "paths": {
                "/api/audio/transcribe": { "post": {} },
                "/api/audio/speak": { "post": {} },
                "/api/sessions/{session_id}": { "patch": {} }
            }
        });
        assert!(desktop_openapi_has_required_routes(&compatible));

        let missing_audio = serde_json::json!({
            "paths": {
                "/api/audio/transcribe": { "post": {} },
                "/api/sessions/{session_id}": { "patch": {} }
            }
        });
        assert!(!desktop_openapi_has_required_routes(&missing_audio));
    }

    #[test]
    fn rename_title_fallback_collapses_whitespace_like_cli_storage() {
        assert_eq!(rename_title_fallback(""), "");
        assert_eq!(rename_title_fallback("   "), "");
        assert_eq!(rename_title_fallback("  hello   world  "), "hello world");
        assert_eq!(
            rename_title_fallback("line 1\nline 2\tline 3"),
            "line 1 line 2 line 3"
        );
    }

    #[test]
    fn parse_hermes_api_response_rejects_html_success_payloads() {
        let error = parse_hermes_api_response(
            "http://127.0.0.1:9120/api/status",
            reqwest::StatusCode::OK,
            Some("text/html"),
            "<!doctype html><html><body>missing</body></html>",
        )
        .expect_err("html success payloads should be rejected");

        assert_eq!(
            error,
            "Expected JSON from http://127.0.0.1:9120/api/status but got HTML (status 200). The endpoint is likely missing on the Hermes backend."
        );
    }

    #[test]
    fn parse_hermes_api_response_uses_status_reason_when_error_body_is_empty() {
        let error = parse_hermes_api_response(
            "http://127.0.0.1:9120/api/status",
            reqwest::StatusCode::NOT_FOUND,
            None,
            "",
        )
        .expect_err("error responses should be rejected");

        assert_eq!(error, "404: Not Found");
    }

    #[test]
    fn parse_hermes_api_response_returns_null_for_empty_success_bodies() {
        let value = parse_hermes_api_response(
            "http://127.0.0.1:9120/api/status",
            reqwest::StatusCode::NO_CONTENT,
            None,
            "",
        )
        .expect("empty success responses should resolve to null");

        assert_eq!(value, serde_json::Value::Null);
    }

    #[test]
    fn parse_titlebar_theme_payload_accepts_valid_hex_colors_only() {
        let valid = parse_titlebar_theme_payload(&serde_json::json!({
            "background": "#111111",
            "foreground": "#F7F7F7",
        }))
        .expect("valid hex colors should parse");
        assert_eq!(valid.background, "#111111");
        assert_eq!(valid.foreground, "#F7F7F7");

        assert!(
            parse_titlebar_theme_payload(&serde_json::json!({
                "background": "rgb(0,0,0)",
                "foreground": "#ffffff",
            }))
            .is_none()
        );
        assert!(
            parse_titlebar_theme_payload(&serde_json::json!({
                "background": "#111111",
            }))
            .is_none()
        );
    }

    #[test]
    fn titlebar_window_theme_tracks_overlay_contrast() {
        assert_eq!(
            titlebar_window_theme(&TitlebarThemePayload {
                background: "#111111".to_string(),
                foreground: "#f7f7f7".to_string(),
            }),
            tauri::Theme::Dark
        );

        assert_eq!(
            titlebar_window_theme(&TitlebarThemePayload {
                background: "#f7f7f7".to_string(),
                foreground: "#242424".to_string(),
            }),
            tauri::Theme::Light
        );
    }

    #[test]
    fn reveal_path_command_matches_platform_conventions() {
        let path = Path::new("/tmp/hermes/logs/desktop.log");
        let command = reveal_path_command(path);

        #[cfg(target_os = "macos")]
        assert_eq!(
            command,
            Some((
                "open".to_string(),
                vec!["-R".to_string(), path.to_string_lossy().to_string()],
            ))
        );

        #[cfg(target_os = "windows")]
        assert_eq!(
            command,
            Some((
                "explorer".to_string(),
                vec![format!("/select,{}", path.display())],
            ))
        );

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(command, None);
    }
}
