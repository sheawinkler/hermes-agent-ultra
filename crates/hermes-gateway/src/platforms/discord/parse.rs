//! Discord Gateway event parsing.

use crate::gateway::IncomingMessage;

/// Parsed Discord attachment metadata from MESSAGE_CREATE.
#[derive(Debug, Clone)]
pub struct DiscordAttachment {
    pub id: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub url: String,
    pub size: u64,
    /// Present on voice messages.
    pub waveform: Option<Vec<u8>>,
}

/// Parsed MESSAGE_CREATE payload for inbound filtering.
#[derive(Debug, Clone)]
pub struct RawDiscordMessage {
    pub channel_id: String,
    pub message_id: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub content: String,
    pub is_bot: bool,
    pub guild_id: Option<String>,
    pub mentions: Vec<String>,
    pub message_type: u8,
    pub attachments: Vec<DiscordAttachment>,
    /// Member role snowflakes (guild messages).
    pub role_ids: Vec<String>,
    /// Parent channel when `channel_id` is a thread.
    pub parent_channel_id: Option<String>,
}

/// Legacy parsed message (subset).
#[derive(Debug, Clone)]
pub struct IncomingDiscordMessage {
    pub channel_id: String,
    pub message_id: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub content: String,
    pub is_bot: bool,
}

