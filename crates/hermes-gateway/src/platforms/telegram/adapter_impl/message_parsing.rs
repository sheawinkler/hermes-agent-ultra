impl TelegramAdapter {
    /// Parse a Telegram Update into an IncomingMessage.
    ///
    /// Handles both regular messages and callback queries.
    pub fn parse_update(update: &Update) -> Option<IncomingMessage> {
        if let Some(ref cq) = update.callback_query {
            return Self::parse_callback_query(cq);
        }

        let msg = update.message.as_ref()?;
        Self::parse_telegram_message(msg, None)
    }

    /// Parse a regular `TelegramMessage` into `IncomingMessage`.
    fn parse_telegram_message(
        msg: &TelegramMessage,
        callback: Option<(&str, &str)>,
    ) -> Option<IncomingMessage> {
        let text = msg
            .text
            .clone()
            .or_else(|| msg.caption.clone())
            .or_else(|| Self::extract_rich_reply_text(msg));
        let reply = msg.reply_to_message.as_deref();
        let own_media = msg.voice.is_some()
            || msg.photo.is_some()
            || msg.sticker.is_some()
            || msg.document.is_some();
        let replied_media = if own_media { None } else { reply };
        let user_id = msg.from.as_ref().map(|u| u.id);
        let username = msg.from.as_ref().and_then(|u| u.username.clone());

        let voice = msg
            .voice
            .as_ref()
            .or_else(|| replied_media.and_then(|r| r.voice.as_ref()));
        let is_voice = voice.is_some();
        let voice_file_id = voice.map(|v| v.file_id.clone());

        let photo = msg
            .photo
            .as_ref()
            .or_else(|| replied_media.and_then(|r| r.photo.as_ref()));
        let is_photo = photo.is_some();
        let photo_file_id = photo.and_then(|photos| photos.last().map(|p| p.file_id.clone()));

        let is_sticker = msg.sticker.is_some();
        let sticker_file_id = msg.sticker.as_ref().map(|s| s.file_id.clone());

        let replied_document = replied_media
            .and_then(|r| r.document.as_ref())
            .filter(|doc| !Self::document_exceeds_size_limit(doc));
        let document = msg.document.as_ref().or(replied_document);
        let is_document = document.is_some();
        let document_file_id = document.map(|d| d.file_id.clone());
        let document_file_name = document.and_then(|d| d.file_name.clone());
        let document_mime_type = document.and_then(|d| d.mime_type.clone());
        let document_file_size = document.and_then(|d| d.file_size);

        let replied_video = replied_media
            .and_then(|r| r.video.as_ref())
            .filter(|video| !Self::video_exceeds_size_limit(video));
        let video = msg
            .video
            .as_ref()
            .filter(|video| !Self::video_exceeds_size_limit(video))
            .or(replied_video);
        let is_video = video.is_some();
        let video_file_id = video.map(|v| v.file_id.clone());
        let video_file_name = video.and_then(|v| v.file_name.clone());
        let video_mime_type = video.and_then(|v| v.mime_type.clone());
        let video_file_size = video.and_then(|v| v.file_size);

        let reply_to_message_id = msg.reply_to_message.as_ref().map(|r| r.message_id);

        let chat_type = ChatKind::from_telegram_type(&msg.chat.chat_type);
        let is_group = chat_type.is_group_like();

        let (cb_id, cb_data) = match callback {
            Some((id, data)) => (Some(id.to_string()), Some(data.to_string())),
            None => (None, None),
        };

        Some(IncomingMessage {
            chat_id: msg.chat.id,
            user_id,
            username,
            text,
            message_id: msg.message_id,
            is_voice,
            is_photo,
            is_sticker,
            is_document,
            is_video,
            voice_file_id,
            photo_file_id,
            sticker_file_id,
            document_file_id,
            document_file_name,
            document_mime_type,
            document_file_size,
            video_file_id,
            video_file_name,
            video_mime_type,
            video_file_size,
            reply_to_message_id,
            message_thread_id: msg.message_thread_id,
            chat_type,
            is_group,
            callback_query_id: cb_id,
            callback_data: cb_data,
        })
    }

    /// Parse a `CallbackQuery` into an `IncomingMessage`.
    fn parse_callback_query(cq: &CallbackQuery) -> Option<IncomingMessage> {
        let msg = cq.message.as_ref();
        let chat_id = msg.map(|m| m.chat.id).unwrap_or(0);
        let message_id = msg.map(|m| m.message_id).unwrap_or(0);

        let chat_type = msg
            .map(|m| ChatKind::from_telegram_type(&m.chat.chat_type))
            .unwrap_or(ChatKind::Private);
        let is_group = chat_type.is_group_like();

        Some(IncomingMessage {
            chat_id,
            user_id: Some(cq.from.id),
            username: cq.from.username.clone(),
            text: cq.data.clone(),
            message_id,
            is_voice: false,
            is_photo: false,
            is_sticker: false,
            is_document: false,
            is_video: false,
            voice_file_id: None,
            photo_file_id: None,
            sticker_file_id: None,
            document_file_id: None,
            document_file_name: None,
            document_mime_type: None,
            document_file_size: None,
            video_file_id: None,
            video_file_name: None,
            video_mime_type: None,
            video_file_size: None,
            reply_to_message_id: None,
            message_thread_id: msg.and_then(|m| m.message_thread_id),
            chat_type,
            is_group,
            callback_query_id: Some(cq.id.clone()),
            callback_data: cq.data.clone(),
        })
    }

    // -----------------------------------------------------------------------
    // File downloads
    // -----------------------------------------------------------------------

}
