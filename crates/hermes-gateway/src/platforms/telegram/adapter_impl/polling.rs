impl TelegramAdapter {
    /// Fetch updates from Telegram using long polling.
    pub async fn get_updates(&self) -> Result<Vec<Update>, GatewayError> {
        let offset = self.poll_offset.load(Ordering::SeqCst);
        let url = format!("{}/getUpdates", self.api_base);

        let body = serde_json::json!({
            "offset": offset,
            "timeout": self.config.poll_timeout,
            "allowed_updates": ["message", "callback_query"],
        });

        let resp: TelegramResponse<Vec<Update>> = self
            .post_json_with_request_timeout(&url, &body, Some(self.poll_request_timeout()))
            .await?;

        if let Some(updates) = resp.result {
            if let Some(last) = updates.last() {
                self.poll_offset.store(last.update_id + 1, Ordering::SeqCst);
            }
            Ok(updates)
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn delete_webhook(&self, drop_pending_updates: bool) -> Result<(), GatewayError> {
        let url = format!("{}/deleteWebhook", self.api_base);
        let body = serde_json::json!({ "drop_pending_updates": drop_pending_updates });
        let resp: TelegramResponse<bool> = self.post_json(&url, &body).await?;
        if resp.ok {
            Ok(())
        } else {
            Err(GatewayError::SendFailed(
                resp.description
                    .unwrap_or_else(|| "deleteWebhook failed".to_string()),
            ))
        }
    }

    /// Fetch updates with exponential backoff on failures.
    ///
    /// On success the backoff resets to zero. On failure the delay doubles
    /// each time (1 s → 2 s → 4 s … capped at 60 s). The caller can inspect
    /// `PollResult::Backoff` and decide whether to sleep or abort.
    pub async fn poll_with_backoff(&self) -> PollResult {
        match self.get_updates().await {
            Ok(updates) => {
                self.backoff_ms.store(0, Ordering::SeqCst);
                self.consecutive_errors.store(0, Ordering::SeqCst);
                PollResult::Updates(updates)
            }
            Err(e) => {
                let prev = self.backoff_ms.load(Ordering::SeqCst);
                let next = if prev == 0 {
                    INITIAL_BACKOFF_MS
                } else {
                    (prev * 2).min(MAX_BACKOFF_MS)
                };
                self.backoff_ms.store(next, Ordering::SeqCst);

                let err_count = self.consecutive_errors.fetch_add(1, Ordering::SeqCst) + 1;
                let conflict = Self::is_polling_conflict_error(&e);
                if conflict {
                    self.mark_polling_unhealthy();
                }
                warn!(
                    consecutive_errors = err_count,
                    backoff_ms = next,
                    polling_conflict = conflict,
                    "Telegram poll failed: {}",
                    e
                );

                PollResult::Backoff {
                    error: e,
                    delay_ms: next,
                }
            }
        }
    }

    /// Convenience: sleep for the backoff delay. Should be called after
    /// receiving `PollResult::Backoff`.
    pub async fn sleep_backoff(&self) {
        let ms = self.backoff_ms.load(Ordering::SeqCst);
        if ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
        }
    }

    /// Return the current consecutive error count.
    pub fn consecutive_error_count(&self) -> u64 {
        self.consecutive_errors.load(Ordering::SeqCst)
    }

    pub fn polling_reconnect_threshold_reached(&self, threshold: u64) -> bool {
        threshold > 0 && self.consecutive_error_count() >= threshold
    }

    pub fn mark_polling_unhealthy(&self) {
        self.base.mark_stopped();
    }

    pub fn poll_request_timeout(&self) -> Duration {
        Duration::from_secs(
            self.config
                .poll_timeout
                .saturating_add(poll_stall_grace_seconds())
                .max(1),
        )
    }

    pub fn is_polling_conflict_error(err: &GatewayError) -> bool {
        let message = err.to_string().to_ascii_lowercase();
        message.contains("409")
            || message.contains("conflict")
            || message.contains("terminated by other getupdates request")
    }

    // -----------------------------------------------------------------------
    // Update parsing
    // -----------------------------------------------------------------------

}
