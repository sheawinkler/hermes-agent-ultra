impl TelegramAdapter {
    pub async fn send_image_url_with_options(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<(), GatewayError> {
        let (chat_id, inferred_thread_id) = Self::split_gateway_chat_thread(chat_id);
        let message_thread_id = message_thread_id.or(inferred_thread_id);
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "photo": image_url,
        });
        if let Some(cap) = caption.map(str::trim).filter(|s| !s.is_empty()) {
            let truncated: String = cap.chars().take(MAX_CAPTION_LENGTH).collect();
            body["caption"] = serde_json::Value::String(truncated);
        }
        if let Some(reply_id) = reply_to_message_id {
            body["reply_to_message_id"] = serde_json::Value::Number(reply_id.into());
        }
        if let Some(thread_id) = message_thread_id {
            body["message_thread_id"] = serde_json::Value::Number(thread_id.into());
        }
        let _resp: TelegramResponse<SentMessage> = self
            .send_json_with_thread_fallback("sendPhoto", body)
            .await?;
        Ok(())
    }

    async fn set_message_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        reaction: Option<&str>,
    ) -> Result<(), GatewayError> {
        if !self.config.reactions {
            return Ok(());
        }

        let message_id = message_id.parse::<i64>().map_err(|_| {
            GatewayError::SendFailed(format!(
                "Invalid Telegram message_id for reaction: {message_id}"
            ))
        })?;
        let reaction_value = match reaction {
            Some(emoji) => serde_json::json!([{ "type": "emoji", "emoji": emoji }]),
            None => serde_json::json!([]),
        };
        let body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "reaction": reaction_value,
        });
        let url = format!("{}/setMessageReaction", self.api_base);
        let _resp: TelegramResponse<bool> = self.post_json(&url, &body).await?;
        Ok(())
    }

    pub async fn send_approval_request(
        &self,
        chat_id: &str,
        request: &GatewayApprovalRequest,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<Vec<i64>, GatewayError> {
        let approval_id = self.approval_counter.fetch_add(1, Ordering::SeqCst);
        self.approval_state
            .lock()
            .map_err(|_| GatewayError::Platform("telegram approval state poisoned".to_string()))?
            .insert(approval_id, request.session_key.clone());

        let command = truncate_chars(&request.command, 1800);
        let description = truncate_chars(&request.description, 1200);
        let text = format!(
            "*Command approval required*\n\nCommand:\n`{command}`\n\nReason: {description}"
        );
        let keyboard = InlineKeyboardMarkup {
            inline_keyboard: vec![
                vec![
                    InlineKeyboardButton {
                        text: "Approve once".to_string(),
                        callback_data: Some(format!("approval:once:{approval_id}")),
                        url: None,
                    },
                    InlineKeyboardButton {
                        text: "Approve session".to_string(),
                        callback_data: Some(format!("approval:session:{approval_id}")),
                        url: None,
                    },
                ],
                vec![InlineKeyboardButton {
                    text: "Deny".to_string(),
                    callback_data: Some(format!("approval:deny:{approval_id}")),
                    url: None,
                }],
            ],
        };
        self.send_text_with_keyboard(
            chat_id,
            &text,
            keyboard,
            Some("MarkdownV2"),
            reply_to_message_id,
            message_thread_id,
        )
        .await
    }

    pub async fn handle_approval_callback(
        &self,
        callback_query_id: &str,
        callback_data: &str,
    ) -> Result<bool, GatewayError> {
        let Some((choice, approval_id)) = parse_approval_callback(callback_data) else {
            return Ok(false);
        };
        let session_key = self
            .approval_state
            .lock()
            .map_err(|_| GatewayError::Platform("telegram approval state poisoned".to_string()))?
            .remove(&approval_id);
        let Some(session_key) = session_key else {
            self.answer_callback_query(callback_query_id, Some("Approval already resolved"), true)
                .await?;
            return Ok(true);
        };
        let resolved = approval::resolve_gateway_approval(
            &session_key,
            choice,
            matches!(choice, ApprovalChoice::Session),
        );
        let answer = if resolved == 0 {
            "No pending approval for this session"
        } else if choice == ApprovalChoice::Deny {
            "Denied"
        } else {
            "Approved"
        };
        self.answer_callback_query(callback_query_id, Some(answer), false)
            .await?;
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

}
