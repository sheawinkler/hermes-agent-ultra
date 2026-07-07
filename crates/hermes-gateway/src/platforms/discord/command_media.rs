#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordClientReentryAction {
    ReuseFreshSlot,
    ClosePreviousClient,
}

/// Re-entering connect with an open client must close the old websocket first.
pub fn discord_client_reentry_action(previous_client_open: bool) -> DiscordClientReentryAction {
    if previous_client_open {
        DiscordClientReentryAction::ClosePreviousClient
    } else {
        DiscordClientReentryAction::ReuseFreshSlot
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordSlashSyncPolicy {
    Off,
    Diff,
    Bulk,
}

impl DiscordSlashSyncPolicy {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            Some(value) if value.eq_ignore_ascii_case("off") => Self::Off,
            Some(value) if value.eq_ignore_ascii_case("bulk") => Self::Bulk,
            _ => Self::Diff,
        }
    }

    pub fn should_register(self, slash_commands_enabled: bool) -> bool {
        slash_commands_enabled && self != Self::Off
    }
}

/// Resolve a Discord channel prompt, preferring exact thread/channel IDs over parents.
pub fn discord_resolve_channel_prompt<'a>(
    prompts: &'a BTreeMap<String, String>,
    channel_id: &str,
    parent_channel_id: Option<&str>,
) -> Option<&'a str> {
    let channel_id = channel_id.trim();
    if !channel_id.is_empty() {
        if let Some(prompt) = prompts
            .get(channel_id)
            .map(String::as_str)
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
        {
            return Some(prompt);
        }
    }

    parent_channel_id
        .map(str::trim)
        .filter(|parent| !parent.is_empty())
        .and_then(|parent| prompts.get(parent))
        .map(String::as_str)
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
}

