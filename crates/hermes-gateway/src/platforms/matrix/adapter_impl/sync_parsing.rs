impl MatrixAdapter {
    /// Extract messages from joined room timelines in a `/sync` response.
    ///
    /// Handles `m.room.message`, `m.reaction`, and `m.room.encrypted` events.
    async fn parse_sync_events(
        &self,
        sync_response: &serde_json::Value,
    ) -> Vec<IncomingMatrixMessage> {
        let mut messages = Vec::new();

        let rooms = match sync_response.get("rooms").and_then(|r| r.get("join")) {
            Some(join) => join,
            None => return messages,
        };

        let rooms_map = match rooms.as_object() {
            Some(m) => m,
            None => return messages,
        };

        for (room_id, room_data) in rooms_map {
            let events = match room_data
                .get("timeline")
                .and_then(|t| t.get("events"))
                .and_then(|e| e.as_array())
            {
                Some(arr) => arr,
                None => continue,
            };

            for event in events {
                let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let event_id = event
                    .get("event_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let sender = event
                    .get("sender")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                match event_type {
                    "m.room.message" => {
                        if let Some(msg) =
                            self.parse_room_message(room_id, &event_id, &sender, event)
                        {
                            messages.push(msg);
                        }
                    }
                    "m.reaction" => {
                        if let Some(msg) = self.parse_reaction(room_id, &event_id, &sender, event) {
                            messages.push(msg);
                        }
                    }
                    "m.room.encryption" => {
                        self.e2ee.remember_encrypted_room(room_id);
                    }
                    "m.room.encrypted" => {
                        messages.push(
                            self.parse_encrypted_event(room_id, event_id, sender, event)
                                .await,
                        );
                    }
                    _ => {}
                }
            }
        }

        messages
    }

    async fn parse_encrypted_event(
        &self,
        room_id: &str,
        event_id: String,
        sender: String,
        event: &serde_json::Value,
    ) -> IncomingMatrixMessage {
        self.e2ee.remember_encrypted_room(room_id);

        match self.try_native_decrypt_event(room_id, event).await {
            Ok(Some(decrypted)) => {
                debug!(
                    event_id = %event_id,
                    room_id,
                    event_type = %decrypted.event_type,
                    "Decrypted Matrix encrypted event via native runtime"
                );
                return IncomingMatrixMessage {
                    room_id: room_id.to_string(),
                    event_id,
                    sender,
                    body: decrypted.body,
                    event_type: decrypted.event_type,
                    is_edit: decrypted.is_edit,
                    relates_to: decrypted.relates_to,
                };
            }
            Ok(None) => {}
            Err(err) => {
                warn!(
                    event_id = %event_id,
                    room_id,
                    error = %err,
                    "Matrix native decrypt failed; trying fallback paths"
                );
            }
        }

        if let Some(cfg) = &self.decrypt_ffi {
            match self.run_decrypt_ffi(room_id, &event_id, &sender, event, cfg) {
                Ok(decrypted) => {
                    debug!(
                        event_id = %event_id,
                        room_id,
                        event_type = %decrypted.event_type,
                        "Decrypted Matrix encrypted event via FFI"
                    );
                    return IncomingMatrixMessage {
                        room_id: room_id.to_string(),
                        event_id,
                        sender,
                        body: decrypted.body,
                        event_type: decrypted.event_type,
                        is_edit: decrypted.is_edit,
                        relates_to: decrypted.relates_to,
                    };
                }
                Err(err) => {
                    warn!(
                        event_id = %event_id,
                        room_id,
                        error = %err,
                        "Matrix decrypt FFI failed; forwarding encrypted metadata fallback"
                    );
                }
            }
        }

        let body = Self::render_encrypted_event_body(event);
        warn!(
            event_id = %event_id,
            room_id,
            "Received encrypted event — forwarding encrypted metadata"
        );
        IncomingMatrixMessage {
            room_id: room_id.to_string(),
            event_id,
            sender,
            body,
            event_type: "m.room.encrypted".to_string(),
            is_edit: false,
            relates_to: None,
        }
    }

    fn run_decrypt_ffi(
        &self,
        room_id: &str,
        event_id: &str,
        sender: &str,
        event: &serde_json::Value,
        cfg: &MatrixDecryptFfiConfig,
    ) -> Result<MatrixDecryptFfiOutput, String> {
        let payload = serde_json::json!({
            "room_id": room_id,
            "event_id": event_id,
            "sender": sender,
            "event": event,
        });
        let payload_bytes =
            serde_json::to_vec(&payload).map_err(|e| format!("serialize payload failed: {e}"))?;

        let mut child = Command::new(&cfg.command)
            .args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .suppress_windows_console()
            .spawn()
            .map_err(|e| format!("spawn failed for '{}': {e}", cfg.command))?;

        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(&payload_bytes)
                .map_err(|e| format!("write stdin failed: {e}"))?;
            stdin
                .flush()
                .map_err(|e| format!("flush stdin failed: {e}"))?;
        }
        drop(child.stdin.take());

        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    let output = child
                        .wait_with_output()
                        .map_err(|e| format!("wait failed: {e}"))?;
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                        return Err(if stderr.is_empty() {
                            format!("process exited with {}", output.status)
                        } else {
                            format!("process exited with {}: {stderr}", output.status)
                        });
                    }
                    let stdout = String::from_utf8(output.stdout)
                        .map_err(|e| format!("stdout is not valid UTF-8: {e}"))?;
                    return Self::parse_decrypt_ffi_output(stdout.trim());
                }
                Ok(None) => {
                    if started.elapsed() >= cfg.timeout {
                        let _ = child.kill();
                        let output = child.wait_with_output().ok();
                        let stderr = output
                            .as_ref()
                            .map(|out| String::from_utf8_lossy(&out.stderr).trim().to_string())
                            .unwrap_or_default();
                        return Err(if stderr.is_empty() {
                            format!("process timed out after {}ms", cfg.timeout.as_millis())
                        } else {
                            format!(
                                "process timed out after {}ms: {stderr}",
                                cfg.timeout.as_millis()
                            )
                        });
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(format!("try_wait failed: {e}")),
            }
        }
    }

}
