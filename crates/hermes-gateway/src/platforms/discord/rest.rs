//! Discord REST API outbound operations.

use std::time::Duration;

use tracing::info;

use hermes_core::errors::GatewayError;

use super::config::MAX_MESSAGE_LENGTH;
use super::gateway_loop::DiscordInner;
use super::types::{DiscordEmbed, DiscordMessage, DiscordThread, EmbedMedia, SlashCommand};

const INTERACTION_FLAG_EPHEMERAL: u64 = 1 << 6;

pub fn auth_header(token: &str) -> String {
    format!("Bot {token}")
}

pub fn char_count(text: &str) -> usize {
    text.chars().count()
}

/// First `max_chars` Unicode scalars of `text` (Discord counts characters, not bytes).
pub fn truncate_to_char_limit(text: &str, max_chars: usize) -> String {
    if char_count(text) <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

pub fn split_message(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    if char_count(text) <= max_chars {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if char_count(rest) <= max_chars {
            chunks.push(rest.to_string());
            break;
        }

        let mut end_byte = 0;
        let mut count = 0;
        for (byte_idx, ch) in rest.char_indices() {
            count += 1;
            end_byte = byte_idx + ch.len_utf8();
            if count >= max_chars {
                break;
            }
        }

        if end_byte >= rest.len() {
            chunks.push(rest.to_string());
            break;
        }

        let break_at = rest[..end_byte]
            .rfind('\n')
            .map(|pos| pos + 1)
            .filter(|&pos| pos > 0)
            .unwrap_or(end_byte);

        chunks.push(rest[..break_at].to_string());
        rest = &rest[break_at..];
        if break_at == 0 {
            let ch_len = rest
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1);
            chunks.push(rest[..ch_len.min(rest.len())].to_string());
            rest = &rest[ch_len.min(rest.len())..];
        }
    }
    chunks
}

pub fn is_forum_channel_type(channel_type: Option<u8>) -> bool {
    channel_type == Some(15)
}

pub fn outbound_upload_name(path: &str) -> (String, Option<&'static str>) {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".gif") {
        return ("animation.gif".to_string(), Some("image/gif"));
    }
    if lower.ends_with(".mp4") || lower.ends_with(".mov") || lower.ends_with(".webm") {
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("video.mp4")
            .to_string();
        return (name, Some("video/mp4"));
    }
    if lower.ends_with(".ogg")
        || lower.ends_with(".opus")
        || lower.ends_with(".mp3")
        || lower.ends_with(".wav")
        || lower.ends_with(".m4a")
    {
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("voice.ogg")
            .to_string();
        return (name, Some("audio/ogg"));
    }
    (
        std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string(),
        None,
    )
}

