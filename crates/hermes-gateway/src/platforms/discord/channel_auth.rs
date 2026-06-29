// ---------------------------------------------------------------------------
// Discord channel policy
// ---------------------------------------------------------------------------

fn scalar_json_to_discord_id(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn discord_id_set_from_csv(raw: &str) -> BTreeSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn discord_id_set_from_json(value: Option<&serde_json::Value>) -> BTreeSet<String> {
    let Some(value) = value else {
        return BTreeSet::new();
    };
    match value {
        serde_json::Value::String(raw) => discord_id_set_from_csv(raw),
        serde_json::Value::Array(values) => values
            .iter()
            .filter_map(scalar_json_to_discord_id)
            .collect::<BTreeSet<_>>(),
        other => scalar_json_to_discord_id(other).into_iter().collect(),
    }
}

fn bool_from_json(value: Option<&serde_json::Value>, default: bool) -> bool {
    match value {
        Some(serde_json::Value::Bool(v)) => *v,
        Some(serde_json::Value::Number(n)) => n.as_i64().map(|v| v != 0).unwrap_or(default),
        Some(serde_json::Value::String(raw)) => parse_allowed_mention_bool(raw, default),
        _ => default,
    }
}

fn channel_matches(
    ids: &BTreeSet<String>,
    channel_id: &str,
    parent_channel_id: Option<&str>,
) -> bool {
    if ids.iter().any(|id| id.trim() == "*") {
        return true;
    }
    let channel_id = channel_id.trim();
    let parent_channel_id = parent_channel_id.map(str::trim).filter(|s| !s.is_empty());
    (!channel_id.is_empty() && ids.contains(channel_id))
        || parent_channel_id
            .map(|parent| ids.contains(parent))
            .unwrap_or(false)
}

/// Discord channel-level policy controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordChannelControls {
    /// Server channel IDs whose messages are always dropped.
    #[serde(default)]
    pub ignored_channels: BTreeSet<String>,
    /// Server channel IDs where automatic thread creation is suppressed.
    #[serde(default)]
    pub no_thread_channels: BTreeSet<String>,
    /// Server channel IDs where mention-free responses are allowed.
    #[serde(default)]
    pub free_response_channels: BTreeSet<String>,
    /// Global auto-thread toggle. Defaults to true to match upstream behavior.
    #[serde(default = "default_true_channel_control")]
    pub auto_thread: bool,
    /// Require explicit mentions even in participated/free-response threads.
    #[serde(default)]
    pub thread_require_mention: bool,
}

fn default_true_channel_control() -> bool {
    true
}

impl Default for DiscordChannelControls {
    fn default() -> Self {
        Self {
            ignored_channels: BTreeSet::new(),
            no_thread_channels: BTreeSet::new(),
            free_response_channels: BTreeSet::new(),
            auto_thread: true,
            thread_require_mention: false,
        }
    }
}

impl DiscordChannelControls {
    pub fn from_extra(extra: &std::collections::HashMap<String, serde_json::Value>) -> Self {
        Self {
            ignored_channels: discord_id_set_from_json(extra.get("ignored_channels")),
            no_thread_channels: discord_id_set_from_json(extra.get("no_thread_channels")),
            free_response_channels: discord_id_set_from_json(extra.get("free_response_channels")),
            auto_thread: bool_from_json(extra.get("auto_thread"), true),
            thread_require_mention: bool_from_json(extra.get("thread_require_mention"), false),
        }
    }

    pub fn is_ignored(&self, context: &DiscordChannelContext) -> bool {
        if context.is_dm {
            return false;
        }
        channel_matches(
            &self.ignored_channels,
            &context.channel_id,
            context.parent_channel_id.as_deref(),
        )
    }

    pub fn allows_free_response(&self, context: &DiscordChannelContext) -> bool {
        if context.is_dm {
            return true;
        }
        context.voice_linked_text_channel
            || channel_matches(
                &self.free_response_channels,
                &context.channel_id,
                context.parent_channel_id.as_deref(),
            )
    }

