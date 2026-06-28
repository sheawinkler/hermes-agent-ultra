impl TelegramAdapter {
    /// Send a document file.
    pub async fn send_document(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart_with_options(TelegramMultipartRequest {
            chat_id,
            file_path,
            caption,
            method: "sendDocument",
            field_name: "document",
            reply_to_message_id: None,
            message_thread_id: None,
        })
        .await
    }

    pub async fn send_document_with_options(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart_with_options(TelegramMultipartRequest {
            chat_id,
            file_path,
            caption,
            method: "sendDocument",
            field_name: "document",
            reply_to_message_id,
            message_thread_id,
        })
        .await
    }

    /// Send a photo file.
    pub async fn send_photo(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart_with_options(TelegramMultipartRequest {
            chat_id,
            file_path,
            caption,
            method: "sendPhoto",
            field_name: "photo",
            reply_to_message_id: None,
            message_thread_id: None,
        })
        .await
    }

    pub async fn send_photo_with_options(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart_with_options(TelegramMultipartRequest {
            chat_id,
            file_path,
            caption,
            method: "sendPhoto",
            field_name: "photo",
            reply_to_message_id,
            message_thread_id,
        })
        .await
    }

    /// Send an audio file.
    pub async fn send_audio(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart(chat_id, file_path, caption, "sendAudio", "audio")
            .await
    }

    /// Send a video file.
    pub async fn send_video(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart(chat_id, file_path, caption, "sendVideo", "video")
            .await
    }

    /// Send a voice message (OGG Opus).
    pub async fn send_voice(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart(chat_id, file_path, caption, "sendVoice", "voice")
            .await
    }

    /// Send an animation (GIF / MPEG4).
    pub async fn send_animation(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart(chat_id, file_path, caption, "sendAnimation", "animation")
            .await
    }

    /// Send a sticker by file path.
    pub async fn send_sticker(&self, chat_id: &str, file_path: &str) -> Result<i64, GatewayError> {
        self.send_multipart(chat_id, file_path, None, "sendSticker", "sticker")
            .await
    }

    /// Send a sticker by its `file_id` (already on Telegram servers).
    pub async fn send_sticker_by_id(
        &self,
        chat_id: &str,
        sticker_file_id: &str,
    ) -> Result<i64, GatewayError> {
        let body = serde_json::json!({
            "chat_id": chat_id,
            "sticker": sticker_file_id,
        });

        let url = format!("{}/sendSticker", self.api_base);
        let resp: TelegramResponse<SentMessage> = self.post_json(&url, &body).await?;

        resp.result.map(|m| m.message_id).ok_or_else(|| {
            GatewayError::SendFailed(
                resp.description
                    .unwrap_or_else(|| "sendSticker failed".into()),
            )
        })
    }

    /// Shared multipart upload for all media-sending endpoints.
    async fn send_multipart(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        method: &str,
        field_name: &str,
    ) -> Result<i64, GatewayError> {
        self.send_multipart_with_options(TelegramMultipartRequest {
            chat_id,
            file_path,
            caption,
            method,
            field_name,
            reply_to_message_id: None,
            message_thread_id: None,
        })
        .await
    }

    async fn send_multipart_with_options(
        &self,
        mut request: TelegramMultipartRequest<'_>,
    ) -> Result<i64, GatewayError> {
        let (chat_id, inferred_thread_id) = Self::split_gateway_chat_thread(request.chat_id);
        request.chat_id = chat_id;
        request.message_thread_id = request.message_thread_id.or(inferred_thread_id);
        match self.send_multipart_once(request).await {
            Ok(id) => Ok(id),
            Err(err)
                if Self::gateway_error_thread_or_reply_missing(&err)
                    && request.has_thread_context() =>
            {
                if let Some(thread_id) = request.message_thread_id {
                    self.prune_stale_dm_topic_binding_target(Some((
                        request.chat_id.to_string(),
                        thread_id.to_string(),
                    )));
                }
                self.send_multipart_once(request.without_thread_context())
                    .await
            }
            Err(err) => Err(err),
        }
    }

    async fn send_multipart_once(
        &self,
        request: TelegramMultipartRequest<'_>,
    ) -> Result<i64, GatewayError> {
        let url = format!("{}/{}", self.api_base, request.method);

        let file_bytes = tokio::fs::read(request.file_path).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to read file {}: {}", request.file_path, e))
        })?;

        let file_name = std::path::Path::new(request.file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name);

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", request.chat_id.to_string())
            .part(request.field_name.to_string(), part);

        if let Some(cap) = request.caption.map(str::trim).filter(|s| !s.is_empty()) {
            let truncated: String = cap.chars().take(MAX_CAPTION_LENGTH).collect();
            form = form.text("caption", truncated);
        }

        if let Some(reply_id) = request.reply_to_message_id {
            form = form.text("reply_to_message_id", reply_id.to_string());
        }

        if let Some(thread_id) = request.message_thread_id {
            form = form.text("message_thread_id", thread_id.to_string());
        }

        let resp = self
            .client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("{} failed: {}", request.method, e)))?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::RateLimited {
                retry_after_secs: Self::retry_after_from_telegram_body(&body_text),
            });
        }

        let result: TelegramResponse<SentMessage> = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!(
                "Failed to parse {} response: {}",
                request.method, e
            ))
        })?;

        if result.ok {
            result.result.map(|m| m.message_id).ok_or_else(|| {
                GatewayError::SendFailed(
                    result
                        .description
                        .unwrap_or_else(|| format!("{} returned no message", request.method)),
                )
            })
        } else {
            Err(Self::telegram_response_error(request.method, &result))
        }
    }

    // -----------------------------------------------------------------------
    // Receiving messages (long polling)
    // -----------------------------------------------------------------------

}
