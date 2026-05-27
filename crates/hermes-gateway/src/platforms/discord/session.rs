//! Discord Gateway WebSocket state machine (no I/O).

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Discord Gateway payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayPayload {
    pub op: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
}

/// Discord Gateway opcodes.
pub mod opcodes {
    pub const DISPATCH: u8 = 0;
    pub const HEARTBEAT: u8 = 1;
    pub const IDENTIFY: u8 = 2;
    pub const PRESENCE_UPDATE: u8 = 3;
    pub const VOICE_STATE: u8 = 4;
    pub const RESUME: u8 = 6;
    pub const RECONNECT: u8 = 7;
    pub const REQUEST_GUILD_MEMBERS: u8 = 8;
    pub const INVALID_SESSION: u8 = 9;
    pub const HELLO: u8 = 10;
    pub const HEARTBEAT_ACK: u8 = 11;
}

/// Actions for the WebSocket driver after handling a gateway event.
#[derive(Debug, Clone, PartialEq)]
pub enum GatewayAction {
    SendIdentify,
    SendHeartbeat,
    SendResume,
    Reconnect,
    InvalidSession(bool),
    Dispatch(String, serde_json::Value),
}

/// Client-side Discord Gateway session state.
#[derive(Debug)]
pub struct GatewaySession {
    pub sequence: Option<u64>,
    pub session_id: Option<String>,
    pub resume_gateway_url: Option<String>,
    pub heartbeat_interval_ms: Option<u64>,
    pub heartbeat_acknowledged: bool,
    pub identified: bool,
}

impl GatewaySession {
    pub fn new() -> Self {
        Self {
            sequence: None,
            session_id: None,
            resume_gateway_url: None,
            heartbeat_interval_ms: None,
            heartbeat_acknowledged: true,
            identified: false,
        }
    }

    pub fn can_resume(&self) -> bool {
        self.session_id.is_some() && self.sequence.is_some()
    }

    pub fn handle_gateway_event(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        if let Some(seq) = payload.s {
            self.sequence = Some(seq);
        }

        match payload.op {
            opcodes::HELLO => self.handle_hello(payload),
            opcodes::HEARTBEAT_ACK => self.handle_heartbeat_ack(),
            opcodes::HEARTBEAT => self.handle_heartbeat_request(),
            opcodes::RECONNECT => vec![GatewayAction::Reconnect],
            opcodes::INVALID_SESSION => self.handle_invalid_session(payload),
            opcodes::DISPATCH => self.handle_dispatch(payload),
            _ => {
                debug!("unhandled gateway opcode {}", payload.op);
                vec![]
            }
        }
    }

    fn handle_hello(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        let mut actions = Vec::new();

        if let Some(d) = &payload.d {
            if let Some(interval) = d.get("heartbeat_interval").and_then(|v| v.as_u64()) {
                self.heartbeat_interval_ms = Some(interval);
                debug!("gateway HELLO: heartbeat_interval={}ms", interval);
            }
        }

        // Heartbeats are driven by the interval loop after IDENTIFY/RESUME — do not
        // send OP 1 on HELLO before authentication (Discord close code 4003).

        if self.can_resume() {
            actions.push(GatewayAction::SendResume);
        } else {
            actions.push(GatewayAction::SendIdentify);
        }

        actions
    }

    fn handle_heartbeat_ack(&mut self) -> Vec<GatewayAction> {
        self.heartbeat_acknowledged = true;
        debug!("heartbeat ACK received");
        vec![]
    }

    fn handle_heartbeat_request(&self) -> Vec<GatewayAction> {
        vec![GatewayAction::SendHeartbeat]
    }

    fn handle_invalid_session(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        let resumable = payload
            .d
            .as_ref()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !resumable {
            self.session_id = None;
            self.sequence = None;
            self.identified = false;
        }

        warn!("INVALID_SESSION received (resumable={})", resumable);
        vec![GatewayAction::InvalidSession(resumable)]
    }

    fn handle_dispatch(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        let event_name = match &payload.t {
            Some(name) => name.clone(),
            None => return vec![],
        };

        let data = payload.d.clone().unwrap_or(serde_json::Value::Null);

        if event_name == "READY" {
            self.handle_ready(&data);
        }

        vec![GatewayAction::Dispatch(event_name, data)]
    }

    fn handle_ready(&mut self, data: &serde_json::Value) {
        self.identified = true;

        if let Some(sid) = data.get("session_id").and_then(|v| v.as_str()) {
            self.session_id = Some(sid.to_string());
        }
        if let Some(url) = data.get("resume_gateway_url").and_then(|v| v.as_str()) {
            self.resume_gateway_url = Some(url.to_string());
        }

        info!(
            "READY: session_id={:?}, resume_url={:?}",
            self.session_id, self.resume_gateway_url
        );
    }

