/// Discord Message object.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordMessage {
    pub id: String,
    pub channel_id: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub author: Option<DiscordUser>,
}

/// Discord User object.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordUser {
    pub id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub bot: Option<bool>,
}

/// Incoming message parsed from a Discord MESSAGE_CREATE event.
#[derive(Debug, Clone)]
pub struct IncomingDiscordMessage {
    pub channel_id: String,
    pub message_id: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub content: String,
    pub is_bot: bool,
    pub message_type: u8,
    pub mention_user_ids: Vec<String>,
    pub reply_to_message_id: Option<String>,
    pub reply_to_text: Option<String>,
    pub attachments: Vec<DiscordIncomingAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordIncomingAttachment {
    pub filename: String,
    pub url: String,
    pub content_type: Option<String>,
    pub size: Option<u64>,
}

impl IncomingDiscordMessage {
    pub fn mentions_user(&self, user_id: &str) -> bool {
        let needle = user_id.trim();
        !needle.is_empty()
            && self
                .mention_user_ids
                .iter()
                .any(|mentioned| mentioned.trim() == needle)
    }
}

fn parse_discord_incoming_attachments(
    value: Option<&serde_json::Value>,
) -> Vec<DiscordIncomingAttachment> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let url = item.get("url")?.as_str()?.to_string();
                    let filename = item
                        .get("filename")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(DiscordIncomingAttachment {
                        filename,
                        url,
                        content_type: item
                            .get("content_type")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        size: item.get("size").and_then(|v| v.as_u64()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Event types: MESSAGE_UPDATE
// ---------------------------------------------------------------------------

/// Parsed data from a `MESSAGE_UPDATE` dispatch event.
///
/// Discord may send partial updates — only `id` and `channel_id` are
/// guaranteed; other fields are optional.
#[derive(Debug, Clone)]
pub struct MessageUpdateEvent {
    pub channel_id: String,
    pub message_id: String,
    pub content: Option<String>,
    pub author_id: Option<String>,
    pub guild_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Event types: INTERACTION_CREATE (slash commands)
// ---------------------------------------------------------------------------

/// Parsed interaction from `INTERACTION_CREATE`.
#[derive(Debug, Clone)]
pub struct InteractionData {
    pub id: String,
    pub application_id: String,
    /// Interaction type (2 = APPLICATION_COMMAND, 3 = MESSAGE_COMPONENT, …).
    pub interaction_type: u8,
    pub token: String,
    pub channel_id: Option<String>,
    pub guild_id: Option<String>,
    pub user_id: Option<String>,
    pub command_name: Option<String>,
    pub command_options: Vec<InteractionOption>,
}

/// A single option supplied to a slash command invocation.
#[derive(Debug, Clone)]
pub struct InteractionOption {
    pub name: String,
    pub value: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Event types: Reactions
// ---------------------------------------------------------------------------

/// Parsed data from `MESSAGE_REACTION_ADD` / `MESSAGE_REACTION_REMOVE`.
#[derive(Debug, Clone)]
pub struct ReactionEvent {
    pub user_id: String,
    pub channel_id: String,
    pub message_id: String,
    pub guild_id: Option<String>,
    pub emoji_name: Option<String>,
    pub emoji_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Event types: Voice state
// ---------------------------------------------------------------------------

/// Parsed `VOICE_STATE_UPDATE` event.
#[derive(Debug, Clone)]
pub struct VoiceState {
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub user_id: String,
    pub session_id: String,
    pub deaf: bool,
    pub mute: bool,
    pub self_deaf: bool,
    pub self_mute: bool,
    pub suppress: bool,
}

// ---------------------------------------------------------------------------
// Slash command registration types
// ---------------------------------------------------------------------------

/// Definition of a slash command to register with Discord.
#[derive(Debug, Clone, Serialize)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SlashCommandOption>>,
    /// Discord permission bitset string. "0" hides the command by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_member_permissions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dm_permission: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nsfw: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contexts: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_types: Option<Vec<u8>>,
    /// Command type (1 = CHAT_INPUT, 2 = USER, 3 = MESSAGE). Default 1.
    #[serde(rename = "type", default = "default_command_type")]
    pub command_type: u8,
}

/// A single option for a slash command.
#[derive(Debug, Clone, Serialize)]
pub struct SlashCommandOption {
    pub name: String,
    pub description: String,
    /// Option type (3 = STRING, 4 = INTEGER, 5 = BOOLEAN, 6 = USER, …).
    #[serde(rename = "type")]
    pub option_type: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<SlashCommandChoice>>,
}

/// A predefined choice for a slash command option.
#[derive(Debug, Clone, Serialize)]
pub struct SlashCommandChoice {
    pub name: String,
    pub value: serde_json::Value,
}

pub fn apply_owner_only_slash_visibility(commands: &mut [SlashCommand]) {
    for command in commands {
        command.default_member_permissions = Some("0".to_string());
    }
}

// ---------------------------------------------------------------------------
// Embed types
// ---------------------------------------------------------------------------

/// A Discord rich embed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordEmbed {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer: Option<EmbedFooter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<EmbedMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<EmbedMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<EmbedAuthor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<EmbedField>,
}

impl DiscordEmbed {
    pub fn new() -> Self {
        Self {
            title: None,
            description: None,
            url: None,
            color: None,
            timestamp: None,
            footer: None,
            image: None,
            thumbnail: None,
            author: None,
            fields: Vec::new(),
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    pub fn with_color(mut self, color: u32) -> Self {
        self.color = Some(color);
        self
    }

    pub fn with_footer(mut self, text: impl Into<String>) -> Self {
        self.footer = Some(EmbedFooter {
            text: text.into(),
            icon_url: None,
        });
        self
    }

    pub fn with_timestamp(mut self, ts: impl Into<String>) -> Self {
        self.timestamp = Some(ts.into());
        self
    }

    pub fn add_field(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
        inline: bool,
    ) -> Self {
        self.fields.push(EmbedField {
            name: name.into(),
            value: value.into(),
            inline: Some(inline),
        });
        self
    }
}

impl Default for DiscordEmbed {
    fn default() -> Self {
        Self::new()
    }
}

/// Embed footer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedFooter {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

/// Embed media (image / thumbnail).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedMedia {
    pub url: String,
}

/// Embed author.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedAuthor {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

/// A single field in an embed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline: Option<bool>,
}

// ---------------------------------------------------------------------------
// Thread creation result
// ---------------------------------------------------------------------------

/// Response from creating a thread.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordThread {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub thread_type: Option<u8>,
    pub guild_id: Option<String>,
    pub parent_id: Option<String>,
}

/// Response from creating a Discord forum post thread.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordForumThread {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub message: Option<DiscordMessage>,
}

/// Result of a forum post send where follow-up chunk failures are non-fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordForumSendOutcome {
    pub thread_id: String,
    pub message_id: String,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// DiscordAdapter
// ---------------------------------------------------------------------------

