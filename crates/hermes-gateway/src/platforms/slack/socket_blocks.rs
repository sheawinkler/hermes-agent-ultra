impl SlackMediaFile {
    fn from_value(file: &serde_json::Value) -> Option<Self> {
        let id = slack_value_string(file, "id");
        let name = slack_value_string(file, "name");
        let mimetype =
            slack_value_string(file, "mimetype").or_else(|| slack_value_string(file, "mime_type"));
        let subtype = slack_value_string(file, "subtype");
        let url_private = slack_value_string(file, "url_private");
        let url_private_download = slack_value_string(file, "url_private_download");

        if id.is_none()
            && name.is_none()
            && mimetype.is_none()
            && subtype.is_none()
            && url_private.is_none()
            && url_private_download.is_none()
        {
            return None;
        }

        let kind = slack_media_kind(name.as_deref(), mimetype.as_deref(), subtype.as_deref());
        let (cache_extension, reported_mime_type) = if kind == SlackMediaKind::Audio {
            let ext = resolve_slack_audio_ext(name.as_deref(), mimetype.as_deref());
            let reported = slack_audio_mime_for_ext(&ext).to_string();
            (Some(ext), Some(reported))
        } else {
            (
                None,
                mimetype
                    .as_deref()
                    .map(slack_mime_key)
                    .filter(|s| !s.is_empty()),
            )
        };

        Some(Self {
            id,
            name,
            mimetype,
            subtype,
            url_private,
            url_private_download,
            kind,
            cache_extension,
            reported_mime_type,
        })
    }
}

// ---------------------------------------------------------------------------
// Socket Mode session management
// ---------------------------------------------------------------------------

/// Connection state for a Socket Mode WebSocket session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketModeConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Closing,
}

/// Describes what the caller should do after `handle_envelope`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketModeAction {
    Ack,
    MessageEvent(IncomingSlackMessage),
    InteractiveEvent(InteractivePayload),
    SlashCommand(SlashCommandPayload),
    Ignore,
}

/// Manages a single Socket Mode WebSocket session, tracking connection
/// lifecycle and providing envelope acknowledgment helpers.
#[derive(Debug)]
pub struct SocketModeSession {
    state: SocketModeConnectionState,
    envelopes_acked: u64,
    mention_policy: SlackMentionPolicy,
}

impl SocketModeSession {
    pub fn new() -> Self {
        Self::with_mention_policy(SlackMentionPolicy::default())
    }

    pub fn with_config(config: &SlackConfig) -> Self {
        Self::with_mention_policy(SlackMentionPolicy::from_config(config))
    }

    pub fn with_mention_policy(mention_policy: SlackMentionPolicy) -> Self {
        Self {
            state: SocketModeConnectionState::Disconnected,
            envelopes_acked: 0,
            mention_policy,
        }
    }

    pub fn state(&self) -> SocketModeConnectionState {
        self.state
    }
    pub fn envelopes_acked(&self) -> u64 {
        self.envelopes_acked
    }

    pub fn mark_connecting(&mut self) {
        self.state = SocketModeConnectionState::Connecting;
    }

    pub fn mark_connected(&mut self) {
        self.state = SocketModeConnectionState::Connected;
        debug!("Socket Mode session connected");
    }

    pub fn mark_closing(&mut self) {
        self.state = SocketModeConnectionState::Closing;
    }

