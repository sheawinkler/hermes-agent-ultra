#[async_trait]
impl ChannelDirectoryProvider for SlackAdapter {
    fn platform_name(&self) -> &str {
        "slack"
    }

    async fn list_channel_entries(&self) -> Result<Vec<ChannelEntry>, GatewayError> {
        self.list_user_conversations().await
    }
}

#[async_trait]
impl PlatformAdapter for SlackAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Slack adapter starting (token: {})",
            describe_secret(&self.config.token)
        );
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Slack adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.post_message(chat_id, text, None).await?;
        Ok(())
    }

    async fn send_message_threaded(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
        thread_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.post_message(chat_id, text, thread_id).await?;
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        // In Slack, message_id is the `ts` timestamp.
        self.update_message(chat_id, message_id, text).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.upload_file(chat_id, file_path, caption, None).await
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let (blocks, fallback_text) = slack_image_url_blocks(image_url, caption);
        self.post_blocks(chat_id, &blocks, &fallback_text, None)
            .await?;
        Ok(())
    }

    async fn add_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        if !self.reactions_enabled() {
            return Ok(());
        }
        SlackAdapter::add_reaction(self, chat_id, message_id, emoji).await
    }

    async fn remove_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        if !self.reactions_enabled() {
            return Ok(());
        }
        SlackAdapter::remove_reaction(self, chat_id, message_id, emoji).await
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn splits_long_messages(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "slack"
    }
}

