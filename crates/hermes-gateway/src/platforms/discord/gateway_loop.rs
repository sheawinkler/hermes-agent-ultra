//! Discord Gateway WebSocket driver.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use reqwest::Client;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;

use super::config::{DiscordConfig, DISCORD_API_BASE};
use super::dedup::MessageDedup;
use super::filter::{DiscordInboundConfig, should_accept_message};
use super::parse::{
    interaction_to_incoming, parse_interaction_create, parse_message_create_raw, raw_to_incoming,
    ready_bot_user_id,
};
use super::session::{
    GatewayAction, GatewayPayload, GatewaySession, IdentifyData, IdentifyProperties, ResumeData,
    opcodes,
};
use crate::adapter::BasePlatformAdapter;
use crate::gateway::IncomingMessage;

const RECONNECT_SECS: &[u64] = &[2, 5, 10, 30, 60];

pub struct DiscordInner {
    pub config: DiscordConfig,
    pub client: Client,
    pub base: BasePlatformAdapter,
    pub inbound_tx: RwLock<Option<mpsc::Sender<IncomingMessage>>>,
    pub bot_user_id: RwLock<Option<String>>,
    pub dedup: RwLock<MessageDedup>,
    pub stop: tokio::sync::Notify,
}

/// Fetch the Discord Gateway WebSocket URL.
pub async fn fetch_gateway_url(client: &Client, token: &str) -> Result<String, GatewayError> {
    fetch_gateway_url_at(client, token, DISCORD_API_BASE).await
}

/// Same as [`fetch_gateway_url`] but with an injectable API base (tests / mocks).
pub async fn fetch_gateway_url_at(
    client: &Client,
    token: &str,
    api_base: &str,
) -> Result<String, GatewayError> {
    let url = format!("{api_base}/gateway");
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bot {token}"))
        .send()
        .await
        .map_err(|e| GatewayError::ConnectionFailed(format!("Discord GET /gateway: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(GatewayError::ConnectionFailed(format!(
            "Discord GET /gateway HTTP {status}: {text}"
        )));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| {
        GatewayError::ConnectionFailed(format!("Discord GET /gateway json: {e}"))
    })?;
    body.get("url")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| GatewayError::ConnectionFailed("Discord GET /gateway missing url".into()))
}

/// Process one gateway text frame; returns inbound messages to forward (testable, no I/O).
pub async fn process_gateway_frame(
    session: &mut GatewaySession,
    frame: &str,
    inner: &DiscordInner,
) -> (Vec<GatewayAction>, Vec<IncomingMessage>) {
    let payload: GatewayPayload = match serde_json::from_str(frame) {
        Ok(p) => p,
        Err(e) => {
            debug!("discord gateway invalid json: {e}");
            return (vec![], vec![]);
        }
    };
    process_gateway_payload(session, &payload, inner).await
}

/// Process a parsed gateway payload.
pub async fn process_gateway_payload(
    session: &mut GatewaySession,
    payload: &GatewayPayload,
    inner: &DiscordInner,
) -> (Vec<GatewayAction>, Vec<IncomingMessage>) {
    let actions = session.handle_gateway_event(payload);
    let mut inbounds = Vec::new();

    for action in &actions {
        if let GatewayAction::Dispatch(name, data) = action {
            if name == "READY" {
                if let Some(bot_id) = ready_bot_user_id(data) {
                    *inner.bot_user_id.write().await = Some(bot_id.clone());
                    info!("Discord bot online (user_id={bot_id})");
                } else {
                    info!("Discord READY received");
                }
            }
            if name == "MESSAGE_CREATE" {
                if let Some(msgs) = try_message_create_inbound(inner, data).await {
                    inbounds.extend(msgs);
                }
            }
            if name == "INTERACTION_CREATE" {
                if let Some(msgs) = try_interaction_create_inbound(inner, data).await {
                    inbounds.extend(msgs);
                }
            }
        }
    }

    (actions, inbounds)
}

async fn try_interaction_create_inbound(
    inner: &DiscordInner,
    data: &serde_json::Value,
) -> Option<Vec<IncomingMessage>> {
    let interaction = parse_interaction_create(data)?;
    let incoming = interaction_to_incoming(&interaction)?;
    if let Err(err) = inner
        .defer_interaction(&interaction.id, &interaction.token)
        .await
    {
        warn!(
            interaction_id = %interaction.id,
            "Discord defer_interaction failed: {err}"
        );
        return Some(vec![]);
    }
    info!(
        command = ?interaction.command_name,
        channel_id = ?interaction.channel_id,
        user_id = ?interaction.user_id,
        "Discord slash interaction accepted"
    );
    Some(vec![incoming])
}

