include!("adapter_impl/core_native.rs");
include!("adapter_impl/messaging.rs");
include!("adapter_impl/rooms.rs");
include!("adapter_impl/media_sync.rs");
include!("adapter_impl/sync_parsing.rs");
include!("adapter_impl/event_parsing.rs");

// ---------------------------------------------------------------------------
// PlatformAdapter trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl PlatformAdapter for MatrixAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("Matrix adapter starting (user: {})", self.config.user_id);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Matrix adapter stopping");
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
        self.send_text(chat_id, text, parse_mode).await?;
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        self.edit_text(chat_id, message_id, text).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let path = std::path::Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime = mime_from_extension(ext);
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;

        let size = file_bytes.len();
        let mxc_uri = self.upload_media(file_bytes, file_name, mime).await?;
        self.send_media_message(chat_id, &mxc_uri, file_name, mime, size, caption)
            .await?;
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let downloaded = download_media_url(&self.client, image_url).await;

        let (file_bytes, content_type) = match downloaded {
            Ok(result) => result,
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "Matrix image-url download failed; falling back to text"
                );
                let fallback = image_fallback_text(image_url, caption);
                return self
                    .send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await;
            }
        };

        let file_name = remote_image_file_name(image_url, content_type.as_deref());
        let ext = std::path::Path::new(&file_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png");
        let mime = normalized_image_content_type(content_type.as_deref())
            .unwrap_or_else(|| mime_from_extension(ext).to_string());
        let size = file_bytes.len();
        let mxc_uri = self.upload_media(file_bytes, &file_name, &mime).await?;
        self.send_media_message(chat_id, &mxc_uri, &file_name, &mime, size, caption)
            .await?;
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "matrix"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
