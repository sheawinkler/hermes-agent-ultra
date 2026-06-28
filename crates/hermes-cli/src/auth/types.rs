pub const DEFAULT_NOUS_PORTAL_URL: &str = "https://portal.nousresearch.com";
pub const DEFAULT_NOUS_INFERENCE_URL: &str = "https://inference-api.nousresearch.com/v1";
pub const DEFAULT_NOUS_CLIENT_ID: &str = "hermes-cli";
pub const DEFAULT_NOUS_SCOPE: &str = NOUS_INFERENCE_INVOKE_SCOPE;
pub const NOUS_INFERENCE_INVOKE_SCOPE: &str = "inference:invoke";
pub const NOUS_AUTH_PATH_INVOKE_JWT: &str = "invoke_jwt";
pub const DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS: u32 = 30 * 60;
pub const NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS: i64 = 120;
const NOUS_DEVICE_AUTH_POLL_INTERVAL_CAP_SECONDS: u64 = 1;

pub const DEFAULT_CODEX_ISSUER: &str = "https://auth.openai.com";
pub const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const ANTHROPIC_OAUTH_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const ANTHROPIC_OAUTH_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
pub const ANTHROPIC_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const ANTHROPIC_OAUTH_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
pub const ANTHROPIC_OAUTH_SCOPE: &str = "org:create_api_key user:profile user:inference";
pub const ANTHROPIC_OAUTH_ACCESS_TOKEN_REFRESH_SKEW_SECONDS: i64 = 60;
const ANTHROPIC_CLAUDE_CODE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const ANTHROPIC_CLAUDE_CODE_KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_QWEN_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";
pub const QWEN_OAUTH_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";
pub const QWEN_OAUTH_TOKEN_URL: &str = "https://chat.qwen.ai/api/v1/oauth2/token";
pub const QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS: i64 = 120;
pub const DEFAULT_GEMINI_CLOUDCODE_BASE_URL: &str = "cloudcode-pa://google";
pub const GEMINI_OAUTH_ACCESS_TOKEN_REFRESH_SKEW_SECONDS: i64 = 60;
const GEMINI_AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GEMINI_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const GEMINI_USERINFO_ENDPOINT: &str = "https://www.googleapis.com/oauth2/v1/userinfo";
const GEMINI_OAUTH_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile";
const GEMINI_CALLBACK_HOST: &str = "127.0.0.1";
const GEMINI_CALLBACK_PORT: u16 = 8085;
const GEMINI_CALLBACK_PATH: &str = "/oauth2callback";
const GEMINI_CALLBACK_WAIT_SECS: u64 = 300;
const DEFAULT_GEMINI_CLIENT_ID_PROJECT_NUM: &str = "681255809395";
const DEFAULT_GEMINI_CLIENT_ID_HASH: &str = "oo8ft2oprdrnp9e3aqf6av3hmdib135j";
const DEFAULT_GEMINI_CLIENT_SECRET_SUFFIX: &str = "4uHgMPm-1o7Sk-geV6Cu5clXFsxl";

#[derive(Debug, Clone)]
pub struct NousDeviceCodeOptions {
    pub portal_base_url: Option<String>,
    pub inference_base_url: Option<String>,
    pub client_id: Option<String>,
    pub scope: Option<String>,
    pub open_browser: bool,
    pub timeout_seconds: f64,
    pub min_key_ttl_seconds: u32,
}

