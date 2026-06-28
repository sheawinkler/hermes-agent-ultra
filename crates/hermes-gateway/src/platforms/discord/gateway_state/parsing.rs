impl DiscordAdapter {
    // -----------------------------------------------------------------------
    // Event parsing
    // -----------------------------------------------------------------------

    /// Parse a MESSAGE_CREATE dispatch event into an IncomingDiscordMessage.
    pub fn parse_message_create(data: &serde_json::Value) -> Option<IncomingDiscordMessage> {
        let channel_id = data.get("channel_id")?.as_str()?.to_string();
        let message_id = data.get("id")?.as_str()?.to_string();
        let content = data
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let author = data.get("author");
        let user_id = author
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let username = author
            .and_then(|a| a.get("username"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let is_bot = author
            .and_then(|a| a.get("bot"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let message_type = data.get("type").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
        let mention_user_ids = data
            .get("mentions")
            .and_then(|v| v.as_array())
            .map(|mentions| {
                mentions
                    .iter()
                    .filter_map(|mention| mention.get("id").and_then(|id| id.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let reply_to_message_id = data
            .get("message_reference")
            .and_then(|reference| reference.get("message_id"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let reply_to_text = data
            .get("referenced_message")
            .and_then(|message| message.get("content"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(String::from);
        let mut attachments = parse_discord_incoming_attachments(data.get("attachments"));
        attachments.extend(parse_discord_incoming_attachments(
            data.get("referenced_message")
                .and_then(|message| message.get("attachments")),
        ));

        Some(IncomingDiscordMessage {
            channel_id,
            message_id,
            user_id,
            username,
            content,
            is_bot,
            message_type,
            mention_user_ids,
            reply_to_message_id,
            reply_to_text,
            attachments,
        })
    }

    /// Apply Discord inbound self/system/bot filtering to a parsed message.
    pub fn should_accept_message(
        message: &IncomingDiscordMessage,
        client_user_id: Option<&str>,
        bot_policy: DiscordBotMessagePolicy,
    ) -> bool {
        if let (Some(author_id), Some(client_id)) = (message.user_id.as_deref(), client_user_id) {
            if author_id.trim() == client_id.trim() {
                return false;
            }
        }

        if !discord_message_type_is_user_visible(message.message_type) {
            return false;
        }

        if !message.is_bot {
            return true;
        }

        match bot_policy {
            DiscordBotMessagePolicy::None => false,
            DiscordBotMessagePolicy::All => true,
            DiscordBotMessagePolicy::Mentions => client_user_id
                .map(|id| message.mentions_user(id))
                .unwrap_or(false),
        }
    }

    /// Parse a MESSAGE_UPDATE dispatch event.
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

    /// Parse an INTERACTION_CREATE dispatch event.
    pub fn parse_interaction_create(data: &serde_json::Value) -> Option<InteractionData> {
        let id = data.get("id")?.as_str()?.to_string();
        let application_id = data.get("application_id")?.as_str()?.to_string();
        let interaction_type = data.get("type")?.as_u64()? as u8;
        let token = data.get("token")?.as_str()?.to_string();

        let channel_id = data
            .get("channel_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let guild_id = data
            .get("guild_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        // User ID can be in `member.user.id` (guild) or `user.id` (DM).
        let user_id = data
            .get("member")
            .and_then(|m| m.get("user"))
            .and_then(|u| u.get("id"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                data.get("user")
                    .and_then(|u| u.get("id"))
                    .and_then(|v| v.as_str())
            })
            .map(String::from);

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
        })
    }

    /// Parse a MESSAGE_REACTION_ADD or MESSAGE_REACTION_REMOVE event.
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

    /// Parse a VOICE_STATE_UPDATE event.
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

    /// Route a dispatch event by name to the appropriate parser.
    ///
    /// Returns a [`DispatchEvent`] for known event types, or `None`.
    pub fn parse_dispatch(event_name: &str, data: &serde_json::Value) -> Option<DispatchEvent> {
        match event_name {
            "MESSAGE_CREATE" => Self::parse_message_create(data).map(DispatchEvent::MessageCreate),
            "MESSAGE_UPDATE" => Self::parse_message_update(data).map(DispatchEvent::MessageUpdate),
            "INTERACTION_CREATE" => {
                Self::parse_interaction_create(data).map(DispatchEvent::InteractionCreate)
            }
            "MESSAGE_REACTION_ADD" => {
                Self::parse_reaction_event(data).map(DispatchEvent::ReactionAdd)
            }
            "MESSAGE_REACTION_REMOVE" => {
                Self::parse_reaction_event(data).map(DispatchEvent::ReactionRemove)
            }
            "VOICE_STATE_UPDATE" => {
                Self::parse_voice_state_update(data).map(DispatchEvent::VoiceStateUpdate)
            }
            _ => None,
        }
    }
}
