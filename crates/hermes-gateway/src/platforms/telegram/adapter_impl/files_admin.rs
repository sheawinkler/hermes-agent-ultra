impl TelegramAdapter {
    /// Download a file from Telegram by file_id.
    /// Returns the URL from which the file can be downloaded.
    pub async fn get_file_url(&self, file_id: &str) -> Result<String, GatewayError> {
        let url = format!("{}/getFile", self.api_base);
        let body = serde_json::json!({ "file_id": file_id });

        let resp: TelegramResponse<TelegramFile> = self.post_json(&url, &body).await?;

        let file = resp.result.ok_or_else(|| {
            GatewayError::ConnectionFailed(
                resp.description.unwrap_or_else(|| "getFile failed".into()),
            )
        })?;

        let file_path = file
            .file_path
            .ok_or_else(|| GatewayError::ConnectionFailed("File path not available".into()))?;

        Ok(format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.config.token, file_path
        ))
    }

    // -----------------------------------------------------------------------
    // Group chat helpers
    // -----------------------------------------------------------------------

    /// Get information about a chat member (useful for admin checks).
    pub async fn get_chat_member(
        &self,
        chat_id: &str,
        user_id: i64,
    ) -> Result<ChatMember, GatewayError> {
        let url = format!("{}/getChatMember", self.api_base);
        let body = serde_json::json!({
            "chat_id": chat_id,
            "user_id": user_id,
        });

        let resp: TelegramResponse<ChatMember> = self.post_json(&url, &body).await?;

        resp.result.ok_or_else(|| {
            GatewayError::Platform(
                resp.description
                    .unwrap_or_else(|| "getChatMember failed".into()),
            )
        })
    }

    /// Check if a user is an admin or creator in a chat.
    pub async fn is_admin(&self, chat_id: &str, user_id: i64) -> Result<bool, GatewayError> {
        let member = self.get_chat_member(chat_id, user_id).await?;
        Ok(matches!(
            member.status.as_str(),
            "administrator" | "creator"
        ))
    }

    /// Check whether a text message mentions this bot (for group filtering).
    ///
    /// Returns `true` if the message contains `@bot_username` or if the
    /// bot_username is not configured (pass-through).
    pub fn is_mentioned_in(&self, text: &str) -> bool {
        match self.config.bot_username {
            Some(ref bot_user) => {
                let mention = format!("@{}", bot_user);
                text.contains(&mention)
            }
            None => true,
        }
    }

    /// Strip the bot mention from text, returning the cleaned message.
    pub fn strip_mention(&self, text: &str) -> String {
        match self.config.bot_username {
            Some(ref bot_user) => {
                let mention = format!("@{}", bot_user);
                text.replace(&mention, "").trim().to_string()
            }
            None => text.to_string(),
        }
    }

    /// Return true if this Telegram document can be processed by parity flows.
    pub fn is_supported_document(doc: &Document) -> bool {
        let ext = doc
            .file_name
            .as_deref()
            .and_then(Self::extract_extension)
            .or_else(|| doc.mime_type.as_deref().and_then(Self::extension_from_mime));
        ext.map(|e| SUPPORTED_DOCUMENT_EXTENSIONS.contains(&e.as_str()))
            .unwrap_or(false)
    }

    /// Return true if this Telegram document exceeds processing size limits.
    pub fn document_exceeds_size_limit(doc: &Document) -> bool {
        doc.file_size
            .map(|sz| sz > TELEGRAM_MAX_DOCUMENT_SIZE_BYTES)
            .unwrap_or(true)
    }

    fn extract_extension(name: &str) -> Option<String> {
        std::path::Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .filter(|e| !e.is_empty())
    }

    fn extension_from_mime(mime: &str) -> Option<String> {
        match mime {
            "application/pdf" => Some("pdf".to_string()),
            "text/markdown" => Some("md".to_string()),
            "text/plain" => Some("txt".to_string()),
            "application/zip" => Some("zip".to_string()),
            "image/png" => Some("png".to_string()),
            "image/jpeg" => Some("jpg".to_string()),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
                Some("docx".to_string())
            }
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
                Some("xlsx".to_string())
            }
            "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
                Some("pptx".to_string())
            }
            _ => None,
        }
    }

}