/// Compose per-run system prompt layers in Python gateway order.
pub fn discord_compose_ephemeral_system_prompt(
    context_prompt: Option<&str>,
    channel_prompt: Option<&str>,
    global_prompt: Option<&str>,
) -> Option<String> {
    let parts = [context_prompt, channel_prompt, global_prompt]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(String::from)
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordModelPickerEdit {
    pub title: String,
    pub description: String,
    pub clears_view: bool,
}

pub fn discord_model_picker_switch_edits(
    model_id: &str,
    switch_result: &str,
) -> (DiscordModelPickerEdit, DiscordModelPickerEdit) {
    (
        DiscordModelPickerEdit {
            title: "Switching Model".into(),
            description: format!("Switching to `{}`...", model_id.trim()),
            clears_view: true,
        },
        DiscordModelPickerEdit {
            title: "Model Switched".into(),
            description: switch_result.to_string(),
            clears_view: true,
        },
    )
}

fn strip_discord_mentions(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' && matches!(chars.peek(), Some('@' | '#' | '&')) {
            let mut consumed_marker = false;
            for next in chars.by_ref() {
                if next == '>' {
                    consumed_marker = true;
                    break;
                }
            }
            if consumed_marker {
                out.push(' ');
                continue;
            }
        }
        out.push(ch);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn discord_auto_thread_name(content: &str) -> String {
    let stripped = strip_discord_mentions(content);
    let candidate = stripped.trim();
    let candidate = if candidate.is_empty() {
        "Hermes"
    } else {
        candidate
    };

    truncate_discord_utf16_with_suffix(candidate, 80, "...")
}

pub fn discord_thread_create_success_message(thread_id: &str) -> String {
    format!("Created thread <#{}>.", thread_id.trim())
}

pub fn discord_thread_create_failure_message(error: &str) -> String {
    format!("Failed to create thread: {}", error.trim())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordAttachmentKind {
    Image,
    Audio,
    Document,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordAttachmentHandling {
    pub kind: DiscordAttachmentKind,
    pub prefer_bot_session_read: bool,
    pub fallback_uses_ssrf_gate: bool,
    pub inject_text_content: bool,
}

pub fn discord_attachment_handling(
    filename: &str,
    content_type: Option<&str>,
    size_bytes: u64,
) -> DiscordAttachmentHandling {
    let ext = Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let content_type = content_type.unwrap_or_default().to_ascii_lowercase();
    let kind = if content_type.starts_with("image/")
        || matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp")
    {
        DiscordAttachmentKind::Image
    } else if content_type.starts_with("audio/")
        || matches!(ext.as_str(), "mp3" | "wav" | "ogg" | "m4a" | "flac")
    {
        DiscordAttachmentKind::Audio
    } else if !filename.trim().is_empty() {
        DiscordAttachmentKind::Document
    } else {
        DiscordAttachmentKind::Other
    };
    let inject_text_content = kind == DiscordAttachmentKind::Document
        && size_bytes <= 100 * 1024
        && (content_type.starts_with("text/")
            || matches!(ext.as_str(), "txt" | "md" | "markdown" | "log"));

    DiscordAttachmentHandling {
        kind,
        prefer_bot_session_read: matches!(
            kind,
            DiscordAttachmentKind::Image
                | DiscordAttachmentKind::Audio
                | DiscordAttachmentKind::Document
        ),
        fallback_uses_ssrf_gate: !matches!(kind, DiscordAttachmentKind::Other),
        inject_text_content,
    }
}

pub fn discord_inject_document_text(caption: &str, filename: &str, document_text: &str) -> String {
    let injected = format!(
        "[Content of {}]:\n{}",
        filename.trim(),
        document_text.trim_end()
    );
    let caption = caption.trim();
    if caption.is_empty() {
        injected
    } else {
        format!("{}\n\n{}", injected, caption)
    }
}

pub fn discord_opus_library_candidates(
    platform: &str,
    find_library_result: Option<&str>,
) -> Vec<String> {
    if let Some(found) = find_library_result
        .map(str::trim)
        .filter(|found| !found.is_empty())
    {
        return vec![found.to_string()];
    }

    if platform.eq_ignore_ascii_case("darwin") || platform.eq_ignore_ascii_case("macos") {
        vec![
            "/opt/homebrew/lib/libopus.dylib".into(),
            "/usr/local/lib/libopus.dylib".into(),
        ]
    } else {
        Vec::new()
    }
}

pub fn discord_should_log_opus_decode_error(error: Option<&str>) -> bool {
    error.map(str::trim).filter(|err| !err.is_empty()).is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscordVoiceJoinAction {
    Connect,
    MoveExisting,
    AlreadyConnected,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DiscordVoiceJoinTracker {
    connected_guilds: BTreeSet<String>,
    inflight_guilds: BTreeSet<String>,
}

impl DiscordVoiceJoinTracker {
    pub fn begin_join(&mut self, guild_id: impl Into<String>) -> DiscordVoiceJoinAction {
        let guild_id = guild_id.into();
        if self.connected_guilds.contains(&guild_id) {
            return DiscordVoiceJoinAction::AlreadyConnected;
        }
        if self.inflight_guilds.contains(&guild_id) {
            return DiscordVoiceJoinAction::MoveExisting;
        }
        self.inflight_guilds.insert(guild_id);
        DiscordVoiceJoinAction::Connect
    }

    pub fn complete_join(&mut self, guild_id: impl AsRef<str>, connected: bool) {
        let guild_id = guild_id.as_ref();
        self.inflight_guilds.remove(guild_id);
        if connected {
            self.connected_guilds.insert(guild_id.to_string());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordSlashRegistrationSpec {
    pub name: String,
    pub description: String,
    pub args_hint: Option<String>,
    pub command_text: String,
}

impl DiscordSlashRegistrationSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        args_hint: Option<impl Into<String>>,
        command_text: impl Into<String>,
    ) -> Self {
        let args_hint = args_hint
            .map(Into::into)
            .map(|hint: String| hint.trim().to_string())
            .filter(|hint| !hint.is_empty());
        Self {
            name: name.into(),
            description: description.into(),
            args_hint,
            command_text: command_text.into(),
        }
    }

    pub fn to_slash_command(&self) -> SlashCommand {
        let options = self.args_hint.as_ref().map(|hint| {
            vec![SlashCommandOption {
                name: "args".into(),
                description: hint.chars().take(100).collect(),
                option_type: 3,
                required: Some(false),
                choices: None,
            }]
        });
        SlashCommand {
            name: self.name.clone(),
            description: self.description.chars().take(100).collect(),
            options,
            default_member_permissions: None,
            dm_permission: Some(true),
            nsfw: Some(false),
            contexts: None,
            integration_types: None,
            command_type: 1,
        }
    }

    pub fn dispatch_text(&self, args: Option<&str>) -> String {
        let args = args.map(str::trim).filter(|args| !args.is_empty());
        match args {
            Some(args) => format!("{} {}", self.command_text.trim(), args),
            None => self.command_text.trim().to_string(),
        }
    }
}

mod command_sync;
pub use command_sync::{
    discord_auto_registered_commands, discord_command_fingerprint, plan_discord_command_sync,
    DiscordCommandSyncMutation, DiscordCommandSyncSummary,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordCommandSyncStateEntry {
    pub fingerprint: Option<String>,
    pub last_success_at: Option<u64>,
    pub last_attempt_at: Option<u64>,
    pub retry_after_until: Option<u64>,
    pub retry_after: Option<u64>,
}

impl DiscordCommandSyncStateEntry {
    pub fn should_attempt(&self, fingerprint: &str, now_epoch_secs: u64) -> bool {
        if self
            .retry_after_until
            .map(|until| until > now_epoch_secs)
            .unwrap_or(false)
        {
            return false;
        }
        self.fingerprint.as_deref() != Some(fingerprint)
    }

    pub fn record_attempt(&mut self, now_epoch_secs: u64) {
        self.last_attempt_at = Some(now_epoch_secs);
    }

    pub fn record_success(&mut self, fingerprint: impl Into<String>, now_epoch_secs: u64) {
        self.fingerprint = Some(fingerprint.into());
        self.last_success_at = Some(now_epoch_secs);
        self.retry_after = None;
        self.retry_after_until = None;
    }

    pub fn record_rate_limit(&mut self, retry_after_secs: u64, now_epoch_secs: u64) {
        self.retry_after = Some(retry_after_secs);
        self.retry_after_until = Some(now_epoch_secs.saturating_add(retry_after_secs));
    }
}

/// Channel-bound skill binding parsed from Python-style Discord config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordChannelSkillBinding {
    pub id: String,
    pub skills: Vec<String>,
}

impl DiscordChannelSkillBinding {
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        let obj = value.as_object()?;
        let id = obj.get("id").and_then(scalar_json_to_discord_id)?;
        let skills_value = obj.get("skills").or_else(|| obj.get("skill"))?;
        let mut skills = Vec::new();
        match skills_value {
            serde_json::Value::Array(values) => {
                for value in values {
                    if let Some(skill) = scalar_json_to_discord_id(value) {
                        if !skills.contains(&skill) {
                            skills.push(skill);
                        }
                    }
                }
            }
            value => {
                if let Some(skill) = scalar_json_to_discord_id(value) {
                    skills.push(skill);
                }
            }
        }
        (!skills.is_empty()).then_some(Self { id, skills })
    }

    pub fn list_from_json(value: Option<&serde_json::Value>) -> Vec<Self> {
        match value {
            Some(serde_json::Value::Array(values)) => {
                values.iter().filter_map(Self::from_json).collect()
            }
            Some(value) => Self::from_json(value).into_iter().collect(),
            None => Vec::new(),
        }
    }
}

fn resolve_channel_skills_from_bindings(
    bindings: &[DiscordChannelSkillBinding],
    channel_id: &str,
    parent_id: Option<&str>,
) -> Option<Vec<String>> {
    let channel_id = channel_id.trim();
    let parent_id = parent_id.map(str::trim).filter(|id| !id.is_empty());

    bindings
        .iter()
        .find(|binding| binding.id.trim() == channel_id)
        .or_else(|| {
            parent_id.and_then(|parent| bindings.iter().find(|binding| binding.id.trim() == parent))
        })
        .map(|binding| binding.skills.clone())
}