    pub fn should_auto_thread(&self, context: &DiscordChannelContext) -> bool {
        if !self.auto_thread
            || context.is_dm
            || context.is_thread
            || context.is_reply
            || context.voice_linked_text_channel
            || self.allows_free_response(context)
        {
            return false;
        }

        !channel_matches(
            &self.no_thread_channels,
            &context.channel_id,
            context.parent_channel_id.as_deref(),
        )
    }
}

/// Discord channel context used by pure Rust policy checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordChannelContext {
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_channel_id: Option<String>,
    #[serde(default)]
    pub is_dm: bool,
    #[serde(default)]
    pub is_thread: bool,
    #[serde(default)]
    pub is_reply: bool,
    #[serde(default)]
    pub voice_linked_text_channel: bool,
}

impl DiscordChannelContext {
    pub fn server(channel_id: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            parent_channel_id: None,
            is_dm: false,
            is_thread: false,
            is_reply: false,
            voice_linked_text_channel: false,
        }
    }

    pub fn thread(channel_id: impl Into<String>, parent_channel_id: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            parent_channel_id: Some(parent_channel_id.into()),
            is_dm: false,
            is_thread: true,
            is_reply: false,
            voice_linked_text_channel: false,
        }
    }

    pub fn dm(channel_id: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            parent_channel_id: None,
            is_dm: true,
            is_thread: false,
            is_reply: false,
            voice_linked_text_channel: false,
        }
    }
}

fn id_matches_any(candidate: &str, allowed: &BTreeSet<String>) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return false;
    }
    let candidate_no_at = candidate.strip_prefix('@').unwrap_or(candidate);
    allowed.iter().any(|entry| {
        let allowed = entry.trim();
        if allowed.is_empty() {
            return false;
        }
        if allowed == "*" {
            return true;
        }
        let allowed_no_at = allowed.strip_prefix('@').unwrap_or(allowed);
        allowed.eq_ignore_ascii_case(candidate)
            || allowed.eq_ignore_ascii_case(candidate_no_at)
            || allowed_no_at.eq_ignore_ascii_case(candidate)
            || allowed_no_at.eq_ignore_ascii_case(candidate_no_at)
    })
}

/// Discord user/member data relevant to slash and component authorization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordInteractionSubject {
    pub user_id: Option<String>,
    pub role_ids: BTreeSet<String>,
    /// Guild that the resolved role list belongs to.
    ///
    /// Component interactions carry the resolved member role list directly and
    /// do not need this field. Slash/on-message role checks use it to avoid
    /// trusting roles from a different mutual guild.
    pub role_guild_id: Option<String>,
}

impl DiscordInteractionSubject {
    pub fn user(user_id: impl Into<String>) -> Self {
        Self {
            user_id: Some(user_id.into()),
            role_ids: BTreeSet::new(),
            role_guild_id: None,
        }
    }

    pub fn member(
        user_id: impl Into<String>,
        role_ids: impl IntoIterator<Item = impl Into<String>>,
        role_guild_id: impl Into<String>,
    ) -> Self {
        Self {
            user_id: Some(user_id.into()),
            role_ids: role_ids.into_iter().map(Into::into).collect(),
            role_guild_id: Some(role_guild_id.into()),
        }
    }

    fn has_role_match(&self, allowed_role_ids: &BTreeSet<String>) -> bool {
        self.role_ids
            .iter()
            .any(|role_id| id_matches_any(role_id, allowed_role_ids))
    }
}