    pub fn heartbeat_sent(&mut self) {
        self.heartbeat_acknowledged = false;
    }

    pub fn is_zombie(&self) -> bool {
        !self.heartbeat_acknowledged
    }

    pub fn reset(&mut self) {
        self.sequence = None;
        self.session_id = None;
        self.resume_gateway_url = None;
        self.heartbeat_interval_ms = None;
        self.heartbeat_acknowledged = true;
        self.identified = false;
    }
}

impl Default for GatewaySession {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize)]
pub struct IdentifyData {
    pub token: String,
    pub intents: u64,
    pub properties: IdentifyProperties,
}

#[derive(Debug, Serialize)]
pub struct IdentifyProperties {
    pub os: String,
    pub browser: String,
    pub device: String,
}

#[derive(Debug, Serialize)]
pub struct ResumeData {
    pub token: String,
    pub session_id: String,
    pub seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s01_hello_without_session_sends_identify() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::HELLO,
            d: Some(serde_json::json!({ "heartbeat_interval": 41250 })),
            s: None,
            t: None,
        };
        let actions = session.handle_gateway_event(&payload);
        assert!(!actions.contains(&GatewayAction::SendHeartbeat));
        assert!(actions.contains(&GatewayAction::SendIdentify));
    }

    #[test]
    fn s02_hello_with_session_sends_resume() {
        let mut session = GatewaySession::new();
        session.session_id = Some("sess123".into());
        session.sequence = Some(42);
        let payload = GatewayPayload {
            op: opcodes::HELLO,
            d: Some(serde_json::json!({ "heartbeat_interval": 30000 })),
            s: None,
            t: None,
        };
        let actions = session.handle_gateway_event(&payload);
        assert!(actions.contains(&GatewayAction::SendResume));
        assert!(!actions.contains(&GatewayAction::SendIdentify));
    }

    #[test]
    fn s03_heartbeat_ack_clears_zombie() {
        let mut session = GatewaySession::new();
        session.heartbeat_acknowledged = false;
        let payload = GatewayPayload {
            op: opcodes::HEARTBEAT_ACK,
            d: None,
            s: None,
            t: None,
        };
        session.handle_gateway_event(&payload);
        assert!(session.heartbeat_acknowledged);
    }

    #[test]
    fn s04_heartbeat_opcode_requests_send() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::HEARTBEAT,
            d: None,
            s: None,
            t: None,
        };
        let actions = session.handle_gateway_event(&payload);
        assert_eq!(actions, vec![GatewayAction::SendHeartbeat]);
    }

    #[test]
    fn s05_reconnect_action() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::RECONNECT,
            d: None,
            s: None,
            t: None,
        };
        let actions = session.handle_gateway_event(&payload);
        assert_eq!(actions, vec![GatewayAction::Reconnect]);
    }

    #[test]
    fn s06_invalid_session_not_resumable_clears_state() {
        let mut session = GatewaySession::new();
        session.session_id = Some("sess".into());
        session.sequence = Some(10);
        let payload = GatewayPayload {
            op: opcodes::INVALID_SESSION,
            d: Some(serde_json::Value::Bool(false)),
            s: None,
            t: None,
        };
        session.handle_gateway_event(&payload);
        assert!(session.session_id.is_none());
        assert!(session.sequence.is_none());
    }

    #[test]
    fn s07_invalid_session_resumable_keeps_state() {
        let mut session = GatewaySession::new();
        session.session_id = Some("sess".into());
        session.sequence = Some(10);
        let payload = GatewayPayload {
            op: opcodes::INVALID_SESSION,
            d: Some(serde_json::Value::Bool(true)),
            s: None,
            t: None,
        };
        session.handle_gateway_event(&payload);
        assert!(session.session_id.is_some());
    }

    #[test]
    fn s08_ready_sets_session_fields() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(serde_json::json!({
                "session_id": "abc123",
                "resume_gateway_url": "wss://resume.discord.gg",
            })),
            s: Some(1),
            t: Some("READY".into()),
        };
        session.handle_gateway_event(&payload);
        assert_eq!(session.session_id, Some("abc123".into()));
        assert_eq!(
            session.resume_gateway_url,
            Some("wss://resume.discord.gg".into())
        );
        assert!(session.identified);
    }

    #[test]
    fn s09_dispatch_updates_sequence() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(serde_json::json!({})),
            s: Some(42),
            t: Some("GUILD_CREATE".into()),
        };
        session.handle_gateway_event(&payload);
        assert_eq!(session.sequence, Some(42));
    }

    #[test]
    fn s10_unknown_opcode_no_panic() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: 99,
            d: None,
            s: None,
            t: None,
        };
        let actions = session.handle_gateway_event(&payload);
        assert!(actions.is_empty());
    }
}
