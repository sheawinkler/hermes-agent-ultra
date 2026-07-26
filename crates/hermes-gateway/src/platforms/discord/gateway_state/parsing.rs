fn discord_channel_type_is_thread(channel_type: Option<u8>) -> bool {
    // GUILD_NEWS_THREAD, GUILD_PUBLIC_THREAD, and GUILD_PRIVATE_THREAD.
    matches!(channel_type, Some(10..=12))
}

impl DiscordAdapter {
    // -----------------------------------------------------------------------
    // Event parsing
    // -----------------------------------------------------------------------

    /// Parse a MESSAGE_CREATE dispatch event into an IncomingDiscordMessage.
    pub fn parse_message_create(data: &serde_json::Value) -> Option<IncomingDiscordMessage> {
        let channel_id = data.get("channel_id")?.as_str()?.to_string();
        let message_id = data.get("id")?.as_str()?.to_string();
        let channel_type = data
            .get("channel_type")
            .and_then(|value| value.as_u64())
            .and_then(|value| u8::try_from(value).ok());
        let guild_id = data
            .get("guild_id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if matches!(channel_type, Some(1 | 3)) && guild_id.is_some() {
            // Discord DM channel types cannot carry guild authority. Reject
            // contradictory payloads rather than routing them as either DM or
            // guild traffic.
            return None;
        }
        // Discord MESSAGE_CREATE can carry a direct `parent_id` on thread
        // channel payloads. Some gateway/proxy payloads instead nest it in
        // `thread`; THREAD_CREATE/UPDATE state is the final fallback.
        let parent_channel_id = data
            .get("parent_id")
            .and_then(|value| value.as_str())
            .or_else(|| {
                data.get("thread")
                    .and_then(|thread| thread.get("parent_id"))
                    .and_then(|value| value.as_str())
            })
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let is_thread =
            discord_channel_type_is_thread(channel_type) || parent_channel_id.is_some();
        let thread_id = is_thread.then(|| channel_id.clone());
        // Only explicit Discord DM channel types may enter the DM path.
        // Missing channel metadata is ambiguous—not a DM default—and the
        // gateway worker resolves it through trusted cache/REST authority.
        let is_dm = if is_thread {
            false
        } else {
            matches!(channel_type, Some(1 | 3)) && guild_id.is_none()
        };
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
            .unwrap_or(false)
            || data
                .get("webhook_id")
                .is_some_and(|webhook_id| !webhook_id.is_null());
        let member_role_ids = data
            .get("member")
            .and_then(|member| member.get("roles"))
            .and_then(|roles| roles.as_array())
            .map(|roles| {
                roles
                    .iter()
                    .filter_map(|role| role.as_str())
                    .map(str::trim)
                    .filter(|role| !role.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
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
        let attachments = parse_discord_incoming_attachments(data.get("attachments"));
        let reply_to_attachments = parse_discord_incoming_attachments(
            data.get("referenced_message")
                .and_then(|message| message.get("attachments")),
        );

        Some(IncomingDiscordMessage {
            channel_id,
            channel_type,
            guild_id,
            parent_channel_id,
            thread_id,
            is_dm,
            message_id,
            user_id,
            username,
            content,
            is_bot,
            member_role_ids,
            message_type,
            mention_user_ids,
            reply_to_message_id,
            reply_to_text,
            reply_to_attachments,
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


    /// Route production-consumed Discord dispatches, or return `None`.
    pub fn parse_dispatch(event_name: &str, data: &serde_json::Value) -> Option<DispatchEvent> {
        (event_name == "MESSAGE_CREATE")
            .then(|| Self::parse_message_create(data))
            .flatten()
            .map(DispatchEvent::MessageCreate)
    }
}