async fn try_message_create_inbound(
    inner: &DiscordInner,
    data: &serde_json::Value,
) -> Option<Vec<IncomingMessage>> {
    let raw = parse_message_create_raw(data)?;
    let bot_id = inner.bot_user_id.read().await.clone();
    let filter_cfg = DiscordInboundConfig {
        require_mention: inner.config.require_mention,
        bot_user_id: bot_id,
        free_response_channels: inner.config.free_response_channels.clone(),
        allowed_channels: inner.config.allowed_channels.clone(),
        ignored_channels: inner.config.ignored_channels.clone(),
    };
    if !should_accept_message(&raw, &filter_cfg) {
        debug!(
            channel_id = %raw.channel_id,
            user_id = ?raw.user_id,
            is_dm = raw.guild_id.is_none(),
            require_mention = filter_cfg.require_mention,
            "Discord MESSAGE_CREATE dropped by inbound filter"
        );
        return Some(vec![]);
    }
    info!(
        channel_id = %raw.channel_id,
        user_id = ?raw.user_id,
        is_dm = raw.guild_id.is_none(),
        "Discord inbound message accepted"
    );
    let mut dedup = inner.dedup.write().await;
    if dedup.is_duplicate(&raw.message_id) {
        return Some(vec![]);
    }
    drop(dedup);
    Some(vec![raw_to_incoming(&raw)])
}

fn normalize_bot_token(token: &str) -> String {
    let t = token.trim();
    t.strip_prefix("Bot ")
        .or_else(|| t.strip_prefix("bot "))
        .unwrap_or(t)
        .to_string()
}

pub fn build_identify_payload(config: &DiscordConfig) -> GatewayPayload {
    GatewayPayload {
        op: opcodes::IDENTIFY,
        d: Some(
            serde_json::to_value(IdentifyData {
                token: normalize_bot_token(&config.token),
                intents: config.intents,
                properties: IdentifyProperties {
                    os: "linux".into(),
                    browser: "hermes-agent".into(),
                    device: "hermes-agent".into(),
                },
            })
            .unwrap(),
        ),
        s: None,
        t: None,
    }
}

pub fn build_heartbeat_payload(sequence: Option<u64>) -> GatewayPayload {
    let d = match sequence {
        Some(s) => serde_json::Value::Number(s.into()),
        None => serde_json::Value::Null,
    };
    GatewayPayload {
        op: opcodes::HEARTBEAT,
        d: Some(d),
        s: None,
        t: None,
    }
}

pub fn build_resume_payload(config: &DiscordConfig, session_id: &str, seq: u64) -> GatewayPayload {
    GatewayPayload {
        op: opcodes::RESUME,
        d: Some(
            serde_json::to_value(ResumeData {
                token: normalize_bot_token(&config.token),
                session_id: session_id.to_string(),
                seq,
            })
            .unwrap(),
        ),
        s: None,
        t: None,
    }
}

fn payload_to_ws_text(payload: &GatewayPayload) -> Result<String, GatewayError> {
    serde_json::to_string(payload)
        .map_err(|e| GatewayError::ConnectionFailed(format!("gateway serialize: {e}")))
}

async fn send_gateway_payload(
    ws: &mut futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        WsMessage,
    >,
    session: &mut GatewaySession,
    payload: &GatewayPayload,
) -> Result<(), GatewayError> {
    if payload.op == opcodes::HEARTBEAT {
        session.heartbeat_sent();
    }
    let text = payload_to_ws_text(payload)?;
    ws.send(WsMessage::Text(text.into()))
        .await
        .map_err(|e| GatewayError::ConnectionFailed(format!("discord ws send: {e}")))?;
    Ok(())
}

async fn apply_gateway_actions(
    ws: &mut futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        WsMessage,
    >,
    session: &mut GatewaySession,
    inner: &Arc<DiscordInner>,
    actions: Vec<GatewayAction>,
    inbounds: Vec<IncomingMessage>,
) -> Result<bool, GatewayError> {
    let mut reconnect = false;
    let mut invalidate_resumable = None;

    for action in actions {
        match action {
            GatewayAction::SendIdentify => {
                send_gateway_payload(ws, session, &build_identify_payload(&inner.config)).await?;
            }
            GatewayAction::SendHeartbeat => {
                send_gateway_payload(
                    ws,
                    session,
                    &build_heartbeat_payload(session.sequence),
                )
                .await?;
            }
            GatewayAction::SendResume => {
                if let (Some(sid), Some(seq)) = (session.session_id.clone(), session.sequence) {
                    send_gateway_payload(
                        ws,
                        session,
                        &build_resume_payload(&inner.config, &sid, seq),
                    )
                    .await?;
                }
            }
            GatewayAction::Reconnect => reconnect = true,
            GatewayAction::InvalidSession(resumable) => {
                invalidate_resumable = Some(resumable);
                reconnect = true;
            }
            GatewayAction::Dispatch(_, _) => {}
        }
    }

    if let Some(resumable) = invalidate_resumable {
        if !resumable {
            session.reset();
        }
    }

    if !inbounds.is_empty() {
        if let Some(tx) = inner.inbound_tx.read().await.clone() {
            for msg in inbounds {
                let _ = tx.send(msg).await;
            }
        } else {
            debug!("discord inbound dropped: no inbound_tx configured");
        }
    }

    Ok(reconnect)
}

