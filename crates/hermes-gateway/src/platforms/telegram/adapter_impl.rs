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

    async fn send_message_threaded(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        thread_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        let thread_id = parse_telegram_thread_id(thread_id)?;
        let parse_mode = self.resolve_parse_mode(parse_mode);
        self.send_text_with_thread_policy(
            chat_id,
            text,
            parse_mode,
            thread_id,
            false,
        )
        .await?;
        Ok(())
    }

    async fn send_message_with_options(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let thread_id = parse_telegram_thread_id(options.thread_id.as_deref())?;
        let parse_mode = self.resolve_parse_mode(parse_mode);
        self.send_text_with_thread_policy(
            chat_id,
            text,
            parse_mode,
            thread_id,
            options.notify,
        )
        .await?;
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

    async fn send_file_with_options(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let message_thread_id = parse_telegram_thread_id(options.thread_id.as_deref())?;
        let extension = std::path::Path::new(file_path)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_lowercase();
        let (method, field_name) =
            Self::media_method_for_delivery(&extension, options.send_as_voice);
        self.send_multipart_with_thread_policy(
            TelegramMultipartRequest {
                chat_id,
                file_path,
                caption,
                method,
                field_name,
                reply_to_message_id: None,
                message_thread_id,
            },
            options.notify,
        )
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

    async fn send_image_url_with_options(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let message_thread_id = parse_telegram_thread_id(options.thread_id.as_deref())?;
        self.send_image_url_with_thread_policy(
            chat_id,
            image_url,
            caption,
            None,
            message_thread_id,
            options.notify,
        )
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

fn parse_telegram_thread_id(thread_id: Option<&str>) -> Result<Option<i64>, GatewayError> {
    let Some(thread_id) = thread_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let parsed = thread_id.parse::<i64>().map_err(|error| {
        GatewayError::SendFailed(format!(
            "invalid Telegram thread id '{thread_id}': {error}"
        ))
    })?;
    match parsed {
        0 => Ok(None),
        value if value > 0 => Ok(Some(value)),
        _ => Err(GatewayError::SendFailed(format!(
            "invalid Telegram thread id '{thread_id}': expected a positive integer"
        ))),
    }
}

/// Split a message into chunks that fit within the given max length,
/// preferring to break at newline boundaries.
fn split_message(text: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;
    loop {
        let Some((hard_end, _)) = remaining.char_indices().nth(max_chars) else {
            chunks.push(remaining.to_string());
            break;
        };
        let break_at = remaining[..hard_end]
            .rfind('\n')
            .map(|position| position + 1)
            .unwrap_or(hard_end);
        chunks.push(remaining[..break_at].to_string());
        remaining = &remaining[break_at..];
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
