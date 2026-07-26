// ---------------------------------------------------------------------------
// Socket Mode runtime
// ---------------------------------------------------------------------------
//
// SocketModeSession deliberately owns parsing and acknowledgement accounting.
// This module owns the long-lived transport: opening a fresh Slack-provided
// URL, acknowledging envelopes before dispatch, reconnecting after transport
// failures, and handing typed ingress to the Gateway-owned channel.

const SLACK_SOCKET_RETRY_INITIAL: Duration = Duration::from_secs(1);
const SLACK_SOCKET_RETRY_MAX: Duration = Duration::from_secs(30);

impl SlackAdapter {
    /// Install the Gateway ingress sender used by the Socket Mode runtime.
    ///
    /// Socket Mode is a receive transport, not an outbound-only Slack client.
    /// Requiring this sender before start keeps a configured Socket Mode
    /// adapter from acknowledging inbound events that no Gateway can process.
    pub async fn set_inbound_sender(&self, sender: mpsc::Sender<crate::gateway::IncomingMessage>) {
        *self.inbound_tx.write().await = Some(sender);
    }

    pub(crate) async fn start_socket_mode_runtime(&self) -> Result<(), GatewayError> {
        if !self.config.socket_mode {
            return Ok(());
        }
        if self.inbound_tx.read().await.is_none() {
            return Err(GatewayError::Platform(
                "Slack Socket Mode requires a Gateway inbound sender before adapter start".into(),
            ));
        }

        let mut task = self.runtime_task.lock().await;
        if task.as_ref().is_some_and(|task| !task.is_finished()) {
            return Ok(());
        }
        if let Some(completed) = task.take() {
            let _ = completed.await;
        }

        let client = self.client.clone();
        let config = self.config.clone();
        let inbound_tx = Arc::clone(&self.inbound_tx);
        let stop_signal = Arc::clone(&self.stop_signal);
        *task = Some(tokio::spawn(async move {
            run_slack_socket_mode(client, config, inbound_tx, stop_signal).await;
        }));
        Ok(())
    }

    pub(crate) async fn stop_socket_mode_runtime(&self) {
        self.stop_signal.notify_waiters();
        let task = self.runtime_task.lock().await.take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }
}

async fn slack_socket_mode_url(
    client: &Client,
    app_token: Option<&str>,
) -> Result<String, GatewayError> {
    let app_token = app_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| GatewayError::Auth("Socket Mode requires an app-level token (xapp-...)".into()))?;

    let resp = client
        .post(format!("{SLACK_API_BASE}/apps.connections.open"))
        .header("Authorization", format!("Bearer {app_token}"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await
        .map_err(|error| {
            GatewayError::ConnectionFailed(format!(
                "Failed to open Slack Socket Mode connection: {error}"
            ))
        })?;

    let body: serde_json::Value = resp.json().await.map_err(|error| {
        GatewayError::ConnectionFailed(format!(
            "Failed to parse Slack Socket Mode response: {error}"
        ))
    })?;
    if body.get("ok").and_then(|value| value.as_bool()) != Some(true) {
        let error = body
            .get("error")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        return Err(GatewayError::ConnectionFailed(format!(
            "Slack Socket Mode connection failed: {error}"
        )));
    }

    body.get("url")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| GatewayError::ConnectionFailed("No URL in Slack Socket Mode response".into()))
}