impl Default for NousDeviceCodeOptions {
    fn default() -> Self {
        Self {
            portal_base_url: None,
            inference_base_url: None,
            client_id: None,
            scope: None,
            open_browser: true,
            timeout_seconds: 15.0,
            min_key_ttl_seconds: DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodexDeviceCodeOptions {
    pub open_browser: bool,
    pub timeout_seconds: f64,
}

impl Default for CodexDeviceCodeOptions {
    fn default() -> Self {
        Self {
            open_browser: true,
            timeout_seconds: 15.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NousAuthState {
    pub portal_base_url: String,
    pub inference_base_url: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub token_type: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub obtained_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key_expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key_expires_in: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key_reused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key_obtained_at: Option<String>,
}

impl NousAuthState {
    pub fn runtime_api_key(&self) -> Option<String> {
        if let Some(agent_key) = self
            .agent_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(agent_key.to_string());
        }
        let access = self.access_token.trim();
        if access.is_empty() {
            None
        } else {
            Some(access.to_string())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexAuthState {
    pub tokens: CodexTokens,
    pub base_url: String,
    pub last_refresh: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct OpenAiOAuthImport {
    pub state: CodexAuthState,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct AnthropicOAuthImport {
    pub state: AnthropicOAuthState,
    pub source_path: PathBuf,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct NousOAuthImport {
    pub state: NousAuthState,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct NousRuntimeCredentials {
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub key_id: Option<String>,
    pub expires_at: Option<String>,
    pub expires_in: Option<i64>,
    pub source: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExternalOpenAiAuthFile {
    #[serde(default)]
    auth_mode: Option<String>,
    #[serde(default)]
    last_refresh: Option<String>,
    #[serde(default)]
    tokens: Option<ExternalOpenAiAuthTokens>,
}

#[derive(Debug, Deserialize)]
struct ExternalOpenAiAuthTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExternalClaudeCredentialsFile {
    #[serde(default, rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ExternalClaudeOauthState>,
}

#[derive(Debug, Deserialize)]
struct ExternalClaudeOauthState {
    #[serde(default, rename = "accessToken")]
    access_token: Option<String>,
    #[serde(default, rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(default, rename = "expiresAt")]
    expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthStore {
    #[serde(default = "default_auth_store_version")]
    version: u32,
    #[serde(default)]
    providers: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

const fn default_auth_store_version() -> u32 {
    1
}

impl Default for AuthStore {
    fn default() -> Self {
        Self {
            version: default_auth_store_version(),
            providers: BTreeMap::new(),
            active_provider: None,
            updated_at: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct NousDeviceCodeResponse {
    device_code: Option<String>,
    user_code: Option<String>,
    verification_uri: Option<String>,
    verification_uri_complete: Option<String>,
    expires_in: Option<i64>,
    interval: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct NousTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    token_type: Option<String>,
    scope: Option<String>,
    expires_in: Option<i64>,
    inference_base_url: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexDeviceUserCodeResponse {
    user_code: Option<String>,
    device_auth_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    interval: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CodexDevicePollResponse {
    authorization_code: Option<String>,
    code_verifier: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    expires_in: Option<i64>,
}

fn deserialize_optional_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::Number(number) => number
            .as_i64()
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("expected integer-compatible number")),
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                trimmed
                    .parse::<i64>()
                    .map(Some)
                    .map_err(serde::de::Error::custom)
            }
        }
        other => Err(serde::de::Error::custom(format!(
            "expected integer or numeric string, got {other}"
        ))),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QwenCliTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub resource_url: String,
    pub expiry_date: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct QwenRuntimeCredentials {
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub source: String,
    pub expires_at_ms: Option<i64>,
    pub auth_file: PathBuf,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub tokens: QwenCliTokens,
}

#[derive(Debug, Clone)]
pub struct QwenAuthStatus {
    pub logged_in: bool,
    pub auth_file: PathBuf,
    pub source: Option<String>,
    pub api_key: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GeminiOAuthLoginOptions {
    pub open_browser: bool,
    pub timeout_seconds: f64,
}

impl Default for GeminiOAuthLoginOptions {
    fn default() -> Self {
        Self {
            open_browser: true,
            timeout_seconds: 20.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiOAuthFileState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    managed_project_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AnthropicOAuthLoginOptions {
    pub open_browser: bool,
    pub timeout_seconds: f64,
}

impl Default for AnthropicOAuthLoginOptions {
    fn default() -> Self {
        Self {
            open_browser: true,
            timeout_seconds: 20.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicOAuthState {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct GeminiRuntimeCredentials {
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub source: String,
    pub expires_at_ms: Option<i64>,
    pub auth_file: PathBuf,
    pub email: Option<String>,
    pub project_id: Option<String>,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GeminiOAuthStatus {
    pub logged_in: bool,
    pub auth_file: PathBuf,
    pub source: Option<String>,
    pub api_key: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub email: Option<String>,
    pub project_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AnthropicOAuthStatus {
    pub logged_in: bool,
    pub source: Option<String>,
    pub api_key: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub error: Option<String>,
}
