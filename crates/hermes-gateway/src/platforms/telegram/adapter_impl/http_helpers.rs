impl TelegramAdapter {
    /// POST JSON to a Telegram API endpoint and deserialize the response.
    ///
    /// Detects HTTP 429 (rate limited) responses, extracts `retry_after`
    /// from the response body, sleeps, then retries up to
    /// `RATE_LIMIT_MAX_RETRIES` times.
    fn telegram_response_error<T>(method: &str, response: &TelegramResponse<T>) -> GatewayError {
        if let Some(retry_after_secs) = response
            .parameters
            .as_ref()
            .and_then(|parameters| parameters.retry_after)
        {
            return GatewayError::RateLimited {
                retry_after_secs: Some(retry_after_secs),
            };
        }

        GatewayError::SendFailed(
            response
                .description
                .clone()
                .unwrap_or_else(|| format!("{method} failed")),
        )
    }

    fn retry_after_from_telegram_body(text: &str) -> Option<u64> {
        serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|value| value.get("parameters")?.get("retry_after")?.as_u64())
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<TelegramResponse<T>, GatewayError> {
        self.post_json_with_request_timeout(url, body, None).await
    }

    async fn post_json_with_request_timeout<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
        request_timeout: Option<Duration>,
    ) -> Result<TelegramResponse<T>, GatewayError> {
        let mut retries = 0u32;

        loop {
            let mut request = self.client.post(url).json(body);
            if let Some(timeout) = request_timeout {
                request = request.timeout(timeout);
            }
            let resp = request.send().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!("Telegram API request failed: {}", e))
            })?;

            let status = resp.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let text = resp.text().await.unwrap_or_default();

                let retry_after = Self::retry_after_from_telegram_body(&text).unwrap_or(5);

                retries += 1;
                if retries > RATE_LIMIT_MAX_RETRIES {
                    return Err(GatewayError::RateLimited {
                        retry_after_secs: Some(retry_after),
                    });
                }

                warn!(
                    retry_after_secs = retry_after,
                    attempt = retries,
                    "Telegram API rate limited, backing off"
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(retry_after)).await;
                continue;
            }

            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    return Err(GatewayError::RateLimited {
                        retry_after_secs: Self::retry_after_from_telegram_body(&text),
                    });
                }
                return Err(GatewayError::SendFailed(format!(
                    "Telegram API returned HTTP {}: {}",
                    status, text
                )));
            }

            return resp.json::<TelegramResponse<T>>().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Failed to parse Telegram API response: {}",
                    e
                ))
            });
        }
    }

    /// Resolve a `ParseMode` to the Telegram API string.
    fn resolve_parse_mode(&self, parse_mode: Option<ParseMode>) -> Option<&'static str> {
        match parse_mode {
            Some(ParseMode::Markdown) => Some("MarkdownV2"),
            Some(ParseMode::Html) => Some("HTML"),
            Some(ParseMode::Plain) | None => {
                if self.config.parse_markdown {
                    Some("MarkdownV2")
                } else if self.config.parse_html {
                    Some("HTML")
                } else {
                    None
                }
            }
        }
    }

    /// Determine the appropriate send method for a file based on extension.
    fn media_method_for_extension(ext: &str) -> (&'static str, &'static str) {
        match ext {
            "jpg" | "jpeg" | "png" | "webp" => ("sendPhoto", "photo"),
            "gif" => ("sendAnimation", "animation"),
            "mp4" | "mov" | "avi" | "mkv" | "webm" => ("sendVideo", "video"),
            "mp3" | "aac" | "m4a" => ("sendAudio", "audio"),
            "ogg" | "oga" => ("sendVoice", "voice"),
            "webm_sticker" | "tgs" => ("sendSticker", "sticker"),
            _ => ("sendDocument", "document"),
        }
    }
}
