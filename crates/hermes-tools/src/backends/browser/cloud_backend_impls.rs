#[async_trait]
impl BrowserBackend for BrowserbaseBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        validate_url_does_not_exfiltrate_secret(url)?;
        let result = self
            .browserbase_command("Page.navigate", json!({"url": url}))
            .await?;
        Ok(json!({"status": "navigated", "url": url, "cdp": result}).to_string())
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        let result = self
            .browserbase_command("Accessibility.getFullAXTree", json!({}))
            .await?;
        Ok(redact_browser_observation(
            &json!({"status": "snapshot", "cdp": result}).to_string(),
        ))
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        let js = format!(
            "document.querySelector('{}')?.click(); 'clicked'",
            selector.replace('\'', "\\'")
        );
        let result = self
            .browserbase_command("Runtime.evaluate", json!({"expression": js}))
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
            .browserbase_command("Runtime.evaluate", json!({"expression": js}))
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
            .browserbase_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "scrolled", "direction": direction, "amount": px, "cdp": result})
                .to_string(),
        )
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        let result = self
            .browserbase_command(
                "Runtime.evaluate",
                json!({"expression": "history.back(); 'back'"}),
            )
            .await?;
        Ok(json!({"status": "navigated_back", "cdp": result}).to_string())
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        let result = self
            .browserbase_command(
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
            .browserbase_command(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true}),
            )
            .await?;
        Ok(json!({"status": "images_found", "selector": sel, "cdp": result}).to_string())
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        let result = self
            .browserbase_command("Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        Ok(browser_vision_payload(instruction, result))
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match action {
            "read" => {
                let result = self
                    .browserbase_command("Runtime.evaluate", json!({
                        "expression": "'Console messages require Runtime.consoleAPICalled event listener'"
                    }))
                    .await?;
                Ok(json!({"status": "console_read", "cdp": result}).to_string())
            }
            "clear" => {
                let result = self
                    .browserbase_command(
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
impl BrowserBackend for BrowserUseBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        validate_url_does_not_exfiltrate_secret(url)?;
        let result = self
            .browser_use_command("Page.navigate", json!({"url": url}))
            .await?;
        Ok(json!({"status": "navigated", "url": url, "cdp": result}).to_string())
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        let result = self
            .browser_use_command("Accessibility.getFullAXTree", json!({}))
            .await?;
        Ok(redact_browser_observation(
            &json!({"status": "snapshot", "cdp": result}).to_string(),
        ))
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        let js = format!(
            "document.querySelector('{}')?.click(); 'clicked'",
            selector.replace('\'', "\\'")
        );
        let result = self
            .browser_use_command("Runtime.evaluate", json!({"expression": js}))
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
            .browser_use_command("Runtime.evaluate", json!({"expression": js}))
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
            .browser_use_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "scrolled", "direction": direction, "amount": px, "cdp": result})
                .to_string(),
        )
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        let result = self
            .browser_use_command(
                "Runtime.evaluate",
                json!({"expression": "history.back(); 'back'"}),
            )
            .await?;
        Ok(json!({"status": "navigated_back", "cdp": result}).to_string())
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        let result = self
            .browser_use_command(
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
            .browser_use_command(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true}),
            )
            .await?;
        Ok(json!({"status": "images_found", "selector": sel, "cdp": result}).to_string())
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        let result = self
            .browser_use_command("Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        Ok(browser_vision_payload(instruction, result))
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match action {
            "read" => {
                let result = self
                    .browser_use_command("Runtime.evaluate", json!({
                        "expression": "'Console messages require Runtime.consoleAPICalled event listener'"
                    }))
                    .await?;
                Ok(json!({"status": "console_read", "cdp": result}).to_string())
            }
            "clear" => {
                let result = self
                    .browser_use_command(
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
impl BrowserBackend for FirecrawlBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        validate_url_does_not_exfiltrate_secret(url)?;
        let result = self
            .firecrawl_command("Page.navigate", json!({"url": url}))
            .await?;
        Ok(json!({"status": "navigated", "url": url, "cdp": result}).to_string())
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        let result = self
            .firecrawl_command("Accessibility.getFullAXTree", json!({}))
            .await?;
        Ok(redact_browser_observation(
            &json!({"status": "snapshot", "cdp": result}).to_string(),
        ))
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        let js = format!(
            "document.querySelector('{}')?.click(); 'clicked'",
            selector.replace('\'', "\\'")
        );
        let result = self
            .firecrawl_command("Runtime.evaluate", json!({"expression": js}))
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
            .firecrawl_command("Runtime.evaluate", json!({"expression": js}))
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
            .firecrawl_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "scrolled", "direction": direction, "amount": px, "cdp": result})
                .to_string(),
        )
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        let result = self
            .firecrawl_command(
                "Runtime.evaluate",
                json!({"expression": "history.back(); 'back'"}),
            )
            .await?;
        Ok(json!({"status": "navigated_back", "cdp": result}).to_string())
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        let result = self
            .firecrawl_command(
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
            .firecrawl_command(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true}),
            )
            .await?;
        Ok(json!({"status": "images_found", "selector": sel, "cdp": result}).to_string())
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        let result = self
            .firecrawl_command("Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        Ok(browser_vision_payload(instruction, result))
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match action {
            "read" => {
                let result = self
                    .firecrawl_command("Runtime.evaluate", json!({
                        "expression": "'Console messages require Runtime.consoleAPICalled event listener'"
                    }))
                    .await?;
                Ok(json!({"status": "console_read", "cdp": result}).to_string())
            }
            "clear" => {
                let result = self
                    .firecrawl_command(
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
