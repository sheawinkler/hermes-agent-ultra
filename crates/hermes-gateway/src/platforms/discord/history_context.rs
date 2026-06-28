/// Discord bot-message acceptance policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordBotMessagePolicy {
    /// Reject other bot/webhook senders.
    None,
    /// Accept bot/webhook senders only when they mention this bot.
    Mentions,
    /// Accept all bot/webhook senders.
    All,
}

impl DiscordBotMessagePolicy {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            Some(value) if value.eq_ignore_ascii_case("all") => Self::All,
            Some(value) if value.eq_ignore_ascii_case("mentions") => Self::Mentions,
            _ => Self::None,
        }
    }

    pub fn from_lookup<F>(mut lookup: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        Self::parse(lookup(DISCORD_ALLOW_BOTS_ENV).as_deref())
    }

    pub fn bypasses_gateway_allowlist(self) -> bool {
        matches!(self, Self::Mentions | Self::All)
    }
}

fn discord_message_type_is_user_visible(message_type: u8) -> bool {
    matches!(message_type, 0 | 19)
}

pub fn discord_flatten_clarify_choice(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        serde_json::Value::Object(map) => ["label", "description", "text", "title"]
            .into_iter()
            .filter_map(|key| map.get(key).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .find(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        serde_json::Value::Array(values) => {
            let joined = values
                .iter()
                .filter_map(discord_flatten_clarify_choice)
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.is_empty()).then_some(joined)
        }
        other => {
            let rendered = other.to_string();
            let trimmed = rendered.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
    }
}

pub fn discord_normalize_clarify_choices(
    values: impl IntoIterator<Item = serde_json::Value>,
) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| discord_flatten_clarify_choice(&value))
        .collect()
}

pub fn discord_clarify_button_label(index: usize, choice: &str) -> String {
    let prefix = format!("{}. ", index + 1);
    let budget = 80usize.saturating_sub(prefix.chars().count()).max(1);
    let choice_len = choice.chars().count();
    let label_body = if choice_len <= budget {
        choice.to_string()
    } else {
        let mut chars = choice
            .chars()
            .take(budget.saturating_sub(1))
            .collect::<String>();
        while chars.chars().last().is_some_and(char::is_whitespace) {
            chars.pop();
        }

        let cut_at = {
            let char_vec = chars.chars().collect::<Vec<_>>();
            let trailing_half = budget / 2;
            let space_cut = char_vec
                .iter()
                .rposition(|ch| *ch == ' ')
                .filter(|pos| *pos >= trailing_half);
            space_cut.or_else(|| {
                char_vec
                    .iter()
                    .rposition(|ch| matches!(*ch, '-' | ',' | '.' | ')'))
                    .filter(|pos| *pos >= trailing_half)
                    .map(|pos| pos + 1)
            })
        };

        if let Some(cut_at) = cut_at.filter(|pos| *pos > 0) {
            chars = chars.chars().take(cut_at).collect();
        }
        while chars.chars().last().is_some_and(char::is_whitespace) {
            chars.pop();
        }
        format!("{chars}…")
    };
    format!("{prefix}{label_body}")
}

fn discord_non_conversational_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"(?i)^\s*💾\s*Self-improvement review:\s+\S[\s\S]*$",
            r#"(?i)^\s*💾\s+Skill\s+['"].+?['"]\s+(?:created|updated|improved|patched)\.?\s*$"#,
            r"(?i)^\s*⏳\s+Working\s+—\s+\d+\s+min(?:\s|$)",
            r"(?i)^\s*\[Background process\s+\S+\s+(?:finished with exit code|is still running~)[\s\S]*\]\s*$",
            r"(?i)^\s*(?:✅|❌)\s+Hermes update\s+(?:finished|failed|timed out)[\s\S]*$",
            r"(?i)^\s*♻️?\s+Gateway\s+(?:restarted successfully|online\b)[\s\S]*$",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("valid Discord non-conversational pattern"))
        .collect()
    })
}

