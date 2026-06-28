#[async_trait]
impl ImageGenBackend for FalImageGenBackend {
    async fn generate(&self, request: ImageGenerateRequest) -> Result<String, ToolError> {
        let prepared = self.prepare_request(&request)?;
        let url = self.transport.submit_url(&prepared.endpoint);
        let (auth_name, auth_value) = self.transport.auth_header();

        let resp = self
            .client
            .post(url)
            .header(auth_name, auth_value)
            .header("Content-Type", "application/json")
            .json(&prepared.body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("fal.ai API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read fal.ai response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "fal.ai API error ({}): {}",
                status, text
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse fal.ai response: {}", e))
        })?;

        let images: Vec<Value> = data
            .get("images")
            .and_then(|i| i.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|img| {
                        json!({
                            "url": img.get("url").and_then(|u| u.as_str()).unwrap_or(""),
                            "width": img.get("width").and_then(|w| w.as_u64()).unwrap_or(0),
                            "height": img.get("height").and_then(|h| h.as_u64()).unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let image = images
            .first()
            .and_then(|img| img.get("url"))
            .and_then(Value::as_str)
            .map(Value::from)
            .unwrap_or(Value::Null);

        Ok(json!({
            "success": true,
            "image": image,
            "images": images,
            "modality": prepared.modality,
            "transport": self.transport.label(),
            "model": self.model_path,
            "endpoint": prepared.endpoint,
            "source_images": prepared.source_image_count,
        })
        .to_string())
    }

    fn capabilities(&self) -> ImageGenCapabilities {
        let spec = fal_model_spec(&self.model_path);
        ImageGenCapabilities {
            provider: Some("FAL.ai".to_string()),
            model: Some(
                spec.map(|spec| spec.display)
                    .unwrap_or_else(|| self.model_path.as_str())
                    .to_string(),
            ),
            modalities: if spec.and_then(|spec| spec.edit_endpoint).is_some() {
                vec!["text".to_string(), "image".to_string()]
            } else {
                vec!["text".to_string()]
            },
            max_reference_images: spec
                .filter(|spec| spec.edit_endpoint.is_some())
                .map(|spec| spec.max_reference_images)
                .unwrap_or(0),
        }
    }
}

#[async_trait]
impl ImageGenBackend for OpenAICodexImageGenBackend {
    async fn generate(&self, request: ImageGenerateRequest) -> Result<String, ToolError> {
        if request.has_image_inputs() {
            return Err(ToolError::InvalidParams(
                "OpenAI Codex image generation is text-to-image only in this Rust backend; omit image_url/reference_image_urls or switch to an edit-capable FAL model.".into(),
            ));
        }
        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            return Err(ToolError::InvalidParams(
                "Prompt is required and must be a non-empty string.".into(),
            ));
        }
        let token = self
            .config
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "OpenAI Codex image generation requires Codex OAuth credentials. Run `hermes auth codex` or set HERMES_OPENAI_CODEX_API_KEY.".into(),
                )
            })?;
        let image_size = codex_image_size_from_tool_size(request.size.as_deref());
        let body = codex_image_responses_payload(
            prompt,
            image_size,
            self.config.quality.as_str(),
            self.config.chat_model.as_str(),
        );
        let mut req = self
            .client
            .post(self.responses_url())
            .header("Accept", "text/event-stream")
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(&body);
        for (name, value) in codex_cloudflare_headers(Some(token)) {
            req = req.header(name, value);
        }

        let resp = req.send().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Codex image generation request failed: {e}"))
        })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Codex image response: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Codex Responses API returned HTTP {status}: {}",
                text.chars().take(500).collect::<String>()
            )));
        }
        let image_b64 = collect_codex_image_b64_from_sse(&text)?.ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Codex response contained no image_generation_call result".into(),
            )
        })?;
        let image_path =
            save_codex_image_b64(&image_b64, &self.config.output_dir, &self.config.tier_id)?;
        let image = image_path.to_string_lossy().to_string();
        Ok(json!({
            "success": true,
            "image": image,
            "images": [{
                "url": image,
                "path": image_path.to_string_lossy(),
                "width": 0,
                "height": 0,
            }],
            "provider": "openai-codex",
            "transport": "codex",
            "model": self.config.tier_id,
            "prompt": prompt,
            "size": image_size,
            "quality": self.config.quality,
            "modality": "text",
        })
        .to_string())
    }

    fn capabilities(&self) -> ImageGenCapabilities {
        ImageGenCapabilities {
            provider: Some("openai-codex".to_string()),
            model: Some(self.config.tier_id.clone()),
            modalities: vec!["text".to_string()],
            max_reference_images: 0,
        }
    }
}

