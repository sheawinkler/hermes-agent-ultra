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

/// Discord IDENTIFY payload data.
#[derive(Debug, Serialize)]
pub struct IdentifyData {
    pub token: String,
    pub intents: u64,
    pub properties: IdentifyProperties,
}

/// Discord IDENTIFY connection properties.
#[derive(Debug, Serialize)]
pub struct IdentifyProperties {
    pub os: String,
    pub browser: String,
    pub device: String,
}

/// Discord RESUME payload data.
#[derive(Debug, Serialize)]
pub struct ResumeData {
    pub token: String,
    pub session_id: String,
    pub seq: u64,
}

// ---------------------------------------------------------------------------
// Gateway state machine
// ---------------------------------------------------------------------------

/// Actions that the external WebSocket driver should take after processing
/// a gateway event through [`GatewaySession::handle_gateway_event`].
#[derive(Debug, Clone, PartialEq)]
pub enum GatewayAction {
    /// Send an IDENTIFY payload to the gateway.
    SendIdentify,
    /// Send a HEARTBEAT payload with the current sequence number.
    SendHeartbeat,
    /// Send a RESUME payload to continue a disconnected session.
    SendResume,
    /// The gateway requested a reconnect – close and reconnect.
    Reconnect,
    /// The session has been invalidated; if `bool` is true the session
    /// is resumable, otherwise a fresh IDENTIFY is required.
    InvalidSession(bool),
    /// A dispatch event arrived. Contains the event name and its data.
    Dispatch(String, serde_json::Value),
}

/// Manages the client-side state for a single Discord Gateway connection.
///
/// This is a pure state machine: feed it [`GatewayPayload`]s received from
/// the WebSocket and it will return a list of [`GatewayAction`]s that the
/// driver should execute. The struct never performs I/O itself, making it
/// easy to test and compose with any WebSocket library.
#[derive(Debug)]
pub struct GatewaySession {
    /// Last received sequence number.
    pub sequence: Option<u64>,
    /// Session ID from the READY event.
    pub session_id: Option<String>,
    /// Resume gateway URL from the READY event.
    pub resume_gateway_url: Option<String>,
    /// Heartbeat interval in milliseconds, extracted from HELLO.
    pub heartbeat_interval_ms: Option<u64>,
    /// Whether the last heartbeat was acknowledged.
    pub heartbeat_acknowledged: bool,
    /// Tracks whether we have successfully identified.
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

    /// Returns `true` if the session holds enough data to attempt a RESUME.
    pub fn can_resume(&self) -> bool {
        self.session_id.is_some() && self.sequence.is_some()
    }

    /// Process an incoming gateway payload and return the actions the driver
    /// should perform.
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

        actions.push(GatewayAction::SendHeartbeat);

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

    /// Mark a heartbeat as sent (used by the driver before sending).
    pub fn heartbeat_sent(&mut self) {
        self.heartbeat_acknowledged = false;
    }

    /// Returns `true` if the last heartbeat was not acknowledged, indicating
    /// the connection is likely zombied and should be reconnected.
    pub fn is_zombie(&self) -> bool {
        !self.heartbeat_acknowledged
    }

    /// Reset the session state for a fresh connection.
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

// ---------------------------------------------------------------------------
// Discord REST API types
// ---------------------------------------------------------------------------

