impl MatrixAdapter {
    fn parse_decrypt_ffi_output(stdout: &str) -> Result<MatrixDecryptFfiOutput, String> {
        if stdout.is_empty() {
            return Err("empty stdout from decrypt FFI".to_string());
        }

        if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) {
            if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
                return Err(format!("decrypt FFI error: {err}"));
            }

            let relates_to = value
                .get("relates_to")
                .or_else(|| value.get("m.relates_to"))
                .or_else(|| value.get("content").and_then(|c| c.get("m.relates_to")))
                .and_then(Self::parse_relates_to_json);
            let is_edit = value
                .get("is_edit")
                .and_then(|v| v.as_bool())
                .unwrap_or_else(|| {
                    relates_to
                        .as_ref()
                        .map(|r| r.rel_type == "m.replace")
                        .unwrap_or(false)
                });
            let body = if is_edit {
                value
                    .get("m.new_content")
                    .and_then(|nc| nc.get("body"))
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        value
                            .get("content")
                            .and_then(|c| c.get("m.new_content"))
                            .and_then(|nc| nc.get("body"))
                            .and_then(|v| v.as_str())
                    })
                    .or_else(|| value.get("body").and_then(|v| v.as_str()))
                    .or_else(|| {
                        value
                            .get("content")
                            .and_then(|c| c.get("body"))
                            .and_then(|v| v.as_str())
                    })
            } else {
                value
                    .get("body")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        value
                            .get("content")
                            .and_then(|c| c.get("body"))
                            .and_then(|v| v.as_str())
                    })
                    .or_else(|| {
                        value
                            .get("content")
                            .and_then(|c| c.get("m.new_content"))
                            .and_then(|nc| nc.get("body"))
                            .and_then(|v| v.as_str())
                    })
            }
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

            if body.is_empty() {
                return Err("decrypt FFI JSON missing non-empty body".to_string());
            }

            let event_type = value
                .get("event_type")
                .or_else(|| value.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("m.room.message")
                .to_string();

            return Ok(MatrixDecryptFfiOutput {
                body,
                event_type,
                is_edit,
                relates_to,
            });
        }

        Ok(MatrixDecryptFfiOutput {
            body: stdout.to_string(),
            event_type: "m.room.message".to_string(),
            is_edit: false,
            relates_to: None,
        })
    }

    fn parse_relates_to_json(value: &serde_json::Value) -> Option<RelatesTo> {
        let rel_type = value
            .get("rel_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let event_id = value
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if rel_type.is_empty() || event_id.is_empty() {
            return None;
        }

        let key = value.get("key").and_then(|v| v.as_str()).map(String::from);
        Some(RelatesTo {
            rel_type,
            event_id,
            key,
        })
    }

    fn render_encrypted_event_body(event: &serde_json::Value) -> String {
        let content = event.get("content").cloned().unwrap_or_default();
        if let Some(body) = content.get("body").and_then(|v| v.as_str()) {
            if !body.trim().is_empty() {
                return body.to_string();
            }
        }

        let mut meta = Vec::new();
        if let Some(algorithm) = content.get("algorithm").and_then(|v| v.as_str()) {
            meta.push(format!("algorithm={algorithm}"));
        }
        if let Some(sender_key) = content.get("sender_key").and_then(|v| v.as_str()) {
            meta.push(format!("sender_key={sender_key}"));
        }
        if let Some(device_id) = content.get("device_id").and_then(|v| v.as_str()) {
            meta.push(format!("device_id={device_id}"));
        }
        if let Some(session_id) = content.get("session_id").and_then(|v| v.as_str()) {
            meta.push(format!("session_id={session_id}"));
        }

        if meta.is_empty() {
            "[encrypted event]".to_string()
        } else {
            format!("[encrypted event: {}]", meta.join(", "))
        }
    }

    fn parse_room_message(
        &self,
        room_id: &str,
        event_id: &str,
        sender: &str,
        event: &serde_json::Value,
    ) -> Option<IncomingMatrixMessage> {
        let content = event.get("content")?;

        let relates_to_val = content.get("m.relates_to");
        let rel_type = relates_to_val
            .and_then(|r| r.get("rel_type"))
            .and_then(|v| v.as_str());
        let is_edit = rel_type == Some("m.replace");

        let relates_to = relates_to_val.map(|r| RelatesTo {
            rel_type: r
                .get("rel_type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            event_id: r
                .get("event_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            key: r.get("key").and_then(|v| v.as_str()).map(String::from),
        });

        let body = if is_edit {
            content
                .get("m.new_content")
                .and_then(|nc| nc.get("body"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            content
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        Some(IncomingMatrixMessage {
            room_id: room_id.to_string(),
            event_id: event_id.to_string(),
            sender: sender.to_string(),
            body,
            event_type: "m.room.message".to_string(),
            is_edit,
            relates_to,
        })
    }

    fn parse_reaction(
        &self,
        room_id: &str,
        event_id: &str,
        sender: &str,
        event: &serde_json::Value,
    ) -> Option<IncomingMatrixMessage> {
        let content = event.get("content")?;
        let relates_to_val = content.get("m.relates_to")?;

        let target_event = relates_to_val
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let key = relates_to_val
            .get("key")
            .and_then(|v| v.as_str())
            .map(String::from);

        Some(IncomingMatrixMessage {
            room_id: room_id.to_string(),
            event_id: event_id.to_string(),
            sender: sender.to_string(),
            body: key.clone().unwrap_or_default(),
            event_type: "m.reaction".to_string(),
            is_edit: false,
            relates_to: Some(RelatesTo {
                rel_type: "m.annotation".to_string(),
                event_id: target_event,
                key,
            }),
        })
    }

    /// Extract room IDs from the `invite` section of a sync response.
    fn parse_invites(&self, sync_response: &serde_json::Value) -> Vec<String> {
        sync_response
            .get("rooms")
            .and_then(|r| r.get("invite"))
            .and_then(|inv| inv.as_object())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Returns `true` if the background sync loop is active.
    pub fn is_sync_running(&self) -> bool {
        self.sync_running.load(Ordering::SeqCst)
    }
}