#[async_trait]
impl ImageGenBackend for OpenRouterCompatImageGenBackend {
    async fn generate(&self, request: ImageGenerateRequest) -> Result<String, ToolError> {
        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            return Err(ToolError::InvalidParams(
                "Prompt is required and must be a non-empty string.".into(),
            ));
        }
        let token = self
            .config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ToolError::ExecutionFailed(format!(
                    "{} image generation requires credentials. Set {} or configure image_gen.{}.api_key.",
                    self.config.provider.display_name(),
                    self.config.provider.api_key_env_vars().join("/"),
                    self.config.provider.config_key()
                ))
            })?;
        let aspect = openrouter_compat_aspect_from_tool_size(request.size.as_deref());
        let references = openrouter_compat_reference_image_parts(&request)?
            .into_iter()
            .take(OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES)
            .collect::<Vec<_>>();
        let body = openrouter_compat_chat_payload(
            self.config.model.as_str(),
            prompt,
            aspect,
            references.as_slice(),
        );
        let resp = self
            .client
            .post(self.chat_completions_url())
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", OPENROUTER_COMPAT_HTTP_REFERER)
            .header("X-Title", OPENROUTER_COMPAT_X_TITLE)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ToolError::ExecutionFailed(format!(
                        "{} image generation timed out ({}s)",
                        self.config.provider.display_name(),
                        OPENROUTER_COMPAT_TIMEOUT_SECS
                    ))
                } else if e.is_connect() {
                    ToolError::ExecutionFailed(format!(
                        "{} image generation connection error: {e}",
                        self.config.provider.display_name()
                    ))
                } else {
                    ToolError::ExecutionFailed(format!(
                        "{} image generation request failed: {e}",
                        self.config.provider.display_name()
                    ))
                }
            })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "Failed to read {} image response: {e}",
                self.config.provider.display_name()
            ))
        })?;
        if !status.is_success() {
            let message = openrouter_compat_error_message(&text);
            return Err(ToolError::ExecutionFailed(format!(
                "{} image generation failed ({status}): {message}",
                self.config.provider.display_name()
            )));
        }
        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "{} returned invalid JSON: {e}",
                self.config.provider.display_name()
            ))
        })?;
        let images = extract_openrouter_compat_images(&data);
        let first = images.first().ok_or_else(|| {
            ToolError::ExecutionFailed(format!(
                "{} returned no image. Ensure the model '{}' supports image output.",
                self.config.provider.display_name(),
                self.config.model
            ))
        })?;
        let image_path = save_openrouter_compat_generated_image(
            &self.client,
            first,
            &self.config.output_dir,
            self.config.provider.provider_id(),
        )
        .await?;
        let image = image_path.to_string_lossy().to_string();
        let modality = if request.has_image_inputs() {
            "image"
        } else {
            "text"
        };
        Ok(json!({
            "success": true,
            "image": image,
            "images": [{
                "url": image,
                "source_url": first,
                "path": image_path.to_string_lossy(),
                "width": 0,
                "height": 0,
            }],
            "provider": self.config.provider.provider_id(),
            "transport": "openrouter-compatible",
            "model": self.config.model,
            "prompt": prompt,
            "aspect_ratio": aspect,
            "modality": modality,
            "source_images": references.len(),
        })
        .to_string())
    }

    fn capabilities(&self) -> ImageGenCapabilities {
        ImageGenCapabilities {
            provider: Some(self.config.provider.provider_id().to_string()),
            model: Some(self.config.model.clone()),
            modalities: vec!["text".to_string(), "image".to_string()],
            max_reference_images: OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES,
        }
    }
}

