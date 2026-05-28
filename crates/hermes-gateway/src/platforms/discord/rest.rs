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

pub fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + max_len).min(text.len());
        if end >= text.len() {
            chunks.push(text[start..].to_string());
            break;
        }
        let break_at = text[start..end]
            .rfind('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(end);
        chunks.push(text[start..break_at].to_string());
        start = break_at;
    }
    chunks
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
        for (index, chunk) in chunks.iter().enumerate() {
            if index > 0 && split_delay > Duration::ZERO {
                tokio::time::sleep(split_delay).await;
            }
            let ref_id = mode.reference_for_index(index, reply_to_message_id);
            let id = self.post_message(channel_id, chunk, ref_id).await?;
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
            "content": &content[..content.len().min(MAX_MESSAGE_LENGTH)],
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
        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name);
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
            "content": &content[..content.len().min(MAX_MESSAGE_LENGTH)],
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
            "content": &content[..content.len().min(MAX_MESSAGE_LENGTH)],
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
            "content": &content[..content.len().min(MAX_MESSAGE_LENGTH)],
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
        let first_id = self
            .send_embed(channel_id, caption, std::slice::from_ref(&embeds[0]))
            .await?;
        ids.push(first_id);
        for embed in embeds.into_iter().skip(1) {
            let id = self.send_embed(channel_id, None, &[embed]).await?;
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
        assert_eq!(chunks[0].len(), 2000);
        assert_eq!(chunks[1].len(), 1000);
    }

    #[test]
    fn encode_emoji_unicode() {
        assert_eq!(encode_emoji("\u{1f44d}"), "%F0%9F%91%8D");
    }
}
