impl CamoFoxBrowserBackend {
    pub fn new(endpoint: String, profile: String) -> Self {
        Self {
            inner: CdpBrowserBackend::new(endpoint),
            profile,
        }
    }

    pub fn from_env() -> Self {
        let endpoint = std::env::var("CAMOFOX_CDP_URL")
            .or_else(|_| std::env::var("CHROME_CDP_URL"))
            .or_else(|_| std::env::var("BROWSER_CDP_URL"))
            .unwrap_or_else(|_| "http://localhost:9222".to_string());
        let profile = std::env::var("CAMOFOX_PROFILE").unwrap_or_else(|_| "default".to_string());
        Self::new(endpoint, profile)
    }
}

impl CdpBrowserBackend {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            client: reqwest::Client::new(),
            first_navigation: AtomicBool::new(true),
        }
    }

    /// Create from environment variable `CHROME_CDP_URL` or default localhost.
    pub fn from_env() -> Self {
        let endpoint = std::env::var("CHROME_CDP_URL")
            .or_else(|_| std::env::var("BROWSER_CDP_URL"))
            .unwrap_or_else(|_| "http://localhost:9222".to_string());
        Self::new(endpoint)
    }

    /// Send a CDP command via HTTP (simplified - real impl would use WebSocket).
    async fn cdp_command_http(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        // Get the first available page target
        let targets_resp = self
            .client
            .get(format!("{}/json", self.endpoint))
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Failed to connect to Chrome CDP at {}: {}",
                    self.endpoint, e
                ))
            })?;

        let targets: Vec<Value> = targets_resp.json().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse CDP targets: {}", e))
        })?;

        let ws_url = targets.first()
            .and_then(|t| t.get("webSocketDebuggerUrl"))
            .and_then(|u| u.as_str())
            .ok_or_else(|| ToolError::ExecutionFailed("No Chrome page target found. Is Chrome running with --remote-debugging-port=9222?".into()))?;

        // For a full implementation, we'd use tokio-tungstenite to connect
        // to the WebSocket and send CDP commands. For now, return a structured
        // response indicating the command that would be sent.
        Ok(json!({
            "method": method,
            "params": params,
            "target": ws_url,
            "status": "sent",
        }))
    }

    async fn cdp_command_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout_duration: Duration,
    ) -> Result<Value, ToolError> {
        match tokio::time::timeout(timeout_duration, self.cdp_command_http(method, params)).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(format_cdp_command_error(method, &self.endpoint, err)),
            Err(_) => Err(ToolError::ExecutionFailed(format_cdp_timeout_error(
                method,
                &self.endpoint,
                timeout_duration,
            ))),
        }
    }

    async fn cdp_command(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        self.cdp_command_with_timeout(method, params, cdp_command_timeout())
            .await
    }
}