#[async_trait]
impl ImageGenBackend for KreaImageGenBackend {
    async fn generate(&self, request: ImageGenerateRequest) -> Result<String, ToolError> {
        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            return Err(ToolError::InvalidParams(
                "Prompt is required and must be a non-empty string.".into(),
            ));
        }
        let token = self.config.transport.auth_token().ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Krea image generation requires credentials. Set KREA_API_KEY or enable the Nous-managed Krea gateway.".into(),
            )
        })?;
        let model = krea_model_spec(&self.config.model)
            .unwrap_or_else(|| krea_model_spec(DEFAULT_KREA_IMAGE_MODEL).expect("default model"));
        let krea_aspect = krea_aspect_from_tool_size(request.size.as_deref());
        let references = krea_style_reference_objects(&request);
        let body = krea_submit_payload(
            prompt,
            krea_aspect,
            self.config.creativity.as_str(),
            references.as_slice(),
        );

        let mut submit = self
            .client
            .post(self.config.transport.submit_url(model))
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("User-Agent", "Hermes-Agent/1.0 (krea-image-gen)")
            .json(&body);
        if self.config.transport.is_managed() {
            submit = submit.header("x-idempotency-key", uuid::Uuid::new_v4().to_string());
        }
        let response = submit.send().await.map_err(|e| {
            if e.is_timeout() {
                ToolError::ExecutionFailed(format!(
                    "Krea submit timed out ({}s)",
                    KREA_SUBMIT_TIMEOUT_SECS
                ))
            } else if e.is_connect() {
                ToolError::ExecutionFailed(format!("Krea connection error: {e}"))
            } else {
                ToolError::ExecutionFailed(format!("Krea image generation request failed: {e}"))
            }
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Krea submit response: {e}"))
        })?;
        if !status.is_success() {
            let message = krea_error_message(&text);
            let error = if self.config.transport.is_managed() && status.is_client_error() {
                let hint = if status.as_u16() == 429 {
                    "Krea's shared-key concurrency cap was hit; retry shortly.".to_string()
                } else {
                    format!(
                        "Model '{}' may not be enabled/priced on the Nous Portal's Krea gateway. Set KREA_API_KEY to use Krea directly, or select a different Krea model.",
                        model.id
                    )
                };
                format!(
                    "Nous Subscription Krea gateway rejected '{}' (HTTP {status}): {message}. {hint}",
                    model.id
                )
            } else {
                format!("Krea image generation failed ({status}): {message}")
            };
            return Err(ToolError::ExecutionFailed(error));
        }
        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Krea returned invalid JSON on submit: {e}"))
        })?;
        let job_id = data
            .get("job_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ToolError::ExecutionFailed("Krea submit response missing job_id".into())
            })?;

        let job = self.poll_krea_job(job_id).await?;
        let last_status = job
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match last_status {
            "failed" => {
                let error = job
                    .get("result")
                    .and_then(|result| result.get("error"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                return Err(ToolError::ExecutionFailed(format!(
                    "Krea job {job_id} failed: {error}"
                )));
            }
            "cancelled" => {
                return Err(ToolError::ExecutionFailed(format!(
                    "Krea job {job_id} was cancelled"
                )));
            }
            _ => {}
        }

        let result_image_url = extract_krea_result_image_url(&job).ok_or_else(|| {
            ToolError::ExecutionFailed("Krea result contained no image URL".into())
        })?;
        let image_ref = match save_openrouter_compat_generated_image(
            &self.client,
            result_image_url.as_str(),
            &self.config.output_dir,
            "krea",
        )
        .await
        {
            Ok(path) => path.to_string_lossy().to_string(),
            Err(err) => {
                tracing::warn!(
                    "Krea image URL {} could not be cached ({}); returning source URL",
                    result_image_url,
                    err
                );
                result_image_url.clone()
            }
        };
        let modality = if references.is_empty() {
            "text"
        } else {
            "image"
        };
        Ok(json!({
            "success": true,
            "image": image_ref,
            "images": [{
                "url": image_ref,
                "source_url": result_image_url,
                "width": 0,
                "height": 0,
            }],
            "provider": "krea",
            "transport": self.config.transport.label(),
            "model": model.id,
            "model_display": model.display,
            "prompt": prompt,
            "aspect_ratio": krea_tool_aspect_from_krea(krea_aspect),
            "krea_aspect_ratio": krea_aspect,
            "resolution": DEFAULT_KREA_RESOLUTION,
            "creativity": self.config.creativity,
            "modality": modality,
            "source_images": references.len(),
            "job_id": job_id,
        })
        .to_string())
    }

    fn capabilities(&self) -> ImageGenCapabilities {
        let model = krea_model_spec(&self.config.model)
            .unwrap_or_else(|| krea_model_spec(DEFAULT_KREA_IMAGE_MODEL).expect("default model"));
        ImageGenCapabilities {
            provider: Some("krea".to_string()),
            model: Some(model.id.to_string()),
            modalities: vec!["text".to_string(), "image".to_string()],
            max_reference_images: KREA_MAX_REFERENCE_IMAGES,
        }
    }
}

