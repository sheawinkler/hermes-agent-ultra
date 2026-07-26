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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingDiscordMessage {
    pub channel_id: String,
    pub channel_type: Option<u8>,
    pub guild_id: Option<String>,
    pub parent_channel_id: Option<String>,
    pub thread_id: Option<String>,
    pub is_dm: bool,
    pub message_id: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub content: String,
    pub is_bot: bool,
    /// Role IDs from the authenticated gateway event's guild-member object.
    pub member_role_ids: Vec<String>,
    pub message_type: u8,
    pub mention_user_ids: Vec<String>,
    pub reply_to_message_id: Option<String>,
    pub reply_to_text: Option<String>,
    pub reply_to_attachments: Vec<DiscordIncomingAttachment>,
    pub attachments: Vec<DiscordIncomingAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    pub fn channel_context(&self) -> DiscordChannelContext {
        DiscordChannelContext {
            channel_id: self.channel_id.clone(),
            parent_channel_id: self.parent_channel_id.clone(),
            is_dm: self.is_dm,
            is_thread: self.thread_id.is_some(),
            is_reply: self.reply_to_message_id.is_some(),
            voice_linked_text_channel: false,
        }
    }

    fn gateway_text(&self) -> String {
        use std::fmt::Write as _;

        fn append_attachment(
            text: &mut String,
            label: &str,
            attachment: &DiscordIncomingAttachment,
        ) {
            let _ = write!(text, "[{label}");
            if !attachment.filename.trim().is_empty() {
                let _ = write!(text, ": {}", attachment.filename.trim());
            }
            text.push(']');
            if let Some(content_type) = attachment
                .content_type
                .as_deref()
                .map(str::trim)
                .filter(|content_type| !content_type.is_empty())
            {
                let _ = write!(text, " ({content_type}");
                if let Some(size) = attachment.size {
                    let _ = write!(text, ", {size} bytes");
                }
                text.push(')');
            } else if let Some(size) = attachment.size {
                let _ = write!(text, " ({size} bytes)");
            }
            text.push('\n');
            text.push_str(&attachment.url);
        }

        let mut text = self.content.clone();
        let reply_text = self
            .reply_to_text
            .as_deref()
            .map(str::trim)
            .filter(|reply| !reply.is_empty());
        if reply_text.is_some() || !self.reply_to_attachments.is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str("[Discord reply context]");
            if let Some(reply) = reply_text {
                text.push('\n');
                text.push_str(reply);
            }
            for attachment in &self.reply_to_attachments {
                text.push_str("\n\n");
                append_attachment(&mut text, "Referenced Discord attachment", attachment);
            }
        }
        for attachment in &self.attachments {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            append_attachment(&mut text, "Discord attachment", attachment);
        }
        text
    }

    /// Convert a verified Discord message into the platform-neutral gateway shape.
    ///
    /// A missing Discord author ID is never routeable: it cannot be authorized
    /// or attributed to a human/bot sender.
    pub fn to_gateway_message(&self) -> Option<crate::gateway::IncomingMessage> {
        let user_id = self
            .user_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())?
            .to_string();
        let chat_id = self
            .parent_channel_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .unwrap_or(self.channel_id.trim())
            .to_string();
        if chat_id.is_empty() {
            return None;
        }

        Some(crate::gateway::IncomingMessage {
            platform: "discord".to_string(),
            chat_id,
            user_id,
            text: self.gateway_text(),
            message_id: Some(self.message_id.clone()),
            thread_id: self.thread_id.clone(),
            is_dm: self.is_dm,
        })
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

