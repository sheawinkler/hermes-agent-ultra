impl App {

    fn contextlattice_ui_status_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_CONTEXTLATTICE_UI_STATUS")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn contextlattice_orchestrator_url() -> String {
        std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
            .ok()
            .or_else(|| std::env::var("MEMMCP_ORCHESTRATOR_URL").ok())
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:8075".to_string())
    }

    fn contextlattice_ping_timeout_secs() -> u64 {
        std::env::var("HERMES_CONTEXTLATTICE_PING_TIMEOUT_SECONDS")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(12)
            .clamp(1, 120)
    }

    async fn emit_contextlattice_connectivity_status(&self) {
        if !Self::contextlattice_ui_status_enabled() {
            return;
        }
        let base = Self::contextlattice_orchestrator_url();
        let url = format!("{}/status", base);
        let topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "runbooks/hermes".to_string());
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!("contextlattice preflight ping {} (topic={})", base, topic),
        );
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(Self::contextlattice_ping_timeout_secs()))
            .build()
        {
            Ok(c) => c,
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!("contextlattice client init failed: {}", err),
                );
                return;
            }
        };
        match client.get(&url).send().await {
            Ok(resp) => {
                let status_code = resp.status();
                if status_code.is_success() {
                    let parsed = resp.json::<serde_json::Value>().await.ok();
                    let service = parsed
                        .as_ref()
                        .and_then(|v| v.get("service").and_then(|s| s.as_str()))
                        .unwrap_or("unknown");
                    let ok_flag = parsed
                        .as_ref()
                        .and_then(|v| v.get("ok").and_then(|s| s.as_bool()))
                        .unwrap_or(true);
                    let detail = if ok_flag { "connected" } else { "degraded" };
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "contextlattice {} (service={} status={} endpoint={})",
                            detail, service, status_code, base
                        ),
                    );
                    Self::emit_phase_event(
                        &self.stream_handle_shared,
                        "context",
                        if ok_flag {
                            "contextlattice connected"
                        } else {
                            "contextlattice degraded"
                        },
                        12,
                    );
                } else {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "contextlattice status endpoint returned {} ({})",
                            status_code, url
                        ),
                    );
                }
            }
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!("contextlattice preflight failed: {} ({})", err, url),
                );
            }
        }
    }


}