impl KreaImageGenBackend {
    async fn poll_krea_job(&self, job_id: &str) -> Result<Value, ToolError> {
        let token = self.config.transport.auth_token().ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Krea image generation requires credentials during poll.".into(),
            )
        })?;
        let deadline = Instant::now() + self.config.poll_timeout;
        let mut interval = self.config.poll_initial_interval;
        let mut last_status: Option<String> = None;

        loop {
            if !interval.is_zero() {
                tokio::time::sleep(interval).await;
            }
            interval = scale_duration(interval, KREA_POLL_BACKOFF, self.config.poll_max_interval);
            let response = self
                .client
                .get(self.config.transport.job_url(job_id))
                .header("Authorization", format!("Bearer {token}"))
                .header("User-Agent", "Hermes-Agent/1.0 (krea-image-gen)")
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(err) if Instant::now() < deadline && (err.is_timeout() || err.is_connect()) => {
                    continue;
                }
                Err(err) => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Krea poll timed out for job {job_id}: {err}"
                    )));
                }
            };
            let status = response.status();
            let text = response.text().await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to read Krea poll response: {e}"))
            })?;
            if !status.is_success() {
                if KREA_RETRYABLE_POLL_STATUSES.contains(&status.as_u16())
                    && Instant::now() < deadline
                {
                    continue;
                }
                return Err(ToolError::ExecutionFailed(format!(
                    "Krea poll failed ({status}) for job {job_id}: {}",
                    krea_error_message(&text)
                )));
            }
            let job: Value = match serde_json::from_str(&text) {
                Ok(job) => job,
                Err(err) if Instant::now() < deadline => {
                    tracing::warn!(
                        "Krea poll returned invalid JSON for job {}: {}",
                        job_id,
                        err
                    );
                    continue;
                }
                Err(err) => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Krea poll returned invalid JSON: {err}"
                    )));
                }
            };
            if let Some(status) = job.get("status").and_then(Value::as_str) {
                last_status = Some(status.to_string());
                if matches!(status, "completed" | "failed" | "cancelled") {
                    return Ok(job);
                }
            }
            if job
                .get("completed_at")
                .is_some_and(|value| !value.is_null())
            {
                return Ok(job);
            }
            if Instant::now() >= deadline {
                return Err(ToolError::ExecutionFailed(format!(
                    "Krea job {job_id} did not complete within {}s (last status: {})",
                    self.config.poll_timeout.as_secs(),
                    last_status.unwrap_or_else(|| "unknown".to_string())
                )));
            }
        }
    }
}

#[async_trait]
impl ImageGenBackend for ImageGenRuntimeBackend {
    async fn generate(&self, request: ImageGenerateRequest) -> Result<String, ToolError> {
        match self {
            Self::Fal(backend) => backend.generate(request).await,
            Self::OpenAICodex(backend) => backend.generate(request).await,
            Self::OpenRouterCompat(backend) => backend.generate(request).await,
            Self::Krea(backend) => backend.generate(request).await,
        }
    }

    fn capabilities(&self) -> ImageGenCapabilities {
        match self {
            Self::Fal(backend) => backend.capabilities(),
            Self::OpenAICodex(backend) => backend.capabilities(),
            Self::OpenRouterCompat(backend) => backend.capabilities(),
            Self::Krea(backend) => backend.capabilities(),
        }
    }
}
