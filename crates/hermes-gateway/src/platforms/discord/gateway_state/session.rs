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
    /// Send a server-requested HEARTBEAT immediately without changing the
    /// HELLO-derived periodic schedule.
    SendHeartbeatNow,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatTick {
    Send,
    Reconnect,
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
    /// Highest contiguous sequence durably owned by downstream ingress.
    committed_sequence: Option<u64>,
    /// Fresh IDENTIFY generation used to reject stale durability acknowledgements.
    generation: u64,
    /// Session ID from the READY event.
    pub session_id: Option<String>,
    /// Resume gateway URL from the READY event.
    pub resume_gateway_url: Option<String>,
    /// Authenticated bot user ID from the READY payload.
    pub client_user_id: Option<String>,
    /// Heartbeat interval in milliseconds, extracted from HELLO.
    pub heartbeat_interval_ms: Option<u64>,
    /// Whether one heartbeat is awaiting an ACK.
    pub heartbeat_pending: bool,
    /// Tracks whether we have successfully identified.
    pub identified: bool,
}

impl GatewaySession {
    pub fn new() -> Self {
        Self {
            sequence: None,
            committed_sequence: None,
            generation: 0,
            session_id: None,
            resume_gateway_url: None,
            client_user_id: None,
            heartbeat_interval_ms: None,
            heartbeat_pending: false,
            identified: false,
        }
    }

    /// Reset heartbeat state for a newly established socket while retaining
    /// sequence/session data needed to RESUME it.
    pub fn begin_connection(&mut self) {
        self.heartbeat_interval_ms = None;
        self.heartbeat_pending = false;
    }

    /// Returns `true` if the session holds enough durable state to RESUME.
    pub fn can_resume(&self) -> bool {
        self.session_id.is_some() && self.committed_sequence.is_some()
    }

    pub fn resume_sequence(&self) -> Option<u64> {
        self.committed_sequence
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Start a fresh IDENTIFY generation while preserving the current HELLO
    /// heartbeat interval on the open socket.
    pub fn begin_identify(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.sequence = None;
        self.committed_sequence = None;
        self.session_id = None;
        self.resume_gateway_url = None;
        self.client_user_id = None;
        self.heartbeat_pending = false;
        self.identified = false;
    }


    /// Commits a Gateway sequence only after the ordered ingress owner reports
    /// that every earlier dispatch is safe to skip during RESUME replay.
    pub fn commit_sequence(&mut self, sequence: Option<u64>) {
        if let Some(sequence) = sequence {
            if self
                .committed_sequence
                .is_none_or(|committed| sequence > committed)
            {
                self.committed_sequence = Some(sequence);
            }
        }
    }

    pub fn commit_sequence_for_generation(&mut self, generation: u64, sequence: u64) {
        if generation == self.generation {
            self.commit_sequence(Some(sequence));
        }
    }
    /// Process an incoming gateway payload and return the actions the driver
    /// should perform.
    pub fn handle_gateway_event(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        if let Some(sequence) = payload.s {
            if self.sequence.is_none_or(|received| sequence > received) {
                self.sequence = Some(sequence);
            }
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
        let interval = payload
            .d
            .as_ref()
            .and_then(|data| data.get("heartbeat_interval"))
            .and_then(serde_json::Value::as_u64)
            .filter(|interval| *interval > 0);
        let Some(interval) = interval else {
            warn!(
                "Discord Gateway HELLO omitted a positive heartbeat_interval; reconnecting before authentication"
            );
            return vec![GatewayAction::Reconnect];
        };
        self.heartbeat_interval_ms = Some(interval);
        debug!("gateway HELLO: heartbeat_interval={}ms", interval);

        if self.can_resume() {
            vec![GatewayAction::SendResume]
        } else {
            vec![GatewayAction::SendIdentify]
        }
    }

    fn handle_heartbeat_ack(&mut self) -> Vec<GatewayAction> {
        if self.heartbeat_pending {
            self.heartbeat_pending = false;
            debug!("heartbeat ACK received");
        } else {
            debug!("unsolicited heartbeat ACK ignored");
        }
        vec![]
    }

    fn handle_heartbeat_request(&self) -> Vec<GatewayAction> {
        vec![GatewayAction::SendHeartbeatNow]
    }

    fn handle_invalid_session(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        let resumable = payload
            .d
            .as_ref()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !resumable {
            self.reset();
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

        if event_name == "READY" && !self.handle_ready(&data) {
            return vec![GatewayAction::Reconnect];
        }

        vec![GatewayAction::Dispatch(event_name, data)]
    }

    /// Accept READY only when it establishes both resumable session identity
    /// and the bot identity needed to enforce sender policy. A malformed READY
    /// is connection-fatal rather than leaving a partially identified session
    /// that could route ingress before self-message filtering is possible.
    fn handle_ready(&mut self, data: &serde_json::Value) -> bool {
        let session_id = data
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let client_user_id = data
            .get("user")
            .and_then(|user| user.get("id"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let (Some(session_id), Some(client_user_id)) = (session_id, client_user_id) else {
            warn!("Discord READY missing authoritative session or bot identity; reconnecting");
            self.reset();
            return false;
        };

        let resume_gateway_url = data
            .get("resume_gateway_url")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(discord_gateway_v10_url)
            .transpose();
        let resume_gateway_url = match resume_gateway_url {
            Ok(url) => url,
            Err(error) => {
                warn!(
                    error = %error,
                    "Discord READY supplied an unsupported resume endpoint; reconnecting with fresh IDENTIFY"
                );
                self.reset();
                return false;
            }
        };
        self.identified = true;
        self.session_id = Some(session_id);
        self.resume_gateway_url = resume_gateway_url;
        self.client_user_id = Some(client_user_id);

        info!(
            session_available = true,
            resume_endpoint_available = self.resume_gateway_url.is_some(),
            client_identity_available = true,
            "Discord READY received"
        );
        true
    }

    /// Record one heartbeat that was actually written to the socket.
    pub fn heartbeat_sent(&mut self) {
        self.heartbeat_pending = true;
    }

    /// Decide the next fixed-interval heartbeat action. Discord requires a
    /// reconnect when the previous heartbeat was not ACKed between attempts.
    pub fn heartbeat_tick(&self) -> HeartbeatTick {
        if self.heartbeat_pending {
            HeartbeatTick::Reconnect
        } else {
            HeartbeatTick::Send
        }
    }

    /// Reset the session state after a non-resumable outcome.
    pub fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.sequence = None;
        self.committed_sequence = None;
        self.session_id = None;
        self.resume_gateway_url = None;
        self.client_user_id = None;
        self.heartbeat_interval_ms = None;
        self.heartbeat_pending = false;
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