async fn run_slack_socket_mode(
    client: Client,
    config: SlackConfig,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<crate::gateway::IncomingMessage>>>>,
    stop_signal: Arc<Notify>,
) {
    let mut retry_delay = SLACK_SOCKET_RETRY_INITIAL;

    loop {
        let socket_url = tokio::select! {
            _ = stop_signal.notified() => return,
            result = slack_socket_mode_url(&client, config.app_token.as_deref()) => result,
        };
        let socket_url = match socket_url {
            Ok(url) => url,
            Err(error) => {
                warn!(error = %error, "Slack Socket Mode URL request failed; retrying");
                if !wait_for_slack_socket_retry(&stop_signal, retry_delay).await {
                    return;
                }
                retry_delay = retry_delay.saturating_mul(2).min(SLACK_SOCKET_RETRY_MAX);
                continue;
            }
        };

        let connection = tokio::select! {
            _ = stop_signal.notified() => return,
            result = tokio_tungstenite::connect_async(socket_url) => result,
        };
        let (socket, _) = match connection {
            Ok(connection) => connection,
            Err(error) => {
                warn!(error = %error, "Slack Socket Mode WebSocket connection failed; retrying");
                if !wait_for_slack_socket_retry(&stop_signal, retry_delay).await {
                    return;
                }
                retry_delay = retry_delay.saturating_mul(2).min(SLACK_SOCKET_RETRY_MAX);
                continue;
            }
        };

        let completed_handshake = run_slack_socket_connection(
            socket,
            &config,
            Arc::clone(&inbound_tx),
            Arc::clone(&stop_signal),
        )
        .await;
        if completed_handshake {
            retry_delay = SLACK_SOCKET_RETRY_INITIAL;
        } else {
            retry_delay = retry_delay.saturating_mul(2).min(SLACK_SOCKET_RETRY_MAX);
        }
        if !wait_for_slack_socket_retry(&stop_signal, retry_delay).await {
            return;
        }
    }
}

async fn wait_for_slack_socket_retry(stop_signal: &Notify, delay: Duration) -> bool {
    tokio::select! {
        _ = stop_signal.notified() => false,
        _ = tokio::time::sleep(delay) => true,
    }
}

async fn run_slack_socket_connection(
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    config: &SlackConfig,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<crate::gateway::IncomingMessage>>>>,
    stop_signal: Arc<Notify>,
) -> bool {
    let (mut writer, mut reader) = socket.split();
    let mut session = SocketModeSession::with_config(config);
    session.mark_connecting();
    let mut completed_handshake = false;

    loop {
        let frame = tokio::select! {
            _ = stop_signal.notified() => {
                session.mark_closing();
                let _ = writer.send(WebSocketMessage::Close(None)).await;
                return completed_handshake;
            }
            frame = reader.next() => frame,
        };
        let Some(frame) = frame else {
            warn!("Slack Socket Mode WebSocket closed unexpectedly");
            return completed_handshake;
        };
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => {
                warn!(error = %error, "Slack Socket Mode WebSocket read failed");
                return completed_handshake;
            }
        };

        let payload = match frame {
            WebSocketMessage::Text(payload) => payload.to_string(),
            WebSocketMessage::Binary(payload) => match String::from_utf8(payload.to_vec()) {
                Ok(payload) => payload,
                Err(_) => {
                    warn!("Slack Socket Mode delivered a non-UTF-8 binary envelope");
                    continue;
                }
            },
            WebSocketMessage::Ping(payload) => {
                if let Err(error) = writer.send(WebSocketMessage::Pong(payload)).await {
                    warn!(error = %error, "Slack Socket Mode pong failed");
                    return completed_handshake;
                }
                continue;
            }
            WebSocketMessage::Pong(_) => continue,
            WebSocketMessage::Close(_) => return completed_handshake,
            WebSocketMessage::Frame(_) => continue,
        };

        let envelope = match serde_json::from_str::<SocketModeEnvelope>(&payload) {
            Ok(envelope) => envelope,
            Err(error) => {
                warn!(error = %error, "Slack Socket Mode delivered an invalid JSON envelope");
                continue;
            }
        };

        // Slack considers an envelope delivered only after this response. Ack
        // before queueing work so Gateway backpressure cannot cause duplicate
        // agent turns or a Socket Mode timeout.
        if let Some(envelope_id) = envelope.envelope_id.as_deref() {
            let ack = SocketModeSession::build_ack_payload(envelope_id);
            if let Err(error) = writer.send(WebSocketMessage::Text(ack.into())).await {
                warn!(error = %error, "Slack Socket Mode acknowledgement failed");
                return completed_handshake;
            }
        }

        match session.handle_envelope(&envelope) {
            SocketModeAction::Ack | SocketModeAction::Ignore => {}
            SocketModeAction::MessageEvent(message) => {
                forward_slack_gateway_message(&inbound_tx, slack_gateway_message(message), &stop_signal)
                    .await;
            }
            SocketModeAction::SlashCommand(command) => {
                if let Some(message) = slack_slash_gateway_message(command) {
                    forward_slack_gateway_message(&inbound_tx, message, &stop_signal).await;
                } else {
                    warn!("Slack Socket Mode received a slash command without channel or user identity");
                }
            }
            SocketModeAction::InteractiveEvent(payload) => {
                if let Some(message) = slack_interactive_gateway_message(payload) {
                    forward_slack_gateway_message(&inbound_tx, message, &stop_signal).await;
                }
            }
        }

        if session.state() == SocketModeConnectionState::Connected {
            completed_handshake = true;
        }
        if session.state() == SocketModeConnectionState::Closing {
            return completed_handshake;
        }
    }
}

