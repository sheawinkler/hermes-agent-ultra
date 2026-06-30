fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// SlackConfig
// ---------------------------------------------------------------------------

/// Configuration for the Slack adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    /// Slack bot token (xoxb-...).
    pub token: String,

    /// Slack app-level token for socket mode (xapp-...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_token: Option<String>,

    /// Whether to use Socket Mode for receiving events.
    #[serde(default)]
    pub socket_mode: bool,

    /// Whether reaction lifecycle updates are enabled.
    #[serde(default = "default_true")]
    pub reactions: bool,

    /// Whether non-DM channel messages must mention or wake-word address the bot.
    #[serde(default)]
    pub require_mention: bool,

    /// Optional Slack bot user id used for literal `<@BOTID>` mention checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_user_id: Option<String>,

    /// Extra regex wake words accepted when `require_mention` is enabled.
    #[serde(default)]
    pub mention_patterns: Vec<String>,

    /// Proxy configuration for outbound requests.
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

// ---------------------------------------------------------------------------
// Slack API types
// ---------------------------------------------------------------------------

/// Generic Slack API response.
#[derive(Debug, Deserialize)]
pub struct SlackResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub ts: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackAuthTestResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackConversationsResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub channels: Vec<SlackConversation>,
    #[serde(default)]
    pub response_metadata: SlackResponseMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct SlackResponseMetadata {
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackConversation {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_private: bool,
}

/// Response for `users.info`.
#[derive(Debug, Deserialize)]
pub struct UserInfoResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub user: Option<SlackUser>,
}

/// Slack user profile data.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SlackUser {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub real_name: Option<String>,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default)]
    pub tz: Option<String>,
    #[serde(default)]
    pub profile: Option<SlackUserProfile>,
}

/// Subset of `users.info` profile fields.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SlackUserProfile {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub image_72: Option<String>,
}

/// Response for `chat.getPermalink`.
#[derive(Debug, Deserialize)]
pub struct PermalinkResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub permalink: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
}

/// Slack Socket Mode hello event.
#[derive(Debug, Deserialize)]
pub struct SocketModeHello {
    #[serde(rename = "type")]
    pub event_type: String,
}

/// Slack Socket Mode envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct SocketModeEnvelope {
    #[serde(rename = "type")]
    pub envelope_type: String,
    #[serde(default)]
    pub envelope_id: Option<String>,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
}

/// Slack event payload (from Events API / Socket Mode).
#[derive(Debug, Clone, Deserialize)]
pub struct SlackEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub ts: Option<String>,
    #[serde(default)]
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub bot_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlackMediaKind {
    Audio,
    Video,
    Image,
    Document,
    Unsupported,
}

/// Slack file attachment metadata preserved by the Socket Mode parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackMediaFile {
    pub id: Option<String>,
    pub name: Option<String>,
    pub mimetype: Option<String>,
    pub subtype: Option<String>,
    pub url_private: Option<String>,
    pub url_private_download: Option<String>,
    pub kind: SlackMediaKind,
    pub cache_extension: Option<String>,
    pub reported_mime_type: Option<String>,
}

impl SlackMediaFile {
    pub fn download_url(&self) -> Option<&str> {
        self.url_private_download
            .as_deref()
            .or(self.url_private.as_deref())
    }
}

/// Incoming message parsed from a Slack event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingSlackMessage {
    pub channel: String,
    pub user_id: Option<String>,
    pub text: String,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub is_bot: bool,
    pub media_files: Vec<SlackMediaFile>,
}

/// Token-free mention policy used by Socket Mode routing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlackMentionPolicy {
    pub require_mention: bool,
    pub bot_user_id: Option<String>,
    pub mention_patterns: Vec<String>,
}

impl SlackMentionPolicy {
    fn from_config(config: &SlackConfig) -> Self {
        Self {
            require_mention: config.require_mention,
            bot_user_id: config.bot_user_id.clone(),
            mention_patterns: config.mention_patterns.clone(),
        }
    }
}