pub async fn gateway_loop(inner: Arc<DiscordInner>) {
    let mut backoff_idx = 0usize;
    let mut gateway_session = GatewaySession::new();

    while inner.base.is_running() {
        let gateway_url = match fetch_gateway_url(&inner.client, &inner.config.token).await {
            Ok(u) => u,
            Err(e) => {
                warn!("Discord fetch /gateway failed: {e}");
                let delay = RECONNECT_SECS[backoff_idx.min(RECONNECT_SECS.len() - 1)];
                backoff_idx = (backoff_idx + 1).min(RECONNECT_SECS.len() - 1);
                tokio::time::sleep(Duration::from_secs(delay)).await;
                continue;
            }
        };

        info!("Discord gateway connecting…");
        match tokio_tungstenite::connect_async(&gateway_url).await {
            Ok((ws_stream, _)) => {
                backoff_idx = 0;
                let (mut write, mut read) = ws_stream.split();
                let mut heartbeat_interval: Option<tokio::time::Interval> = None;
                let mut needs_reconnect = false;

                loop {
                    if !inner.base.is_running() {
                        break;
                    }
                    if session_is_zombie_and_should_reconnect(&gateway_session) {
                        needs_reconnect = true;
                        break;
                    }

                    tokio::select! {
                        _ = inner.stop.notified() => {
                            let _ = write.close().await;
                            return;
                        }
                        tick = async {
                            match &mut heartbeat_interval {
                                Some(iv) => {
                                    iv.tick().await;
                                }
                                None => std::future::pending().await,
                            }
                        } => {
                            let _ = tick;
                            if gateway_session.heartbeat_interval_ms.is_some() {
                                let hb = build_heartbeat_payload(gateway_session.sequence);
                                if send_gateway_payload(&mut write, &mut gateway_session, &hb).await.is_err() {
                                    needs_reconnect = true;
                                    break;
                                }
                            }
                        }
                        msg = read.next() => {
                            match msg {
                                Some(Ok(WsMessage::Text(t))) => {
                                    let (actions, inbounds) =
                                        process_gateway_frame(&mut gateway_session, &t, &inner).await;
                                    if gateway_session.heartbeat_interval_ms.is_some()
                                        && heartbeat_interval.is_none()
                                    {
                                        let ms = gateway_session.heartbeat_interval_ms.unwrap();
                                        heartbeat_interval = Some(tokio::time::interval(
                                            Duration::from_millis(ms),
                                        ));
                                    }
                                    if apply_gateway_actions(
                                        &mut write,
                                        &mut gateway_session,
                                        &inner,
                                        actions,
                                        inbounds,
                                    )
                                    .await
                                    .unwrap_or(true)
                                    {
                                        needs_reconnect = true;
                                        break;
                                    }
                                }
                                Some(Ok(WsMessage::Ping(p))) => {
                                    let _ = write.send(WsMessage::Pong(p)).await;
                                }
                                Some(Ok(WsMessage::Close(frame))) => {
                                    if let Some(cf) = frame {
                                        warn!(
                                            "Discord WS closed: code={:?} reason={}",
                                            cf.code,
                                            cf.reason
                                        );
                                    } else {
                                        warn!("Discord WS closed without close frame");
                                    }
                                    needs_reconnect = true;
                                    break;
                                }
                                None => {
                                    needs_reconnect = true;
                                    break;
                                }
                                Some(Err(e)) => {
                                    warn!("Discord WS read error: {e}");
                                    needs_reconnect = true;
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                }

                if !needs_reconnect && !inner.base.is_running() {
                    return;
                }
            }
            Err(e) => warn!("Discord WS connect failed: {e}"),
        }

        let delay = RECONNECT_SECS[backoff_idx.min(RECONNECT_SECS.len() - 1)];
        backoff_idx = (backoff_idx + 1).min(RECONNECT_SECS.len() - 1);
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }
}

fn session_is_zombie_and_should_reconnect(session: &GatewaySession) -> bool {
    session.identified && session.is_zombie()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

    fn test_inner(require_mention: bool, bot_id: Option<&str>) -> DiscordInner {
        let base = BasePlatformAdapter::new("test-token");
        let client = base.build_client().unwrap();
        DiscordInner {
            config: {
                let mut c = super::super::config::DiscordConfig::for_test("test-token");
                c.require_mention = require_mention;
                c
            },
            client,
            base,
            inbound_tx: RwLock::new(None),
            bot_user_id: RwLock::new(bot_id.map(String::from)),
            dedup: RwLock::new(MessageDedup::new()),
            stop: tokio::sync::Notify::new(),
        }
    }

    #[tokio::test]
    async fn g01_hello_triggers_identify_action() {
        let inner = test_inner(false, None);
        let mut session = GatewaySession::new();
        let frame = serde_json::json!({
            "op": 10,
            "d": { "heartbeat_interval": 45000 }
        })
        .to_string();
        let (actions, _) = process_gateway_frame(&mut session, &frame, &inner).await;
        assert!(actions.contains(&GatewayAction::SendIdentify));
    }

    #[tokio::test]
    async fn g02_message_create_produces_inbound() {
        let inner = test_inner(false, Some("bot99"));
        let mut session = GatewaySession::new();
        let frame = serde_json::json!({
            "op": 0,
            "t": "MESSAGE_CREATE",
            "d": {
                "id": "msg-1",
                "channel_id": "ch-1",
                "content": "hello",
                "author": { "id": "user-1", "bot": false }
            }
        })
        .to_string();
        let (_, inbounds) = process_gateway_frame(&mut session, &frame, &inner).await;
        assert_eq!(inbounds.len(), 1);
        assert_eq!(inbounds[0].text, "hello");
        assert!(inbounds[0].is_dm);
    }

    #[tokio::test]
    async fn g03_duplicate_message_id_dropped() {
        let inner = test_inner(false, Some("bot99"));
        let mut session = GatewaySession::new();
        let frame = serde_json::json!({
            "op": 0,
            "t": "MESSAGE_CREATE",
            "d": {
                "id": "dup-1",
                "channel_id": "ch-1",
                "content": "hello",
                "author": { "id": "user-1", "bot": false }
            }
        })
        .to_string();
        let (_, first) = process_gateway_frame(&mut session, &frame, &inner).await;
        assert_eq!(first.len(), 1);
        let (_, second) = process_gateway_frame(&mut session, &frame, &inner).await;
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn g04_filtered_guild_no_mention_dropped() {
        let inner = test_inner(true, Some("bot99"));
        let mut session = GatewaySession::new();
        let frame = serde_json::json!({
            "op": 0,
            "t": "MESSAGE_CREATE",
            "d": {
                "id": "msg-2",
                "channel_id": "ch-1",
                "guild_id": "g-1",
                "content": "hello",
                "author": { "id": "user-1", "bot": false },
                "mentions": []
            }
        })
        .to_string();
        let (_, inbounds) = process_gateway_frame(&mut session, &frame, &inner).await;
        assert!(inbounds.is_empty());
    }

    #[tokio::test]
    async fn g05_ready_sets_bot_user_for_mention_check() {
        let inner = test_inner(true, None);
        let mut session = GatewaySession::new();
        let ready = serde_json::json!({
            "op": 0,
            "t": "READY",
            "d": {
                "session_id": "sess",
                "user": { "id": "bot99" }
            }
        })
        .to_string();
        process_gateway_frame(&mut session, &ready, &inner).await;
        assert_eq!(*inner.bot_user_id.read().await, Some("bot99".into()));

        let msg = serde_json::json!({
            "op": 0,
            "t": "MESSAGE_CREATE",
            "d": {
                "id": "msg-3",
                "channel_id": "ch-1",
                "guild_id": "g-1",
                "content": "hi",
                "author": { "id": "user-1", "bot": false },
                "mentions": [{ "id": "bot99" }]
            }
        })
        .to_string();
        let (_, inbounds) = process_gateway_frame(&mut session, &msg, &inner).await;
        assert_eq!(inbounds.len(), 1);
    }

    #[test]
    fn g06_interaction_create_parses_slash_text() {
        let data = serde_json::json!({
            "id": "int-1",
            "application_id": "app-1",
            "type": 2,
            "token": "tok-abc",
            "channel_id": "ch-99",
            "guild_id": "g-1",
            "member": { "user": { "id": "user-42" } },
            "data": { "name": "help" }
        });
        let interaction = parse_interaction_create(&data).expect("parse");
        let incoming = interaction_to_incoming(&interaction).expect("incoming");
        assert_eq!(incoming.text, "/help");
        assert_eq!(incoming.interaction_id.as_deref(), Some("int-1"));
        assert_eq!(incoming.interaction_token.as_deref(), Some("tok-abc"));
    }
}