pub fn discord_looks_like_non_conversational_history_message(content: &str) -> bool {
    discord_non_conversational_patterns()
        .iter()
        .any(|pattern| pattern.is_match(content))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordHistoryMessage {
    pub id: String,
    pub author_name: Option<String>,
    pub author_is_bot: bool,
    pub author_is_self: bool,
    pub message_type: u8,
    pub content: String,
    pub has_attachments: bool,
}

impl DiscordHistoryMessage {
    pub fn new(
        id: impl Into<String>,
        author_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            author_name: Some(author_name.into()),
            author_is_bot: false,
            author_is_self: false,
            message_type: 0,
            content: content.into(),
            has_attachments: false,
        }
    }

    pub fn self_message(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            author_name: Some("Hermes".into()),
            author_is_bot: true,
            author_is_self: true,
            message_type: 0,
            content: content.into(),
            has_attachments: false,
        }
    }

    pub fn bot_message(
        id: impl Into<String>,
        author_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            author_name: Some(author_name.into()),
            author_is_bot: true,
            author_is_self: false,
            message_type: 0,
            content: content.into(),
            has_attachments: false,
        }
    }
}

fn discord_history_message_is_non_conversational(
    message: &DiscordHistoryMessage,
    non_conversational_ids: &BTreeSet<String>,
) -> bool {
    let id = message.id.trim();
    (!id.is_empty() && non_conversational_ids.contains(id))
        || discord_looks_like_non_conversational_history_message(&message.content)
}

fn discord_history_line(
    message: &DiscordHistoryMessage,
    include_other_bots: bool,
    non_conversational_ids: &BTreeSet<String>,
) -> Option<String> {
    if !discord_message_type_is_user_visible(message.message_type)
        || discord_history_message_is_non_conversational(message, non_conversational_ids)
    {
        return None;
    }
    if message.author_is_bot && !message.author_is_self && !include_other_bots {
        return None;
    }

    let content = match message.content.trim() {
        "" if message.has_attachments => "(attachment)".to_string(),
        "" => return None,
        text => text.to_string(),
    };
    let mut name = message
        .author_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown")
        .to_string();
    if message.author_is_bot {
        name.push_str(" [bot]");
    }
    Some(format!("[{name}] {content}"))
}

pub fn discord_format_channel_context(
    primary_newest_first: &[DiscordHistoryMessage],
    reply_newest_first: &[DiscordHistoryMessage],
    include_other_bots: bool,
    reply_target_id: Option<&str>,
    non_conversational_ids: &BTreeSet<String>,
) -> String {
    let mut collected = Vec::<(String, String)>::new();
    let mut seen_ids = BTreeSet::<String>::new();

    for message in primary_newest_first {
        if discord_history_message_is_non_conversational(message, non_conversational_ids) {
            continue;
        }
        if message.author_is_self {
            break;
        }
        let Some(line) = discord_history_line(message, include_other_bots, non_conversational_ids)
        else {
            continue;
        };
        let id = message.id.trim().to_string();
        if !id.is_empty() {
            seen_ids.insert(id.clone());
        }
        collected.push((id, line));
    }

    let reply_target_id = reply_target_id.map(str::trim).filter(|id| !id.is_empty());
    let mut reply_collected = Vec::<(String, String)>::new();
    if reply_target_id.is_some_and(|target_id| !seen_ids.contains(target_id)) {
        for message in reply_newest_first {
            let id = message.id.trim().to_string();
            if !id.is_empty() && seen_ids.contains(&id) {
                continue;
            }
            let Some(line) =
                discord_history_line(message, include_other_bots, non_conversational_ids)
            else {
                continue;
            };
            if !id.is_empty() {
                seen_ids.insert(id.clone());
            }
            reply_collected.push((id, line));
        }
    }

    let mut blocks = Vec::new();
    if !reply_collected.is_empty() {
        reply_collected.reverse();
        blocks.push(format!(
            "[Context around the replied-to message]\n{}",
            reply_collected
                .into_iter()
                .map(|(_, line)| line)
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if !collected.is_empty() {
        collected.reverse();
        blocks.push(format!(
            "[Recent channel messages]\n{}",
            collected
                .into_iter()
                .map(|(_, line)| line)
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    blocks.join("\n\n")
}

pub fn discord_should_fetch_channel_context(
    require_mention: bool,
    is_free_channel: bool,
    in_bot_thread: bool,
    context: &DiscordChannelContext,
    auto_threaded_channel: bool,
) -> bool {
    if context.is_dm || auto_threaded_channel {
        return false;
    }
    let has_mention_gap = require_mention && !is_free_channel && !in_bot_thread;
    has_mention_gap || context.is_thread || context.is_reply
}

/// Parse Discord reaction lifecycle opt-in values. Default is enabled.
pub fn discord_reactions_enabled_from_raw(raw: Option<&str>) -> bool {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(value) => parse_allowed_mention_bool(value, true),
        None => true,
    }
}