    /// Build the JSON ack payload for a Socket Mode envelope.
    pub fn build_ack_payload(envelope_id: &str) -> String {
        format!(r#"{{"envelope_id":"{}"}}"#, envelope_id)
    }

    /// Inspect an envelope and return a typed action the caller should take.
    pub fn handle_envelope(&mut self, envelope: &SocketModeEnvelope) -> SocketModeAction {
        match envelope.envelope_type.as_str() {
            "hello" => {
                self.mark_connected();
                SocketModeAction::Ignore
            }
            "disconnect" => {
                info!("Socket Mode disconnect requested by server");
                self.mark_closing();
                SocketModeAction::Ignore
            }
            "events_api" => {
                self.envelopes_acked += 1;
                match SlackAdapter::parse_event_with_mention_policy(envelope, &self.mention_policy)
                {
                    Some(msg) => SocketModeAction::MessageEvent(msg),
                    None => SocketModeAction::Ack,
                }
            }
            "interactive" => {
                self.envelopes_acked += 1;
                match InteractivePayload::from_envelope(envelope) {
                    Some(payload) => SocketModeAction::InteractiveEvent(payload),
                    None => SocketModeAction::Ack,
                }
            }
            "slash_commands" => {
                self.envelopes_acked += 1;
                match SlashCommandPayload::from_envelope(envelope) {
                    Some(cmd) => SocketModeAction::SlashCommand(cmd),
                    None => SocketModeAction::Ack,
                }
            }
            other => {
                debug!(envelope_type = other, "Unhandled Socket Mode envelope type");
                SocketModeAction::Ignore
            }
        }
    }
}

impl Default for SocketModeSession {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Interactive components & slash commands
// ---------------------------------------------------------------------------

/// Parsed interactive payload from `block_actions`, `view_submission`, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractivePayload {
    #[serde(rename = "type")]
    pub payload_type: String,
    #[serde(default)]
    pub trigger_id: Option<String>,
    #[serde(default)]
    pub actions: Vec<InteractiveAction>,
    #[serde(default)]
    pub user: Option<InteractiveUser>,
    #[serde(default)]
    pub channel: Option<InteractiveChannel>,
    #[serde(default)]
    pub message: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveAction {
    #[serde(default)]
    pub action_id: Option<String>,
    #[serde(default)]
    pub block_id: Option<String>,
    #[serde(rename = "type", default)]
    pub action_type: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub selected_option: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveUser {
    pub id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveChannel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

impl InteractivePayload {
    pub fn from_envelope(envelope: &SocketModeEnvelope) -> Option<Self> {
        serde_json::from_value(envelope.payload.as_ref()?.clone()).ok()
    }
}

/// Parsed slash command payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommandPayload {
    pub command: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub trigger_id: Option<String>,
    #[serde(default)]
    pub response_url: Option<String>,
}

impl SlashCommandPayload {
    pub fn from_envelope(envelope: &SocketModeEnvelope) -> Option<Self> {
        serde_json::from_value(envelope.payload.as_ref()?.clone()).ok()
    }
}

// ---------------------------------------------------------------------------
// Block Kit message builder
// ---------------------------------------------------------------------------

/// A text object used throughout Block Kit (`plain_text` or `mrkdwn`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextObject {
    #[serde(rename = "type")]
    pub text_type: String,
    pub text: String,
}

impl TextObject {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text_type: "plain_text".into(),
            text: text.into(),
        }
    }
    pub fn mrkdwn(text: impl Into<String>) -> Self {
        Self {
            text_type: "mrkdwn".into(),
            text: text.into(),
        }
    }
}

/// An interactive element within an actions or section block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockElement {
    Button {
        text: TextObject,
        action_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        style: Option<String>,
    },
    Image {
        image_url: String,
        alt_text: String,
    },
    StaticSelect {
        placeholder: TextObject,
        action_id: String,
        options: Vec<SelectOption>,
    },
    Overflow {
        action_id: String,
        options: Vec<SelectOption>,
    },
}

/// An option inside a select menu or overflow element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub text: TextObject,
    pub value: String,
}

/// A section block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionBlock {
    pub text: TextObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessory: Option<BlockElement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<TextObject>>,
}

/// An actions block containing interactive elements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionsBlock {
    pub elements: Vec<BlockElement>,
}

/// A header block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderBlock {
    pub text: TextObject,
}

/// A context block (small text / images below content).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBlock {
    pub elements: Vec<ContextElement>,
}

/// An element within a context block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextElement {
    #[serde(rename = "mrkdwn")]
    Mrkdwn {
        text: String,
    },
    #[serde(rename = "plain_text")]
    PlainText {
        text: String,
    },
    Image {
        image_url: String,
        alt_text: String,
    },
}

