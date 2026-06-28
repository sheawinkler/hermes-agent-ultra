impl TelegramAdapter {
    /// Send a text message, splitting into multiple messages if it exceeds
    /// the 4096 character limit.
    pub async fn send_text(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<&str>,
        reply_to_message_id: Option<i64>,
    ) -> Result<Vec<i64>, GatewayError> {
        self.send_text_inner(chat_id, text, parse_mode, reply_to_message_id, None, None)
            .await
    }

    /// Send a text message with an inline keyboard attached.
    pub async fn send_text_with_keyboard(
        &self,
        chat_id: &str,
        text: &str,
        keyboard: InlineKeyboardMarkup,
        parse_mode: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<Vec<i64>, GatewayError> {
        self.send_text_inner(
            chat_id,
            text,
            parse_mode,
            reply_to_message_id,
            Some(keyboard),
            message_thread_id,
        )
        .await
    }

    async fn send_text_inner(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<&str>,
        reply_to_message_id: Option<i64>,
        keyboard: Option<InlineKeyboardMarkup>,
        message_thread_id: Option<i64>,
    ) -> Result<Vec<i64>, GatewayError> {
        let (chat_id, inferred_thread_id) = Self::split_gateway_chat_thread(chat_id);
        let message_thread_id = message_thread_id.or(inferred_thread_id);
        if self.should_attempt_rich_text(text, keyboard.as_ref()) {
            if let Some(message_id) = self
                .try_send_rich_text(chat_id, text, reply_to_message_id, message_thread_id)
                .await?
            {
                return Ok(vec![message_id]);
            }
        }

        let chunks = split_message(text, MAX_MESSAGE_LENGTH);
        let mut message_ids = Vec::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let rendered_chunk = self.outgoing_text_for_parse_mode(chunk, parse_mode);
            let mut body = serde_json::json!({
                "chat_id": chat_id,
                "text": rendered_chunk,
            });

            if let Some(pm) = parse_mode {
                body["parse_mode"] = serde_json::Value::String(pm.to_string());
            }

            if self.config.disable_link_previews {
                body["disable_web_page_preview"] = serde_json::Value::Bool(true);
            }

            if let Some(thread_id) = message_thread_id {
                body["message_thread_id"] = serde_json::Value::Number(thread_id.into());
            }

            if self.should_thread_reply(reply_to_message_id, i) {
                if let Some(reply_id) = reply_to_message_id {
                    body["reply_to_message_id"] = serde_json::Value::Number(reply_id.into());
                }
            }

            // Attach keyboard only to the last chunk.
            if i == chunks.len() - 1 {
                if let Some(ref kb) = keyboard {
                    body["reply_markup"] =
                        serde_json::to_value(kb).unwrap_or(serde_json::Value::Null);
                }
            }

            let resp: TelegramResponse<SentMessage> = self
                .send_json_with_thread_fallback("sendMessage", body)
                .await?;

            if let Some(msg) = resp.result {
                message_ids.push(msg.message_id);
            } else {
                return Err(GatewayError::SendFailed(
                    resp.description
                        .unwrap_or_else(|| "sendMessage returned no message".to_string()),
                ));
            }
        }

        Ok(message_ids)
    }

    async fn try_send_rich_text(
        &self,
        chat_id: &str,
        text: &str,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<Option<i64>, GatewayError> {
        let body = self.rich_message_body(chat_id, text, reply_to_message_id, message_thread_id);
        let resp: TelegramResponse<SentMessage> = match self
            .send_json_with_thread_fallback("sendRichMessage", body)
            .await
        {
            Ok(resp) => resp,
            Err(err) if Self::rich_fallback_error(&err) => {
                if Self::rich_capability_error(&err) {
                    self.latch_rich_send_disabled();
                }
                return Ok(None);
            }
            Err(err) => return Err(err),
        };
        resp.result
            .map(|msg| msg.message_id)
            .ok_or_else(|| GatewayError::SendFailed("sendRichMessage returned no message".into()))
            .map(Some)
    }

    async fn try_edit_rich_text(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
        message_thread_id: Option<i64>,
    ) -> Result<Option<()>, GatewayError> {
        let body = self.rich_edit_body(chat_id, message_id, text, message_thread_id);
        match self
            .send_json_with_thread_fallback::<serde_json::Value>("editMessageText", body)
            .await
        {
            Ok(_) => Ok(Some(())),
            Err(err) if Self::rich_not_modified_error(&err) => Ok(Some(())),
            Err(err) if Self::rich_fallback_error(&err) => {
                if Self::rich_capability_error(&err) {
                    self.latch_rich_send_disabled();
                }
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    /// Edit an existing message's text.
    pub async fn edit_text(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
        parse_mode: Option<&str>,
    ) -> Result<(), GatewayError> {
        let (chat_id, message_thread_id) = Self::split_gateway_chat_thread(chat_id);
        if self.rich_eligible_text(text) {
            if let Some(()) = self
                .try_edit_rich_text(chat_id, message_id, text, message_thread_id)
                .await?
            {
                return Ok(());
            }
        }

        let rendered_text = self.outgoing_text_for_parse_mode(text, parse_mode);
        let rendered_text = truncate_chars(&rendered_text, MAX_MESSAGE_LENGTH);

        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id.parse::<i64>().unwrap_or(0),
            "text": rendered_text,
        });

        if let Some(thread_id) = message_thread_id {
            body["message_thread_id"] = serde_json::Value::Number(thread_id.into());
        }

        if let Some(pm) = parse_mode {
            body["parse_mode"] = serde_json::Value::String(pm.to_string());
        }

        let _resp: TelegramResponse<serde_json::Value> = self
            .send_json_with_thread_fallback("editMessageText", body)
            .await?;
        Ok(())
    }

    /// Answer a callback query (acknowledges the button press to the user).
    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
        show_alert: bool,
    ) -> Result<(), GatewayError> {
        let mut body = serde_json::json!({
            "callback_query_id": callback_query_id,
            "show_alert": show_alert,
        });

        if let Some(t) = text {
            body["text"] = serde_json::Value::String(t.to_string());
        }

        let url = format!("{}/answerCallbackQuery", self.api_base);
        let _resp: TelegramResponse<bool> = self.post_json(&url, &body).await?;
        Ok(())
    }

    async fn send_json_with_thread_fallback<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        mut body: serde_json::Value,
    ) -> Result<TelegramResponse<T>, GatewayError> {
        let url = format!("{}/{}", self.api_base, method);
        match self.post_json(&url, &body).await {
            Ok(resp) if resp.ok => Ok(resp),
            Ok(resp) => {
                let stale_topic_target = Self::thread_fallback_target_from_body(&body);
                let description = resp
                    .description
                    .as_deref()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{method} failed"));
                if Self::thread_or_reply_missing(&description)
                    && Self::strip_thread_fields_for_fallback(&mut body)
                {
                    self.prune_stale_dm_topic_binding_target(stale_topic_target);
                    let retry = self.post_json(&url, &body).await?;
                    if retry.ok {
                        return Ok(retry);
                    }
                    return Err(Self::telegram_response_error(
                        &format!("{method} fallback"),
                        &retry,
                    ));
                }
                Err(Self::telegram_response_error(method, &resp))
            }
            Err(err) if Self::gateway_error_thread_or_reply_missing(&err) => {
                let stale_topic_target = Self::thread_fallback_target_from_body(&body);
                if Self::strip_thread_fields_for_fallback(&mut body) {
                    self.prune_stale_dm_topic_binding_target(stale_topic_target);
                    self.post_json(&url, &body).await
                } else {
                    Err(err)
                }
            }
            Err(err) => Err(err),
        }
    }

    fn strip_thread_fields_for_fallback(body: &mut serde_json::Value) -> bool {
        let Some(obj) = body.as_object_mut() else {
            return false;
        };
        let removed_thread = obj.remove("message_thread_id").is_some();
        let removed_reply = obj.remove("reply_to_message_id").is_some();
        let removed_reply_parameters = obj.remove("reply_parameters").is_some();
        removed_thread || removed_reply || removed_reply_parameters
    }

    fn thread_fallback_target_from_body(body: &serde_json::Value) -> Option<(String, String)> {
        let obj = body.as_object()?;
        let chat_id = Self::telegram_id_value_to_string(obj.get("chat_id")?)?;
        let thread_id = Self::telegram_id_value_to_string(obj.get("message_thread_id")?)?;
        Some((chat_id, thread_id))
    }

    fn telegram_id_value_to_string(value: &serde_json::Value) -> Option<String> {
        value
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| value.as_i64().map(|id| id.to_string()))
    }

    fn prune_stale_dm_topic_binding_target(&self, target: Option<(String, String)>) {
        let Some((chat_id, thread_id)) = target else {
            return;
        };
        if self.prune_stale_dm_topic_binding(&chat_id, &thread_id) {
            info!(
                chat_id = %chat_id,
                thread_id = %thread_id,
                "Pruned stale Telegram DM topic binding after Bot API reported the thread missing"
            );
        }
    }

    fn gateway_error_thread_or_reply_missing(err: &GatewayError) -> bool {
        match err {
            GatewayError::SendFailed(message)
            | GatewayError::Platform(message)
            | GatewayError::ConnectionFailed(message) => Self::thread_or_reply_missing(message),
            _ => false,
        }
    }

    fn thread_or_reply_missing(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("message thread not found")
            || lower.contains("thread not found")
            || lower.contains("message to be replied not found")
            || lower.contains("reply message not found")
    }

    // -----------------------------------------------------------------------
    // File operations
    // -----------------------------------------------------------------------

}