async fn forward_slack_gateway_message(
    inbound_tx: &Arc<RwLock<Option<mpsc::Sender<crate::gateway::IncomingMessage>>>>,
    message: crate::gateway::IncomingMessage,
    stop_signal: &Notify,
) {
    let sender = inbound_tx.read().await.clone();
    let Some(sender) = sender else {
        warn!("Slack Socket Mode ingress sender disappeared; event was not dispatched");
        return;
    };
    tokio::select! {
        _ = stop_signal.notified() => {}
        result = sender.send(message) => {
            if result.is_err() {
                warn!("Slack Socket Mode Gateway ingress receiver stopped; event was not dispatched");
            }
        }
    }
}

fn slack_gateway_message(message: IncomingSlackMessage) -> crate::gateway::IncomingMessage {
    crate::gateway::IncomingMessage {
        platform: "slack".to_string(),
        chat_id: message.channel,
        user_id: message.user_id.unwrap_or_default(),
        text: message.text,
        message_id: Some(message.ts),
        thread_id: message.thread_ts,
        is_dm: message.is_dm,
    }
}

fn slack_slash_gateway_message(command: SlashCommandPayload) -> Option<crate::gateway::IncomingMessage> {
    let chat_id = command.channel_id?.trim().to_string();
    let user_id = command.user_id?.trim().to_string();
    if chat_id.is_empty() || user_id.is_empty() {
        return None;
    }
    let command_name = command.command.trim();
    if command_name.is_empty() {
        return None;
    }
    let command_name = if command_name.starts_with('/') {
        command_name.to_string()
    } else {
        format!("/{command_name}")
    };
    let argument = command.text.as_deref().map(str::trim).unwrap_or_default();
    let text = if argument.is_empty() {
        command_name
    } else {
        format!("{command_name} {argument}")
    };

    Some(crate::gateway::IncomingMessage {
        platform: "slack".to_string(),
        is_dm: slack_channel_is_dm(&chat_id),
        chat_id,
        user_id,
        text,
        message_id: command.trigger_id,
        thread_id: None,
    })
}

fn slack_interactive_gateway_message(
    payload: InteractivePayload,
) -> Option<crate::gateway::IncomingMessage> {
    let action = payload.actions.first()?;
    let action_id = action.action_id.as_deref().unwrap_or_default();
    let text = match action_id {
        "hermes_approve_once" => "/approve".to_string(),
        "hermes_approve_session" => "/approve session".to_string(),
        "hermes_approve_always" => "/approve always".to_string(),
        "hermes_deny" => "/deny".to_string(),
        // Plugin/custom handlers may encode an existing gateway slash command
        // as the button value. Never reinterpret arbitrary values as prompts.
        _ => action
            .value
            .as_deref()
            .map(str::trim)
            .filter(|value| value.starts_with('/'))
            .map(ToOwned::to_owned)?,
    };
    let chat_id = payload.channel?.id;
    let user_id = payload.user?.id;
    if chat_id.trim().is_empty() || user_id.trim().is_empty() {
        return None;
    }
    let message_id = payload
        .message
        .as_ref()
        .and_then(|message| message.get("ts"))
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    let thread_id = payload
        .message
        .as_ref()
        .and_then(|message| message.get("thread_ts"))
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);

    Some(crate::gateway::IncomingMessage {
        platform: "slack".to_string(),
        is_dm: slack_channel_is_dm(&chat_id),
        chat_id,
        user_id,
        text,
        message_id,
        thread_id,
    })
}

fn slack_channel_is_dm(channel_id: &str) -> bool {
    channel_id.starts_with('D')
}