pub fn encode_emoji(emoji: &str) -> String {
    let mut out = String::new();
    for byte in emoji.as_bytes() {
        if byte.is_ascii_alphanumeric() || *byte == b'-' || *byte == b'_' || *byte == b':' {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

impl DiscordInner {
    fn rest_api(&self) -> &str {
        &self.config.rest_api_base
    }

    pub(crate) fn auth_header(&self) -> String {
        auth_header(&self.config.token)
    }

    async fn post_message(
        &self,
        channel_id: &str,
        content: &str,
        reply_to_message_id: Option<&str>,
    ) -> Result<String, GatewayError> {
        let url = format!("{}/channels/{channel_id}/messages", self.rest_api());
        let mut body = serde_json::json!({
            "content": content,
            "allowed_mentions": self.config.allowed_mentions.to_api_value(),
        });
        if let Some(ref_id) = reply_to_message_id {
            body["message_reference"] = serde_json::json!({
                "message_id": ref_id,
                "fail_if_not_exists": false,
            });
        }
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord send failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Discord API error: {text}")));
        }
        let msg: DiscordMessage = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse Discord response: {e}"))
        })?;
        Ok(msg.id)
    }

    pub async fn send_text(
        &self,
        channel_id: &str,
        content: &str,
    ) -> Result<Vec<String>, GatewayError> {
        self.send_text_with_reply(channel_id, content, None).await
    }

    pub async fn send_text_with_reply(
        &self,
        channel_id: &str,
        content: &str,
        reply_to_message_id: Option<&str>,
    ) -> Result<Vec<String>, GatewayError> {
        let chunks = split_message(content, MAX_MESSAGE_LENGTH);
        let mode = self.config.reply_to_mode;
        let mut message_ids = Vec::new();
        let split_delay = Duration::from_secs_f64(self.config.text_batch_split_delay_seconds);
        let mut target_channel = channel_id.to_string();
        let mut skip_first_post = false;
        if let Some(channel_type) = self.fetch_channel_type(channel_id).await? {
            if is_forum_channel_type(Some(channel_type)) {
                let (thread_id, posted) = self.forum_thread_target(channel_id, &chunks[0]).await?;
                target_channel = thread_id;
                skip_first_post = posted;
            }
        }
        for (index, chunk) in chunks.iter().enumerate() {
            if index == 0 && skip_first_post {
                continue;
            }
            if index > 0 && split_delay > Duration::ZERO {
                tokio::time::sleep(split_delay).await;
            }
            let ref_id = mode.reference_for_index(index, reply_to_message_id);
            let id = self.post_message(&target_channel, chunk, ref_id).await?;
            message_ids.push(id);
        }
        Ok(message_ids)
    }

    pub async fn edit_text(
        &self,
        channel_id: &str,
        message_id: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/channels/{channel_id}/messages/{message_id}",
            self.rest_api()
        );
        let body = serde_json::json!({
            "content": truncate_to_char_limit(content, MAX_MESSAGE_LENGTH),
            "allowed_mentions": self.config.allowed_mentions.to_api_value(),
        });
        let resp = self
            .client
            .patch(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord edit failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Discord edit API error: {text}")));
        }
        Ok(())
    }

    pub async fn delete_message(
        &self,
        channel_id: &str,
        message_id: &str,
    ) -> Result<bool, GatewayError> {
        let url = format!(
            "{}/channels/{channel_id}/messages/{message_id}",
            self.rest_api()
        );
        let resp = self
            .client
            .delete(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord delete failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord delete API error: {text}"
            )));
        }
        Ok(true)
    }

    pub async fn send_embed(
        &self,
        channel_id: &str,
        content: Option<&str>,
        embeds: &[DiscordEmbed],
    ) -> Result<String, GatewayError> {
        let url = format!("{}/channels/{channel_id}/messages", self.rest_api());
        let mut body = serde_json::json!({ "embeds": embeds });
        if let Some(text) = content {
            body["content"] = serde_json::Value::String(text.to_string());
        }
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord embed send failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Discord embed API error: {text}")));
        }
        let msg: DiscordMessage = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse Discord response: {e}"))
        })?;
        Ok(msg.id)
    }

    pub async fn fetch_channel_type(&self, channel_id: &str) -> Result<Option<u8>, GatewayError> {
        let url = format!("{}/channels/{channel_id}", self.rest_api());
        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Discord GET channel: {e}")))?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Discord GET channel json: {e}"))
        })?;
        Ok(body.get("type").and_then(|v| v.as_u64()).map(|v| v as u8))
    }


    pub async fn create_forum_thread(
        &self,
        forum_channel_id: &str,
        name: &str,
        message_content: &str,
    ) -> Result<String, GatewayError> {
        let url = format!("{}/channels/{forum_channel_id}/threads", self.rest_api());
        let body = serde_json::json!({
            "name": name,
            "message": {
                "content": message_content,
                "allowed_mentions": self.config.allowed_mentions.to_api_value(),
            },
        });
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord forum thread failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord forum thread API error: {text}"
            )));
        }
        let thread: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Discord forum thread parse error: {e}"))
        })?;
        thread
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| GatewayError::SendFailed("forum thread missing id".into()))
    }

    async fn forum_thread_target(
        &self,
        forum_channel_id: &str,
        first_chunk: &str,
    ) -> Result<(String, bool), GatewayError> {
        let name: String = first_chunk.chars().take(100).collect();
        let name = if name.trim().is_empty() {
            "Hermes post".to_string()
        } else {
            name
        };
        let thread_id = self
            .create_forum_thread(forum_channel_id, &name, first_chunk)
            .await?;
        Ok((thread_id, true))
    }

    pub async fn upload_file(
        &self,
        channel_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<String, GatewayError> {
        let url = format!("{}/channels/{channel_id}/messages", self.rest_api());
        let file_bytes = tokio::fs::read(file_path).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to read file {file_path}: {e}"))
        })?;
        let (file_name, mime) = outbound_upload_name(file_path);
        let mut part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name);
        if let Some(mime) = mime {
            part = part.mime_str(mime).map_err(|e| {
                GatewayError::SendFailed(format!("invalid mime for upload: {e}"))
            })?;
        }
        let mut form = reqwest::multipart::Form::new().part("files[0]", part);
        if let Some(cap) = caption {
            let payload = serde_json::json!({ "content": cap });
            form = form.text("payload_json", payload.to_string());
        }
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord file upload failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord file upload API error: {text}"
            )));
        }
        let msg: DiscordMessage = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse Discord response: {e}"))
        })?;
        Ok(msg.id)
    }

    pub async fn trigger_typing(&self, channel_id: &str) -> Result<(), GatewayError> {
        let url = format!("{}/channels/{channel_id}/typing", self.rest_api());
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord typing failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord typing API error: {text}"
            )));
        }
        Ok(())
    }

    pub async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        let encoded = encode_emoji(emoji);
        let url = format!(
            "{}/channels/{channel_id}/messages/{message_id}/reactions/{encoded}/@me",
            self.rest_api()
        );
        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord add_reaction failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord add_reaction API error: {text}"
            )));
        }
        Ok(())
    }

    pub async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        let encoded = encode_emoji(emoji);
        let url = format!(
            "{}/channels/{channel_id}/messages/{message_id}/reactions/{encoded}/@me",
            self.rest_api()
        );
        let resp = self
            .client
            .delete(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord remove_reaction failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord remove_reaction API error: {text}"
            )));
        }
        Ok(())
    }

    pub async fn create_thread(
        &self,
        channel_id: &str,
        message_id: &str,
        name: &str,
        auto_archive_duration: Option<u32>,
    ) -> Result<DiscordThread, GatewayError> {
        let url = format!(
            "{}/channels/{channel_id}/messages/{message_id}/threads",
            self.rest_api()
        );
        let mut body = serde_json::json!({ "name": name });
        if let Some(dur) = auto_archive_duration {
            body["auto_archive_duration"] = serde_json::Value::Number(dur.into());
        }
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord create_thread failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord create_thread API error: {text}"
            )));
        }
        resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse thread response: {e}"))
        })
    }

    pub async fn register_slash_commands(&self, commands: &[SlashCommand]) -> Result<(), GatewayError> {
        let app_id = self.config.application_id.as_deref().ok_or_else(|| {
            GatewayError::Platform("application_id required for slash commands".into())
        })?;
        let url = format!("{}/applications/{app_id}/commands", self.rest_api());
        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(commands)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord register_commands failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord register_commands API error: {text}"
            )));
        }
        info!("registered {} global slash commands", commands.len());
        Ok(())
    }

    pub async fn register_guild_slash_commands(
        &self,
        guild_id: &str,
        commands: &[SlashCommand],
    ) -> Result<(), GatewayError> {
        let app_id = self.config.application_id.as_deref().ok_or_else(|| {
            GatewayError::Platform("application_id required for slash commands".into())
        })?;
        let url = format!(
            "{}/applications/{app_id}/guilds/{guild_id}/commands",
            self.rest_api()
        );
        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(commands)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord register_guild_commands failed: {e}"))
            })?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord register_guild_commands API error: {text}"
            )));
        }
        info!(
            "registered {} guild slash commands for {guild_id}",
            commands.len()
        );
        Ok(())
    }

    fn interaction_application_id(&self) -> Result<String, GatewayError> {
        self.config.application_id.clone().ok_or_else(|| {
            GatewayError::Platform("application_id required for interaction responses".into())
        })
    }

    pub async fn respond_to_interaction_immediate(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        content: &str,
        ephemeral: bool,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/interactions/{interaction_id}/{interaction_token}/callback",
            self.rest_api()
        );
        let mut data = serde_json::json!({
            "content": truncate_to_char_limit(content, MAX_MESSAGE_LENGTH),
        });
        if ephemeral {
            data["flags"] = serde_json::json!(INTERACTION_FLAG_EPHEMERAL);
        }
        let body = serde_json::json!({
            "type": 4,
            "data": data,
        });
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord interaction response failed: {e}"))
            })?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord interaction response API error: {text}"
            )));
        }
        Ok(())
    }

    async fn edit_deferred_interaction(
        &self,
        interaction_token: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        let app_id = self.interaction_application_id()?;
        let url = format!(
            "{}/webhooks/{app_id}/{interaction_token}/messages/@original",
            self.rest_api()
        );
        let body = serde_json::json!({
            "content": truncate_to_char_limit(content, MAX_MESSAGE_LENGTH),
        });
        let resp = self
            .client
            .patch(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord deferred interaction edit failed: {e}"))
            })?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord deferred interaction edit API error: {text}"
            )));
        }
        Ok(())
    }

    async fn follow_up_interaction(
        &self,
        interaction_token: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        let app_id = self.interaction_application_id()?;
        let url = format!(
            "{}/webhooks/{app_id}/{interaction_token}",
            self.rest_api()
        );
        let body = serde_json::json!({
            "content": truncate_to_char_limit(content, MAX_MESSAGE_LENGTH),
        });
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord interaction follow-up failed: {e}"))
            })?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord interaction follow-up API error: {text}"
            )));
        }
        Ok(())
    }

    pub async fn respond_to_interaction(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        let chunks = split_message(content, MAX_MESSAGE_LENGTH);
        let deferred = self.take_interaction_deferred(interaction_id).await;
        if deferred {
            self.edit_deferred_interaction(interaction_token, &chunks[0])
                .await?;
        } else {
            self.respond_to_interaction_immediate(
                interaction_id,
                interaction_token,
                &chunks[0],
                false,
            )
            .await?;
        }
        for chunk in chunks.iter().skip(1) {
            self.follow_up_interaction(interaction_token, chunk).await?;
        }
        Ok(())
    }

    pub async fn respond_autocomplete(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        choices: &[super::types::SlashCommandChoice],
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/interactions/{interaction_id}/{interaction_token}/callback",
            self.rest_api()
        );
        let choices_json: Vec<serde_json::Value> = choices
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "value": c.value,
                })
            })
            .collect();
        let body = serde_json::json!({
            "type": 8,
            "data": { "choices": choices_json },
        });
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord autocomplete callback failed: {e}"))
            })?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord autocomplete API error: {text}"
            )));
        }
        Ok(())
    }

    pub async fn defer_interaction(
        &self,
        interaction_id: &str,
        interaction_token: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/interactions/{interaction_id}/{interaction_token}/callback",
            self.rest_api()
        );
        let body = serde_json::json!({ "type": 5 });
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord defer interaction failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord defer interaction API error: {text}"
            )));
        }
        Ok(())
    }

    pub async fn channel_topic_cached(&self, channel_id: &str) -> Option<String> {
        if let Some(t) = self.channel_topic_cache.read().await.get(channel_id) {
            return Some(t.clone());
        }
        let topic = self.fetch_channel_topic(channel_id).await.ok().flatten()?;
        self.channel_topic_cache
            .write()
            .await
            .insert(channel_id.to_string(), topic.clone());
        Some(topic)
    }

    async fn fetch_channel_topic(&self, channel_id: &str) -> Result<Option<String>, GatewayError> {
        let url = format!("{}/channels/{channel_id}", self.rest_api());
        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Discord GET channel: {e}")))?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Discord GET channel json: {e}"))
        })?;
        Ok(body
            .get("topic")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from))
    }

    pub async fn fetch_channel_messages(
        &self,
        channel_id: &str,
        limit: u32,
    ) -> Result<Vec<(String, String)>, GatewayError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let url = format!(
            "{}/channels/{channel_id}/messages?limit={}",
            self.rest_api(),
            limit.min(100)
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Discord GET messages: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Discord GET messages HTTP error: {text}"
            )));
        }
        let body: Vec<serde_json::Value> = resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Discord GET messages json: {e}"))
        })?;
        let mut rows = Vec::new();
        for msg in body.into_iter().rev() {
            let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let content = msg
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !content.trim().is_empty() {
                rows.push((id, content));
            }
        }
        Ok(rows)
    }

    pub async fn send_multiple_image_embeds(
        &self,
        channel_id: &str,
        image_urls: &[&str],
        caption: Option<&str>,
    ) -> Result<Vec<String>, GatewayError> {
        let mut ids = Vec::new();
        if image_urls.is_empty() {
            return Ok(ids);
        }
        let caption_text = caption.unwrap_or("");
        let mut target_channel = channel_id.to_string();
        let mut skip_first_embed = false;
        if let Some(channel_type) = self.fetch_channel_type(channel_id).await? {
            if is_forum_channel_type(Some(channel_type)) {
                let (thread_id, posted) = self.forum_thread_target(channel_id, caption_text).await?;
                target_channel = thread_id;
                skip_first_embed = posted && !caption_text.trim().is_empty();
            }
        }
        let embeds: Vec<DiscordEmbed> = image_urls
            .iter()
            .map(|url| {
                let mut e = DiscordEmbed::new();
                e.image = Some(EmbedMedia {
                    url: (*url).to_string(),
                });
                e
            })
            .collect();
        if !skip_first_embed {
            let first_id = self
                .send_embed(&target_channel, caption, std::slice::from_ref(&embeds[0]))
                .await?;
            ids.push(first_id);
        }
        let start = if skip_first_embed { 0 } else { 1 };
        for embed in embeds.into_iter().skip(start) {
            let id = self.send_embed(&target_channel, None, &[embed]).await?;
            ids.push(id);
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_message_short() {
        let chunks = split_message("hello", 2000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_long() {
        let text = "a".repeat(3000);
        let chunks = split_message(&text, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(char_count(&chunks[0]), 2000);
        assert_eq!(char_count(&chunks[1]), 1000);
    }

    #[test]
    fn split_message_chinese_does_not_panic_at_char_boundary() {
        let text = "好".repeat(2500);
        let chunks = split_message(&text, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(char_count(&chunks[0]), 2000);
        assert_eq!(char_count(&chunks[1]), 500);
        assert!(chunks.join("").chars().eq(text.chars()));
    }

    #[test]
    fn truncate_to_char_limit_chinese() {
        let text = "雨夜来客".repeat(600);
        let truncated = truncate_to_char_limit(&text, 2000);
        assert_eq!(char_count(&truncated), 2000);
    }

    #[test]
    fn encode_emoji_unicode() {
        assert_eq!(encode_emoji("\u{1f44d}"), "%F0%9F%91%8D");
    }
}