/// A Block Kit layout block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Section(SectionBlock),
    Divider {},
    Actions(ActionsBlock),
    Header(HeaderBlock),
    Context(ContextBlock),
}

impl Block {
    pub fn section(text: TextObject) -> Self {
        Block::Section(SectionBlock {
            text,
            accessory: None,
            fields: None,
        })
    }

    pub fn section_with_accessory(text: TextObject, accessory: BlockElement) -> Self {
        Block::Section(SectionBlock {
            text,
            accessory: Some(accessory),
            fields: None,
        })
    }

    pub fn section_with_fields(text: TextObject, fields: Vec<TextObject>) -> Self {
        Block::Section(SectionBlock {
            text,
            accessory: None,
            fields: Some(fields),
        })
    }

    pub fn divider() -> Self {
        Block::Divider {}
    }

    pub fn actions(elements: Vec<BlockElement>) -> Self {
        Block::Actions(ActionsBlock { elements })
    }

    pub fn header(text: impl Into<String>) -> Self {
        Block::Header(HeaderBlock {
            text: TextObject::plain(text),
        })
    }

    pub fn context(elements: Vec<ContextElement>) -> Self {
        Block::Context(ContextBlock { elements })
    }
}

/// Builder for a complete Block Kit message.
#[derive(Debug, Clone, Default)]
pub struct BlockKitMessage {
    blocks: Vec<Block>,
}

impl BlockKitMessage {
    pub fn new() -> Self {
        Self { blocks: Vec::new() }
    }

    pub fn add_block(mut self, block: Block) -> Self {
        self.blocks.push(block);
        self
    }
    pub fn add_section(self, text: TextObject) -> Self {
        self.add_block(Block::section(text))
    }
    pub fn add_divider(self) -> Self {
        self.add_block(Block::divider())
    }
    pub fn add_header(self, text: impl Into<String>) -> Self {
        self.add_block(Block::header(text))
    }
    pub fn add_actions(self, elems: Vec<BlockElement>) -> Self {
        self.add_block(Block::actions(elems))
    }
    pub fn add_context(self, elems: Vec<ContextElement>) -> Self {
        self.add_block(Block::context(elems))
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Serialize the blocks array to a `serde_json::Value`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.blocks).unwrap_or_else(|_| serde_json::json!([]))
    }
}

// ---------------------------------------------------------------------------
// Home tab view
// ---------------------------------------------------------------------------

/// A Slack Home tab view payload for `views.publish`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeView {
    #[serde(rename = "type")]
    view_type: String,
    blocks: Vec<Block>,
}

impl HomeView {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self {
            view_type: "home".into(),
            blocks,
        }
    }

    pub fn from_block_kit(message: &BlockKitMessage) -> Self {
        Self::new(message.blocks().to_vec())
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

// ---------------------------------------------------------------------------
// Modal view (for views.open)
// ---------------------------------------------------------------------------

/// A Slack modal view payload for `views.open`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModalView {
    #[serde(rename = "type")]
    view_type: String,
    title: TextObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    submit: Option<TextObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close: Option<TextObject>,
    blocks: Vec<Block>,
    #[serde(skip_serializing_if = "Option::is_none")]
    callback_id: Option<String>,
}

impl ModalView {
    pub fn new(title: impl Into<String>, blocks: Vec<Block>) -> Self {
        Self {
            view_type: "modal".into(),
            title: TextObject::plain(title),
            submit: None,
            close: None,
            blocks,
            callback_id: None,
        }
    }

    pub fn with_submit(mut self, label: impl Into<String>) -> Self {
        self.submit = Some(TextObject::plain(label));
        self
    }

    pub fn with_close(mut self, label: impl Into<String>) -> Self {
        self.close = Some(TextObject::plain(label));
        self
    }

    pub fn with_callback_id(mut self, id: impl Into<String>) -> Self {
        self.callback_id = Some(id.into());
        self
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