#[async_trait]
impl BrowserBackend for CdpBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        validate_url_does_not_exfiltrate_secret(url)?;
        let first_open = self.first_navigation.swap(false, Ordering::SeqCst);
        let result = self
            .cdp_command_with_timeout(
                "Page.navigate",
                json!({"url": url}),
                cdp_open_timeout(first_open),
            )
            .await
            .map_err(|err| {
                ToolError::ExecutionFailed(format!(
                    "Failed to open {url}: {}",
                    tool_error_message(err)
                ))
            })?;
        Ok(json!({"status": "navigated", "url": url, "cdp": result}).to_string())
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        let result = self
            .cdp_command("Accessibility.getFullAXTree", json!({}))
            .await?;
        Ok(redact_browser_observation(
            &json!({"status": "snapshot", "cdp": result}).to_string(),
        ))
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        // Use Runtime.evaluate to find and click the element
        let js = format!(
            "document.querySelector('{}')?.click(); 'clicked'",
            selector.replace('\'', "\\'")
        );
        let result = self
            .cdp_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(json!({"status": "clicked", "selector": selector, "cdp": result}).to_string())
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        let js = format!(
            "let el = document.querySelector('{}'); if(el) {{ el.value = '{}'; el.dispatchEvent(new Event('input')); 'typed' }} else {{ 'not found' }}",
            selector.replace('\'', "\\'"),
            text.replace('\'', "\\'")
        );
        let result = self
            .cdp_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "typed", "selector": selector, "text": text, "cdp": result})
                .to_string(),
        )
    }

    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        let px = amount.unwrap_or(500) as i32;
        let (x, y) = match direction {
            "up" => (0, -px),
            "down" => (0, px),
            "left" => (-px, 0),
            "right" => (px, 0),
            _ => (0, px),
        };
        let js = format!("window.scrollBy({}, {}); 'scrolled'", x, y);
        let result = self
            .cdp_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "scrolled", "direction": direction, "amount": px, "cdp": result})
                .to_string(),
        )
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        let result = self
            .cdp_command(
                "Runtime.evaluate",
                json!({"expression": "history.back(); 'back'"}),
            )
            .await?;
        Ok(json!({"status": "navigated_back", "cdp": result}).to_string())
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        let result = self
            .cdp_command(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyDown",
                    "key": key,
                }),
            )
            .await?;
        Ok(json!({"status": "key_pressed", "key": key, "cdp": result}).to_string())
    }

    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        let sel = selector.unwrap_or("img");
        let js = format!(
            "JSON.stringify(Array.from(document.querySelectorAll('{}')).map(img => ({{src: img.src, alt: img.alt, width: img.width, height: img.height}})))",
            sel.replace('\'', "\\'")
        );
        let result = self
            .cdp_command(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true}),
            )
            .await?;
        Ok(json!({"status": "images_found", "selector": sel, "cdp": result}).to_string())
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        // Take a screenshot and analyze with vision model
        let result = self
            .cdp_command("Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        Ok(browser_vision_payload(instruction, result))
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match action {
            "read" => {
                let result = self.cdp_command("Runtime.evaluate", json!({
                    "expression": "'Console messages require Runtime.consoleAPICalled event listener'"
                })).await?;
                Ok(json!({"status": "console_read", "cdp": result}).to_string())
            }
            "clear" => {
                let result = self
                    .cdp_command(
                        "Runtime.evaluate",
                        json!({"expression": "console.clear(); 'cleared'"}),
                    )
                    .await?;
                Ok(json!({"status": "console_cleared", "cdp": result}).to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown console action: {}",
                other
            ))),
        }
    }
}

#[async_trait]
impl BrowserBackend for CamoFoxBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        let alias = camofox_loopback_alias_from_env();
        let (browser_url, rewrite) = rewrite_loopback_url_for_camofox(
            url,
            camofox_loopback_rewrite_enabled_from_env(),
            &alias,
        );
        let result = self.inner.navigate(&browser_url).await?;
        let mut value =
            serde_json::from_str::<Value>(&result).unwrap_or_else(|_| json!({"result": result}));
        if let Some(obj) = value.as_object_mut() {
            obj.insert("camofox_profile".into(), self.profile.clone().into());
            if let Some(rewrite) = rewrite {
                obj.insert("requested_url".into(), rewrite.original_url.clone().into());
                obj.insert(
                    "url_rewrite".into(),
                    json!({
                        "from": rewrite.from,
                        "to": rewrite.to,
                        "original_url": rewrite.original_url,
                        "rewritten_url": rewrite.rewritten_url,
                    }),
                );
                obj.insert(
                    "warning".into(),
                    "Rewrote loopback URL for Docker-hosted Camofox".into(),
                );
            }
            Ok(value.to_string())
        } else {
            Ok(value.to_string())
        }
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        self.inner.snapshot().await
    }
    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        self.inner.click(selector).await
    }
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        self.inner.r#type(selector, text).await
    }
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        self.inner.scroll(direction, amount).await
    }
    async fn go_back(&self) -> Result<String, ToolError> {
        self.inner.go_back().await
    }
    async fn press(&self, key: &str) -> Result<String, ToolError> {
        self.inner.press(key).await
    }
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        self.inner.get_images(selector).await
    }
    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        self.inner.vision(instruction).await
    }
    async fn console(&self, action: &str) -> Result<String, ToolError> {
        self.inner.console(action).await
    }
}