/// Slash/component authorization policy matching Discord's Python gate shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordInteractionAuthPolicy {
    pub allowed_user_ids: BTreeSet<String>,
    pub allowed_role_ids: BTreeSet<String>,
    pub allowed_channels: BTreeSet<String>,
    pub ignored_channels: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordAuthDecision {
    Allow,
    Deny(DiscordAuthDenyReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordAuthDenyReason {
    AllowedUsersOrRoles,
    AllowedChannels,
    IgnoredChannels,
}

impl DiscordInteractionAuthPolicy {
    pub fn has_identity_policy(&self) -> bool {
        !self.allowed_user_ids.is_empty() || !self.allowed_role_ids.is_empty()
    }

    pub fn component_allows(&self, subject: &DiscordInteractionSubject) -> bool {
        if !self.has_identity_policy() {
            return true;
        }
        subject
            .user_id
            .as_deref()
            .map(|user_id| id_matches_any(user_id, &self.allowed_user_ids))
            .unwrap_or(false)
            || subject.has_role_match(&self.allowed_role_ids)
    }

    fn slash_role_allows(
        &self,
        subject: &DiscordInteractionSubject,
        guild_id: Option<&str>,
        is_dm: bool,
        dm_role_auth_guild: Option<&str>,
    ) -> bool {
        if self.allowed_role_ids.is_empty() || !subject.has_role_match(&self.allowed_role_ids) {
            return false;
        }

        let Some(role_guild_id) = subject
            .role_guild_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return false;
        };

        if is_dm {
            return dm_role_auth_guild
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(|trusted| trusted == role_guild_id)
                .unwrap_or(false);
        }

        guild_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|origin| origin == role_guild_id)
            .unwrap_or(false)
    }

    pub fn authorize_slash(
        &self,
        subject: &DiscordInteractionSubject,
        channel_context: Option<&DiscordChannelContext>,
        guild_id: Option<&str>,
        dm_role_auth_guild: Option<&str>,
    ) -> DiscordAuthDecision {
        let is_dm = channel_context
            .map(|ctx| ctx.is_dm)
            .unwrap_or(guild_id.is_none());
        if !is_dm {
            let Some(context) = channel_context else {
                if !self.allowed_channels.is_empty() {
                    return DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedChannels);
                }
                if !self.ignored_channels.is_empty() {
                    return DiscordAuthDecision::Deny(DiscordAuthDenyReason::IgnoredChannels);
                }
                return self.authorize_slash_identity(subject, guild_id, is_dm, dm_role_auth_guild);
            };

            if channel_matches(
                &self.ignored_channels,
                &context.channel_id,
                context.parent_channel_id.as_deref(),
            ) {
                return DiscordAuthDecision::Deny(DiscordAuthDenyReason::IgnoredChannels);
            }

            if !self.allowed_channels.is_empty()
                && !channel_matches(
                    &self.allowed_channels,
                    &context.channel_id,
                    context.parent_channel_id.as_deref(),
                )
            {
                return DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedChannels);
            }
        }

        if !self.has_identity_policy() {
            return DiscordAuthDecision::Allow;
        }

        self.authorize_slash_identity(subject, guild_id, is_dm, dm_role_auth_guild)
    }

    fn authorize_slash_identity(
        &self,
        subject: &DiscordInteractionSubject,
        guild_id: Option<&str>,
        is_dm: bool,
        dm_role_auth_guild: Option<&str>,
    ) -> DiscordAuthDecision {
        if !self.has_identity_policy() {
            return DiscordAuthDecision::Allow;
        }

        let user_allowed = subject
            .user_id
            .as_deref()
            .map(|user_id| id_matches_any(user_id, &self.allowed_user_ids))
            .unwrap_or(false);
        if user_allowed || self.slash_role_allows(subject, guild_id, is_dm, dm_role_auth_guild) {
            DiscordAuthDecision::Allow
        } else {
            DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
        }
    }
}

/// Component button authorization with pairing-store fallback.
///
/// Allowlist/role policy remains authoritative. When that fails, an explicitly
/// approved pairing entry authorizes the same Discord user id, matching the
/// gateway-level pairing path without relaxing fail-closed behavior for unknown
/// users.
pub fn discord_component_allows_with_pairing(
    policy: &DiscordInteractionAuthPolicy,
    subject: &DiscordInteractionSubject,
    pairing: Option<&PairingManager>,
) -> bool {
    if policy.component_allows(subject) {
        return true;
    }
    let Some(user_id) = subject
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    pairing
        .and_then(|manager| manager.state(user_id))
        .is_some_and(|state| state == PairingState::Approved)
}

