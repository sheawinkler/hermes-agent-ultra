include!("adapter_impl/core.rs");
include!("adapter_impl/rich_text.rs");
include!("adapter_impl/message_routing.rs");
include!("adapter_impl/text_delivery.rs");
include!("adapter_impl/media_delivery.rs");
include!("adapter_impl/polling.rs");
include!("adapter_impl/message_parsing.rs");
include!("adapter_impl/files_admin.rs");
include!("adapter_impl/image_approval.rs");
include!("adapter_impl/http_helpers.rs");

#[async_trait]
impl PlatformAdapter for TelegramAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Telegram adapter starting (token: {})",
            describe_secret(&self.config.token)
        );
        self.base.mark_running();
        match self.register_command_menu().await {
            Ok(count) if count > 0 => {
                info!(count, "Telegram command menu registered");
            }
            Ok(_) => {}
            Err(err) => {
                warn!(error = %err, "Telegram command menu registration failed");
            }
        }
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Telegram adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        let pm = self.resolve_parse_mode(parse_mode);
        self.send_text(chat_id, text, pm, None).await?;
        Ok(())
    }

    async fn send_or_update_status(
        &self,
        chat_id: &str,
        status_key: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        let key = (chat_id.to_string(), status_key.to_string());
        let existing_id = self
            .status_message_ids
            .lock()
            .ok()
            .and_then(|ids| ids.get(&key).cloned());
        let pm = self.resolve_parse_mode(parse_mode);

        if let Some(message_id) = existing_id {
            match self.edit_text(chat_id, &message_id, text, pm).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    warn!(
                        chat_id,
                        status_key,
                        message_id,
                        error = %err,
                        "Telegram status edit failed; sending replacement status message"
                    );
                }
            }
        }

        let sent_ids = self.send_text(chat_id, text, pm, None).await?;
        if let Some(message_id) = sent_ids.first() {
            if let Ok(mut ids) = self.status_message_ids.lock() {
                ids.insert(key, message_id.to_string());
            }
        }
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        let pm = self.resolve_parse_mode(None);
        self.edit_text(chat_id, message_id, text, pm).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let ext = std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let (method, field) = Self::media_method_for_extension(&ext);
        self.send_multipart(chat_id, file_path, caption, method, field)
            .await?;
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.send_image_url_with_options(chat_id, image_url, caption, None, None)
            .await
    }

    async fn delete_message(&self, chat_id: &str, message_id: &str) -> Result<bool, GatewayError> {
        let (base_chat_id, thread_id) = Self::split_gateway_chat_thread(chat_id);
        let message_id = if thread_id.is_some() {
            message_id
                .split_once(':')
                .map(|(_, id)| id)
                .unwrap_or(message_id)
        } else {
            message_id
        };
        let url = format!("{}/deleteMessage", self.api_base);
        let message_id = message_id.parse::<i64>().map_err(|err| {
            GatewayError::SendFailed(format!("invalid Telegram message_id '{message_id}': {err}"))
        })?;
        let body = serde_json::json!({
            "chat_id": base_chat_id,
            "message_id": message_id,
        });
        let resp: TelegramResponse<bool> = self.post_json(&url, &body).await?;
        if resp.ok {
            Ok(resp.result.unwrap_or(true))
        } else {
            Err(GatewayError::SendFailed(
                resp.description
                    .unwrap_or_else(|| "deleteMessage failed".to_string()),
            ))
        }
    }

    async fn add_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        self.set_message_reaction(chat_id, message_id, Some(emoji))
            .await
    }

    async fn remove_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        _emoji: &str,
    ) -> Result<(), GatewayError> {
        self.set_message_reaction(chat_id, message_id, None).await
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn splits_long_messages(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "telegram"
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Split a message into chunks that fit within the given max length,
/// preferring to break at newline boundaries.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
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

        // Try to break at a newline near the boundary.
        let break_at = text[start..end]
            .rfind('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(end);

        chunks.push(text[start..break_at].to_string());
        start = break_at;
    }

    chunks
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated = text
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn parse_approval_callback(data: &str) -> Option<(ApprovalChoice, u64)> {
    let mut parts = data.split(':');
    if parts.next()? != "approval" {
        return None;
    }
    let choice = match parts.next()? {
        "once" => ApprovalChoice::Once,
        "session" => ApprovalChoice::Session,
        "deny" => ApprovalChoice::Deny,
        _ => return None,
    };
    let approval_id = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((choice, approval_id))
}