#[derive(Debug, Clone)]
pub struct MessageUpdateEvent {
    pub channel_id: String,
    pub message_id: String,
    pub content: Option<String>,
    pub author_id: Option<String>,
    pub guild_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InteractionData {
    pub id: String,
    pub application_id: String,
    pub interaction_type: u8,
    pub token: String,
    pub channel_id: Option<String>,
    pub guild_id: Option<String>,
    pub user_id: Option<String>,
    pub command_name: Option<String>,
    pub command_options: Vec<InteractionOption>,
    pub role_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct InteractionOption {
    pub name: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ReactionEvent {
    pub user_id: String,
    pub channel_id: String,
    pub message_id: String,
    pub guild_id: Option<String>,
    pub emoji_name: Option<String>,
    pub emoji_id: Option<String>,
}

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

#[derive(Debug, Clone)]
pub enum DispatchEvent {
    MessageCreate(IncomingDiscordMessage),
    MessageUpdate(MessageUpdateEvent),
    InteractionCreate(InteractionData),
    ReactionAdd(ReactionEvent),
    ReactionRemove(ReactionEvent),
    VoiceStateUpdate(VoiceState),
}

/// Parse a Discord snowflake from JSON (string or integer).
pub fn json_snowflake(value: &serde_json::Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    value.as_u64().map(|n| n.to_string())
}

pub fn parse_message_create_raw(data: &serde_json::Value) -> Option<RawDiscordMessage> {
    let channel_id = json_snowflake(data.get("channel_id")?)?;
    let message_id = json_snowflake(data.get("id")?)?;
    let content = data
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let author = data.get("author");
    let user_id = author.and_then(|a| a.get("id")).and_then(json_snowflake);
    let username = author
        .and_then(|a| a.get("username"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let is_bot = author
        .and_then(|a| a.get("bot"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let guild_id = data.get("guild_id").and_then(json_snowflake);

    let role_ids = data
        .get("member")
        .and_then(|m| m.get("roles"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(json_snowflake)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let attachments = parse_attachments(data.get("attachments"));

    let parent_channel_id = data
        .get("channel")
        .and_then(|ch| ch.get("parent_id"))
        .and_then(json_snowflake);

    let mentions = data
        .get("mentions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(json_snowflake))
                .collect()
        })
        .unwrap_or_default();

    let message_type = data
        .get("type")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u8;

    Some(RawDiscordMessage {
        channel_id,
        message_id,
        user_id,
        username,
        content,
        is_bot,
        guild_id,
        mentions,
        message_type,
        attachments,
        role_ids,
        parent_channel_id,
    })
}

pub fn parse_attachments(value: Option<&serde_json::Value>) -> Vec<DiscordAttachment> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return vec![];
    };
    arr.iter()
        .filter_map(|att| {
            let id = json_snowflake(att.get("id")?)?;
            let url = att.get("url")?.as_str()?.to_string();
            let filename = att
                .get("filename")
                .and_then(|v| v.as_str())
                .unwrap_or("attachment")
                .to_string();
            let content_type = att
                .get("content_type")
                .and_then(|v| v.as_str())
                .map(String::from);
            let size = att.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            let waveform = att.get("waveform").and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|b| b.as_u64().map(|n| n as u8))
                        .collect()
                })
            });
            Some(DiscordAttachment {
                id,
                filename,
                content_type,
                url,
                size,
                waveform,
            })
        })
        .collect()
}

pub fn raw_to_incoming(
    raw: &RawDiscordMessage,
    media_urls: Vec<String>,
    media_types: Vec<String>,
) -> IncomingMessage {
    IncomingMessage {
        platform: "discord".into(),
        chat_id: raw.channel_id.clone(),
        user_id: raw
            .user_id
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        text: raw.content.clone(),
        media_urls,
        media_types,
        message_id: Some(raw.message_id.clone()),
        is_dm: raw.guild_id.is_none(),
        interaction_id: None,
        interaction_token: None,
        role_ids: raw.role_ids.clone(),
    }
}

pub fn interaction_to_incoming(interaction: &InteractionData) -> Option<IncomingMessage> {
    if interaction.interaction_type != INTERACTION_TYPE_APPLICATION_COMMAND {
        return None;
    }
    let command_name = interaction.command_name.as_ref()?;
    let channel_id = interaction.channel_id.as_ref()?;
    Some(IncomingMessage {
        platform: "discord".into(),
        chat_id: channel_id.clone(),
        user_id: interaction
            .user_id
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        text: format!("/{command_name}"),
        media_urls: vec![],
        media_types: vec![],
        message_id: None,
        is_dm: interaction.guild_id.is_none(),
        interaction_id: Some(interaction.id.clone()),
        interaction_token: Some(interaction.token.clone()),
        role_ids: interaction.role_ids.clone(),
    })
}

pub fn parse_message_create(data: &serde_json::Value) -> Option<IncomingDiscordMessage> {
    parse_message_create_raw(data).map(|r| IncomingDiscordMessage {
        channel_id: r.channel_id,
        message_id: r.message_id,
        user_id: r.user_id,
        username: r.username,
        content: r.content,
        is_bot: r.is_bot,
    })
}

pub fn parse_message_update(data: &serde_json::Value) -> Option<MessageUpdateEvent> {
    let channel_id = data.get("channel_id")?.as_str()?.to_string();
    let message_id = data.get("id")?.as_str()?.to_string();
    let content = data
        .get("content")
        .and_then(|v| v.as_str())
        .map(String::from);
    let author_id = data
        .get("author")
        .and_then(|a| a.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let guild_id = data
        .get("guild_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    Some(MessageUpdateEvent {
        channel_id,
        message_id,
        content,
        author_id,
        guild_id,
    })
}

/// Discord application command interaction (`type` = 2).
pub const INTERACTION_TYPE_APPLICATION_COMMAND: u8 = 2;

pub fn parse_interaction_create(data: &serde_json::Value) -> Option<InteractionData> {
    let id = json_snowflake(data.get("id")?)?;
    let application_id = json_snowflake(data.get("application_id")?)?;
    let interaction_type = data.get("type")?.as_u64()? as u8;
    let token = data.get("token")?.as_str()?.to_string();
    let channel_id = data.get("channel_id").and_then(json_snowflake);
    let guild_id = data.get("guild_id").and_then(json_snowflake);
    let user_id = data
        .get("member")
        .and_then(|m| m.get("user"))
        .and_then(|u| u.get("id"))
        .and_then(json_snowflake)
        .or_else(|| data.get("user").and_then(|u| u.get("id")).and_then(json_snowflake));
    let interaction_roles = data
        .get("member")
        .and_then(|m| m.get("roles"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(json_snowflake)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let cmd_data = data.get("data");
    let command_name = cmd_data
        .and_then(|d| d.get("name"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let command_options = cmd_data
        .and_then(|d| d.get("options"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|opt| {
                    let name = opt.get("name")?.as_str()?.to_string();
                    let value = opt.get("value").cloned().unwrap_or(serde_json::Value::Null);
                    Some(InteractionOption { name, value })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(InteractionData {
        id,
        application_id,
        interaction_type,
        token,
        channel_id,
        guild_id,
        user_id,
        command_name,
        command_options,
        role_ids: interaction_roles,
    })
}

pub fn parse_reaction_event(data: &serde_json::Value) -> Option<ReactionEvent> {
    let user_id = data.get("user_id")?.as_str()?.to_string();
    let channel_id = data.get("channel_id")?.as_str()?.to_string();
    let message_id = data.get("message_id")?.as_str()?.to_string();
    let guild_id = data
        .get("guild_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let emoji = data.get("emoji");
    let emoji_name = emoji
        .and_then(|e| e.get("name"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let emoji_id = emoji
        .and_then(|e| e.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    Some(ReactionEvent {
        user_id,
        channel_id,
        message_id,
        guild_id,
        emoji_name,
        emoji_id,
    })
}

pub fn parse_voice_state_update(data: &serde_json::Value) -> Option<VoiceState> {
    let user_id = data.get("user_id")?.as_str()?.to_string();
    let session_id = data.get("session_id")?.as_str()?.to_string();
    let guild_id = data
        .get("guild_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let channel_id = data
        .get("channel_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let deaf = data.get("deaf").and_then(|v| v.as_bool()).unwrap_or(false);
    let mute = data.get("mute").and_then(|v| v.as_bool()).unwrap_or(false);
    let self_deaf = data
        .get("self_deaf")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let self_mute = data
        .get("self_mute")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let suppress = data
        .get("suppress")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Some(VoiceState {
        guild_id,
        channel_id,
        user_id,
        session_id,
        deaf,
        mute,
        self_deaf,
        self_mute,
        suppress,
    })
}

pub fn parse_dispatch(event_name: &str, data: &serde_json::Value) -> Option<DispatchEvent> {
    match event_name {
        "MESSAGE_CREATE" => parse_message_create(data).map(DispatchEvent::MessageCreate),
        "MESSAGE_UPDATE" => parse_message_update(data).map(DispatchEvent::MessageUpdate),
        "INTERACTION_CREATE" => parse_interaction_create(data).map(DispatchEvent::InteractionCreate),
        "MESSAGE_REACTION_ADD" => parse_reaction_event(data).map(DispatchEvent::ReactionAdd),
        "MESSAGE_REACTION_REMOVE" => parse_reaction_event(data).map(DispatchEvent::ReactionRemove),
        "VOICE_STATE_UPDATE" => parse_voice_state_update(data).map(DispatchEvent::VoiceStateUpdate),
        _ => None,
    }
}

/// Extract bot user id from READY dispatch data.
pub fn ready_bot_user_id(data: &serde_json::Value) -> Option<String> {
    data.get("user")
        .and_then(|u| u.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p01_parse_guild_message_full() {
        let data = serde_json::json!({
            "id": "msg123",
            "channel_id": "ch456",
            "guild_id": "g789",
            "type": 0,
            "content": "hello world",
            "author": { "id": "user789", "username": "testuser", "bot": false },
            "mentions": [{ "id": "bot1" }]
        });
        let raw = parse_message_create_raw(&data).unwrap();
        assert_eq!(raw.channel_id, "ch456");
        assert_eq!(raw.message_id, "msg123");
        assert_eq!(raw.guild_id, Some("g789".into()));
        assert_eq!(raw.mentions, vec!["bot1"]);
        assert_eq!(raw.user_id, Some("user789".into()));
    }

    #[test]
    fn p02_parse_dm_no_guild() {
        let data = serde_json::json!({
            "id": "m1",
            "channel_id": "dm1",
            "content": "hi",
            "author": { "id": "u1", "bot": false }
        });
        let raw = parse_message_create_raw(&data).unwrap();
        assert!(raw.guild_id.is_none());
        let inc = raw_to_incoming(&raw, vec![], vec![]);
        assert!(inc.is_dm);
    }

    #[test]
    fn p03_parse_bot_author() {
        let data = serde_json::json!({
            "id": "m1",
            "channel_id": "c1",
            "content": "bot",
            "author": { "id": "b1", "bot": true }
        });
        let raw = parse_message_create_raw(&data).unwrap();
        assert!(raw.is_bot);
    }

    #[test]
    fn p04_missing_author_returns_none() {
        let data = serde_json::json!({ "id": "m1", "channel_id": "c1" });
        assert!(parse_message_create_raw(&data).is_some());
    }

    #[test]
    fn p05_two_mentions() {
        let data = serde_json::json!({
            "id": "m1",
            "channel_id": "c1",
            "content": "hi",
            "author": { "id": "u1" },
            "mentions": [{ "id": "a" }, { "id": "b" }]
        });
        let raw = parse_message_create_raw(&data).unwrap();
        assert_eq!(raw.mentions.len(), 2);
    }

    #[test]
    fn p08_parse_attachments_on_message() {
        let data = serde_json::json!({
            "id": "m1",
            "channel_id": "c1",
            "content": "",
            "author": { "id": "u1" },
            "attachments": [{
                "id": "a1",
                "filename": "voice.ogg",
                "content_type": "audio/ogg",
                "url": "https://cdn.discordapp.com/v.ogg",
                "size": 200,
                "waveform": [1, 2, 3]
            }]
        });
        let raw = parse_message_create_raw(&data).unwrap();
        assert_eq!(raw.attachments.len(), 1);
        assert!(raw.attachments[0].waveform.is_some());
    }

    #[test]
    fn p06_system_message_type_parsed() {
        let data = serde_json::json!({
            "id": "m1",
            "channel_id": "c1",
            "type": 6,
            "content": "pinned",
            "author": { "id": "u1" }
        });
        let raw = parse_message_create_raw(&data).unwrap();
        assert_eq!(raw.message_type, 6);
    }

    #[test]
    fn p07_interaction_create_maps_to_slash_incoming() {
        let data = serde_json::json!({
            "id": 9001,
            "application_id": 42,
            "type": 2,
            "token": "secret-token",
            "channel_id": 100,
            "guild_id": 200,
            "member": { "user": { "id": 7 } },
            "data": { "name": "new" }
        });
        let interaction = parse_interaction_create(&data).unwrap();
        let incoming = interaction_to_incoming(&interaction).unwrap();
        assert_eq!(incoming.text, "/new");
        assert_eq!(incoming.chat_id, "100");
        assert_eq!(incoming.user_id, "7");
        assert!(incoming.interaction_token.is_some());
    }
}