/// Determine whether a Discord message may be routed without an explicit bot mention.
pub fn discord_allows_message_without_mention(
    require_mention: bool,
    controls: &DiscordChannelControls,
    context: &DiscordChannelContext,
    bot_participated_in_thread: bool,
    bot_mentioned: bool,
) -> bool {
    if bot_mentioned || !require_mention || context.is_dm || controls.allows_free_response(context)
    {
        return true;
    }
    context.is_thread && bot_participated_in_thread && !controls.thread_require_mention
}

/// Discord SendResult-style success handling for unauthorized slash notifications.
pub fn discord_notify_result_counts_delivered(success: Option<bool>) -> bool {
    success.unwrap_or(true)
}

/// Catalog entry used by the flat `/skill` Discord command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordSkillCommandEntry {
    pub name: String,
    pub description: String,
    pub command_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscordSkillCommandDecision {
    Unauthorized,
    UnknownSkill { requested_name: String },
    Dispatch { text: String },
}

#[derive(Debug, Clone, Copy)]
pub struct DiscordSkillCommandRequest<'a> {
    pub requested_name: &'a str,
    pub args: &'a str,
}

pub fn discord_skill_autocomplete_choices(
    policy: &DiscordInteractionAuthPolicy,
    subject: &DiscordInteractionSubject,
    channel_context: Option<&DiscordChannelContext>,
    guild_id: Option<&str>,
    dm_role_auth_guild: Option<&str>,
    entries: &[DiscordSkillCommandEntry],
    current: &str,
) -> Vec<String> {
    if policy.authorize_slash(subject, channel_context, guild_id, dm_role_auth_guild)
        != DiscordAuthDecision::Allow
    {
        return Vec::new();
    }

    let needle = current.trim().to_ascii_lowercase();
    entries
        .iter()
        .filter(|entry| {
            needle.is_empty()
                || entry.name.to_ascii_lowercase().contains(&needle)
                || entry.description.to_ascii_lowercase().contains(&needle)
        })
        .take(25)
        .map(|entry| entry.name.clone())
        .collect()
}

pub fn discord_skill_command_decision(
    policy: &DiscordInteractionAuthPolicy,
    subject: &DiscordInteractionSubject,
    channel_context: Option<&DiscordChannelContext>,
    guild_id: Option<&str>,
    dm_role_auth_guild: Option<&str>,
    entries: &[DiscordSkillCommandEntry],
    request: DiscordSkillCommandRequest<'_>,
) -> DiscordSkillCommandDecision {
    if policy.authorize_slash(subject, channel_context, guild_id, dm_role_auth_guild)
        != DiscordAuthDecision::Allow
    {
        return DiscordSkillCommandDecision::Unauthorized;
    }

    let requested = request.requested_name.trim();
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(requested))
    else {
        return DiscordSkillCommandDecision::UnknownSkill {
            requested_name: requested.to_string(),
        };
    };

    let args = request.args.trim();
    let text = if args.is_empty() {
        entry.command_key.clone()
    } else {
        format!("{} {}", entry.command_key, args)
    };
    DiscordSkillCommandDecision::Dispatch { text }
}

// ---------------------------------------------------------------------------
// Discord gateway parity helpers
// ---------------------------------------------------------------------------

fn discord_user_identifier_requires_member_lookup(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    let candidate = trimmed
        .strip_prefix('@')
        .unwrap_or(trimmed)
        .trim()
        .trim_matches(|c| c == '<' || c == '>');
    !candidate.is_empty() && !candidate.chars().all(|c| c.is_ascii_digit())
}

/// Whether Discord connect must request the privileged members intent.
pub fn discord_members_intent_required(
    allowed_users: impl IntoIterator<Item = impl AsRef<str>>,
) -> bool {
    allowed_users
        .into_iter()
        .any(|user| discord_user_identifier_requires_member_lookup(user.as_ref()))
}
