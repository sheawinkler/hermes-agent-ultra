//! Channel directory cache for discovery, display, and human-friendly lookup.
//!
//! The cache lives at `$HERMES_HOME/channel_directory.json` and mirrors the
//! Python gateway contract: platform entries are persisted by platform name,
//! missing/corrupt caches load as empty, writes are atomic, and session history
//! can seed platforms that cannot enumerate channels directly.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};

use hermes_config::hermes_home;
use hermes_core::errors::GatewayError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelEntry {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing)]
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

impl ChannelEntry {
    pub fn new(
        platform: impl Into<String>,
        id: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            platform: normalize_platform(platform.into().as_str()),
            guild: None,
            kind: None,
            thread_id: None,
        }
    }

    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    pub fn with_guild(mut self, guild: impl Into<String>) -> Self {
        self.guild = Some(guild.into());
        self
    }

    pub fn with_thread(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelDirectorySnapshot {
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub platforms: BTreeMap<String, Vec<ChannelEntry>>,
}

#[async_trait]
pub trait ChannelDirectoryProvider: Send + Sync {
    fn platform_name(&self) -> &str;
    async fn list_channel_entries(&self) -> Result<Vec<ChannelEntry>, GatewayError>;
}

#[derive(Clone, Default)]
pub struct ChannelDirectory {
    channels: Arc<RwLock<HashMap<String, ChannelEntry>>>,
}

impl ChannelDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&self, entry: ChannelEntry) {
        if let Ok(mut channels) = self.channels.write() {
            channels.insert(entry.id.clone(), entry);
        }
    }

    pub fn get(&self, id: &str) -> Option<ChannelEntry> {
        self.channels.read().ok().and_then(|c| c.get(id).cloned())
    }

    pub fn list(&self) -> Vec<ChannelEntry> {
        self.channels
            .read()
            .map(|c| c.values().cloned().collect())
            .unwrap_or_default()
    }
}

pub fn directory_path() -> PathBuf {
    hermes_home().join("channel_directory.json")
}

pub fn load_directory() -> ChannelDirectorySnapshot {
    load_directory_from(directory_path())
}

pub fn load_directory_from(path: impl AsRef<Path>) -> ChannelDirectorySnapshot {
    let path = path.as_ref();
    let Ok(text) = std::fs::read_to_string(path) else {
        return ChannelDirectorySnapshot::default();
    };
    let Ok(mut snapshot) = serde_json::from_str::<ChannelDirectorySnapshot>(&text) else {
        return ChannelDirectorySnapshot::default();
    };
    normalize_snapshot(&mut snapshot);
    snapshot
}

pub fn write_directory_atomic(
    path: impl AsRef<Path>,
    snapshot: &ChannelDirectorySnapshot,
) -> io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let bytes = serde_json::to_vec_pretty(snapshot).map_err(io::Error::other)?;
    let tmp_path = path.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));

    match std::fs::write(&tmp_path, bytes).and_then(|_| std::fs::rename(&tmp_path, path)) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(err)
        }
    }
}

pub fn build_channel_directory(
    platforms: BTreeMap<String, Vec<ChannelEntry>>,
) -> ChannelDirectorySnapshot {
    build_channel_directory_at(directory_path(), platforms)
}

pub fn build_channel_directory_at(
    path: impl AsRef<Path>,
    platforms: BTreeMap<String, Vec<ChannelEntry>>,
) -> ChannelDirectorySnapshot {
    let mut snapshot = ChannelDirectorySnapshot {
        updated_at: Some(Utc::now().to_rfc3339()),
        platforms,
    };
    normalize_snapshot(&mut snapshot);
    if let Err(err) = write_directory_atomic(path, &snapshot) {
        warn!(error = %err, "Channel directory: failed to write cache");
    }
    snapshot
}

pub async fn build_channel_directory_from_providers(
    providers: &[Arc<dyn ChannelDirectoryProvider>],
) -> ChannelDirectorySnapshot {
    build_channel_directory_from_providers_at(directory_path(), hermes_home(), providers).await
}

pub async fn build_channel_directory_from_providers_at(
    path: impl AsRef<Path>,
    hermes_home_path: impl AsRef<Path>,
    providers: &[Arc<dyn ChannelDirectoryProvider>],
) -> ChannelDirectorySnapshot {
    let mut platforms: BTreeMap<String, Vec<ChannelEntry>> = BTreeMap::new();
    let mut discovered = BTreeSet::new();

    for provider in providers {
        let platform = normalize_platform(provider.platform_name());
        match provider.list_channel_entries().await {
            Ok(entries) => {
                platforms.insert(
                    platform.clone(),
                    merge_entries(
                        entries,
                        build_from_sessions_in(&hermes_home_path, &platform),
                    ),
                );
                discovered.insert(platform);
            }
            Err(err) => {
                warn!(platform = %platform, error = %err, "Channel directory: provider failed");
            }
        }
    }

    for platform in SESSION_DISCOVERY_PLATFORMS {
        if discovered.contains(*platform) {
            continue;
        }
        platforms.insert(
            (*platform).to_string(),
            build_from_sessions_in(&hermes_home_path, platform),
        );
    }

    build_channel_directory_at(path, platforms)
}

pub fn build_from_sessions(platform_name: &str) -> Vec<ChannelEntry> {
    build_from_sessions_in(hermes_home(), platform_name)
}

pub fn build_from_sessions_in(
    hermes_home_path: impl AsRef<Path>,
    platform_name: &str,
) -> Vec<ChannelEntry> {
    // `sessions/sessions.json` is a gateway routing index, not the CLI/TUI
    // session list. Python Hermes may include `_` metadata sentinels there so
    // humans who inspect it directly do not confuse it with state.db.
    let platform_name = normalize_platform(platform_name);
    let sessions_path = hermes_home_path
        .as_ref()
        .join("sessions")
        .join("sessions.json");
    let Ok(text) = std::fs::read_to_string(&sessions_path) else {
        return Vec::new();
    };
    let Ok(Value::Object(sessions)) = serde_json::from_str::<Value>(&text) else {
        debug!(path = %sessions_path.display(), "Channel directory: sessions cache is not an object");
        return Vec::new();
    };

    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for (key, session) in &sessions {
        if is_sessions_index_metadata_key(key) {
            continue;
        }
        let Some(session) = session.as_object() else {
            continue;
        };
        let Some(origin) = session.get("origin").and_then(Value::as_object) else {
            continue;
        };
        if value_to_string(origin.get("platform")).as_deref() != Some(platform_name.as_str()) {
            continue;
        }
        let Some(id) = session_entry_id(origin) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let mut entry = ChannelEntry::new(&platform_name, id, session_entry_name(origin))
            .with_kind(
                value_to_string(session.get("chat_type")).unwrap_or_else(|| "dm".to_string()),
            );
        if let Some(thread_id) = value_to_string(origin.get("thread_id")) {
            entry = entry.with_thread(thread_id);
        }
        entries.push(entry);
    }
    entries
}

fn is_sessions_index_metadata_key(key: &str) -> bool {
    key.trim_start().starts_with('_')
}

pub fn resolve_channel_name(platform_name: &str, name: &str) -> Option<String> {
    resolve_channel_name_from(directory_path(), platform_name, name)
}

pub fn resolve_channel_name_from(
    path: impl AsRef<Path>,
    platform_name: &str,
    name: &str,
) -> Option<String> {
    let directory = load_directory_from(path);
    let platform_name = normalize_platform(platform_name);
    let channels = directory.platforms.get(&platform_name)?;
    if channels.is_empty() {
        return None;
    }

    let raw = name.trim();
    for channel in channels {
        if channel.id == raw {
            return Some(channel.id.clone());
        }
    }

    let query = normalize_channel_query(raw);
    for channel in channels {
        if normalize_channel_query(&channel.name) == query
            || normalize_channel_query(&channel_target_name(&platform_name, channel)) == query
        {
            return Some(channel.id.clone());
        }
    }

    if let Some((guild_part, channel_part)) = query.rsplit_once('/') {
        for channel in channels {
            if channel
                .guild
                .as_deref()
                .map(|guild| guild.trim().eq_ignore_ascii_case(guild_part))
                .unwrap_or(false)
                && normalize_channel_query(&channel.name) == channel_part
            {
                return Some(channel.id.clone());
            }
        }
    }

    let matches: Vec<&ChannelEntry> = channels
        .iter()
        .filter(|channel| normalize_channel_query(&channel.name).starts_with(&query))
        .collect();
    if matches.len() == 1 {
        Some(matches[0].id.clone())
    } else {
        None
    }
}

pub fn lookup_channel_type(platform_name: &str, chat_id: &str) -> Option<String> {
    lookup_channel_type_from(directory_path(), platform_name, chat_id)
}

pub fn lookup_channel_type_from(
    path: impl AsRef<Path>,
    platform_name: &str,
    chat_id: &str,
) -> Option<String> {
    let directory = load_directory_from(path);
    directory
        .platforms
        .get(&normalize_platform(platform_name))?
        .iter()
        .find(|entry| entry.id == chat_id)
        .and_then(|entry| entry.kind.clone())
}

pub fn format_directory_for_display() -> String {
    format_directory_for_display_from(directory_path())
}

pub fn format_directory_for_display_from(path: impl AsRef<Path>) -> String {
    let directory = load_directory_from(path);
    let platforms = directory.platforms;
    if !platforms.values().any(|entries| !entries.is_empty()) {
        return "No messaging platforms connected or no channels discovered yet.".to_string();
    }

    let mut lines = vec!["Available messaging targets:\n".to_string()];
    for (platform, channels) in platforms {
        if channels.is_empty() {
            continue;
        }
        if platform == "discord" {
            let mut guilds: BTreeMap<String, Vec<&ChannelEntry>> = BTreeMap::new();
            let mut dms = Vec::new();
            for channel in &channels {
                if let Some(guild) = channel.guild.as_deref().filter(|guild| !guild.is_empty()) {
                    guilds.entry(guild.to_string()).or_default().push(channel);
                } else {
                    dms.push(channel);
                }
            }
            for (guild, mut guild_channels) in guilds {
                guild_channels.sort_by(|a, b| a.name.cmp(&b.name));
                lines.push(format!("Discord ({guild}):"));
                for channel in guild_channels {
                    lines.push(format!(
                        "  discord:{}",
                        channel_target_name(&platform, channel)
                    ));
                }
            }
            if !dms.is_empty() {
                lines.push("Discord (DMs):".to_string());
                for channel in dms {
                    lines.push(format!(
                        "  discord:{}",
                        channel_target_name(&platform, channel)
                    ));
                }
            }
            lines.push(String::new());
        } else {
            lines.push(format!("{}:", title_case_platform(&platform)));
            for channel in channels {
                lines.push(format!(
                    "  {}:{}",
                    platform,
                    channel_target_name(&platform, &channel)
                ));
            }
            lines.push(String::new());
        }
    }

    lines.push("Use these as the \"target\" parameter when sending.".to_string());
    lines.push("Bare platform name (e.g. \"telegram\") sends to home channel.".to_string());
    lines.join("\n")
}

fn normalize_snapshot(snapshot: &mut ChannelDirectorySnapshot) {
    let platforms = std::mem::take(&mut snapshot.platforms);
    snapshot.platforms = platforms
        .into_iter()
        .map(|(platform, mut entries)| {
            let platform = normalize_platform(&platform);
            for entry in &mut entries {
                entry.platform = platform.clone();
            }
            (platform, entries)
        })
        .collect();
}

fn normalize_platform(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}

fn normalize_channel_query(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('#')
        .trim()
        .to_ascii_lowercase()
}

fn channel_target_name(platform_name: &str, channel: &ChannelEntry) -> String {
    if platform_name == "discord"
        && channel
            .guild
            .as_deref()
            .is_some_and(|guild| !guild.is_empty())
    {
        return format!("#{}", channel.name);
    }
    if platform_name != "discord" {
        if let Some(kind) = channel.kind.as_deref().filter(|kind| !kind.is_empty()) {
            return format!("{} ({kind})", channel.name);
        }
    }
    channel.name.clone()
}

fn session_entry_id(origin: &serde_json::Map<String, Value>) -> Option<String> {
    let chat_id = value_to_string(origin.get("chat_id"))?;
    value_to_string(origin.get("thread_id"))
        .filter(|thread_id| !thread_id.is_empty())
        .map(|thread_id| format!("{chat_id}:{thread_id}"))
        .or(Some(chat_id))
}

fn session_entry_name(origin: &serde_json::Map<String, Value>) -> String {
    let base_name = value_to_string(origin.get("chat_name"))
        .or_else(|| value_to_string(origin.get("user_name")))
        .or_else(|| value_to_string(origin.get("chat_id")))
        .unwrap_or_else(|| "unknown".to_string());
    let Some(thread_id) = value_to_string(origin.get("thread_id")).filter(|s| !s.is_empty()) else {
        return base_name;
    };
    let topic =
        value_to_string(origin.get("chat_topic")).unwrap_or_else(|| format!("topic {thread_id}"));
    format!("{base_name} / {topic}")
}

fn value_to_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn merge_entries(primary: Vec<ChannelEntry>, secondary: Vec<ChannelEntry>) -> Vec<ChannelEntry> {
    let mut seen = BTreeSet::new();
    let mut merged = Vec::new();
    for entry in primary.into_iter().chain(secondary) {
        if seen.insert(entry.id.clone()) {
            merged.push(entry);
        }
    }
    merged
}

fn title_case_platform(platform: &str) -> String {
    let mut chars = platform.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

const SESSION_DISCOVERY_PLATFORMS: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "whatsapp",
    "signal",
    "matrix",
    "mattermost",
    "dingtalk",
    "feishu",
    "wecom",
    "wecom_callback",
    "weixin",
    "qqbot",
    "qq",
    "bluebubbles",
    "email",
    "sms",
    "homeassistant",
    "ntfy",
];

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    struct StaticProvider {
        platform: &'static str,
        entries: Vec<ChannelEntry>,
    }

    #[async_trait]
    impl ChannelDirectoryProvider for StaticProvider {
        fn platform_name(&self) -> &str {
            self.platform
        }

        async fn list_channel_entries(&self) -> Result<Vec<ChannelEntry>, GatewayError> {
            Ok(self.entries.clone())
        }
    }

    struct FailingProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ChannelDirectoryProvider for FailingProvider {
        fn platform_name(&self) -> &str {
            "slack"
        }

        async fn list_channel_entries(&self) -> Result<Vec<ChannelEntry>, GatewayError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(GatewayError::ConnectionFailed("boom".into()))
        }
    }

    fn write_directory(dir: &Path, platforms: BTreeMap<String, Vec<ChannelEntry>>) -> PathBuf {
        let path = dir.join("channel_directory.json");
        let snapshot = ChannelDirectorySnapshot {
            updated_at: Some("2026-01-01T00:00:00".into()),
            platforms,
        };
        write_directory_atomic(&path, &snapshot).expect("write directory");
        path
    }

    #[test]
    fn load_directory_missing_and_corrupt_are_empty() {
        let tmp = tempdir().expect("tmp");
        assert_eq!(
            load_directory_from(tmp.path().join("missing.json")),
            ChannelDirectorySnapshot::default()
        );

        let corrupt = tmp.path().join("bad.json");
        std::fs::write(&corrupt, "{bad json").expect("write corrupt");
        assert_eq!(
            load_directory_from(&corrupt),
            ChannelDirectorySnapshot::default()
        );
    }

    #[test]
    fn load_directory_valid_file_fills_platform_field() {
        let tmp = tempdir().expect("tmp");
        let mut platforms = BTreeMap::new();
        platforms.insert(
            "telegram".into(),
            vec![ChannelEntry::new("", "123", "John").with_kind("dm")],
        );
        let path = write_directory(tmp.path(), platforms);
        let loaded = load_directory_from(path);
        assert_eq!(loaded.platforms["telegram"][0].name, "John");
        assert_eq!(loaded.platforms["telegram"][0].platform, "telegram");
    }

    #[test]
    fn failed_write_preserves_previous_cache() {
        let tmp = tempdir().expect("tmp");
        let mut platforms = BTreeMap::new();
        platforms.insert(
            "telegram".into(),
            vec![ChannelEntry::new("telegram", "123", "Alice").with_kind("dm")],
        );
        let path = write_directory(tmp.path(), platforms);
        let previous = std::fs::read_to_string(&path).expect("previous");

        let bad_path = tmp.path().join("blocked").join("channel_directory.json");
        std::fs::write(tmp.path().join("blocked"), "not a dir").expect("block parent");
        let mut replacement = BTreeMap::new();
        replacement.insert(
            "telegram".into(),
            vec![ChannelEntry::new("telegram", "999", "Bob").with_kind("dm")],
        );
        let _ = build_channel_directory_at(&bad_path, replacement);

        assert_eq!(std::fs::read_to_string(&path).expect("current"), previous);
    }

    #[test]
    fn resolve_channel_name_matches_exact_case_prefix_guild_and_ids() {
        let tmp = tempdir().expect("tmp");
        let mut platforms = BTreeMap::new();
        platforms.insert(
            "discord".into(),
            vec![
                ChannelEntry::new("discord", "111", "general")
                    .with_guild("ServerA")
                    .with_kind("channel"),
                ChannelEntry::new("discord", "222", "general")
                    .with_guild("ServerB")
                    .with_kind("channel"),
                ChannelEntry::new("discord", "333", "bot-home")
                    .with_guild("ServerA")
                    .with_kind("channel"),
            ],
        );
        platforms.insert(
            "slack".into(),
            vec![
                ChannelEntry::new("slack", "C0B0QV5434G", "engineering").with_kind("channel"),
                ChannelEntry::new("slack", "C99", "c0b0qv5434g").with_kind("channel"),
                ChannelEntry::new("slack", "C01", "engineering-backend").with_kind("channel"),
                ChannelEntry::new("slack", "C02", "design-team").with_kind("channel"),
            ],
        );
        let path = write_directory(tmp.path(), platforms);

        assert_eq!(
            resolve_channel_name_from(&path, "discord", "#bot-home"),
            Some("333".into())
        );
        assert_eq!(
            resolve_channel_name_from(&path, "discord", "ServerA/general"),
            Some("111".into())
        );
        assert_eq!(
            resolve_channel_name_from(&path, "discord", "ServerB/general"),
            Some("222".into())
        );
        assert_eq!(
            resolve_channel_name_from(&path, "slack", "C0B0QV5434G"),
            Some("C0B0QV5434G".into())
        );
        assert_eq!(
            resolve_channel_name_from(&path, "slack", "c0b0qv5434g"),
            Some("C99".into())
        );
        assert_eq!(
            resolve_channel_name_from(&path, "slack", "design"),
            Some("C02".into())
        );
        assert_eq!(
            resolve_channel_name_from(&path, "slack", "engineering"),
            Some("C0B0QV5434G".into())
        );
    }

    #[test]
    fn resolve_channel_name_ambiguous_prefix_returns_none_and_display_suffixes_work() {
        let tmp = tempdir().expect("tmp");
        let mut platforms = BTreeMap::new();
        platforms.insert(
            "slack".into(),
            vec![
                ChannelEntry::new("slack", "C01", "eng-backend").with_kind("channel"),
                ChannelEntry::new("slack", "C02", "eng-frontend").with_kind("channel"),
            ],
        );
        platforms.insert(
            "telegram".into(),
            vec![
                ChannelEntry::new("telegram", "123", "Alice").with_kind("dm"),
                ChannelEntry::new("telegram", "456", "Dev Group").with_kind("group"),
                ChannelEntry::new("telegram", "-1001:17585", "Coaching Chat / topic 17585")
                    .with_kind("group"),
            ],
        );
        let path = write_directory(tmp.path(), platforms);

        assert_eq!(resolve_channel_name_from(&path, "slack", "eng"), None);
        assert_eq!(
            resolve_channel_name_from(&path, "telegram", "Alice (dm)"),
            Some("123".into())
        );
        assert_eq!(
            resolve_channel_name_from(&path, "telegram", "Dev Group (group)"),
            Some("456".into())
        );
        assert_eq!(
            resolve_channel_name_from(&path, "telegram", "Coaching Chat / topic 17585 (group)"),
            Some("-1001:17585".into())
        );
    }

    #[test]
    fn build_from_sessions_dedupes_and_preserves_distinct_topics() {
        let tmp = tempdir().expect("tmp");
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
        std::fs::write(
            sessions_dir.join("sessions.json"),
            serde_json::json!({
                "_README": "Gateway routing index only; full sessions live in state.db.",
                "group_root": {"origin": {"platform": "telegram", "chat_id": "-1001", "chat_name": "Coaching Chat"}, "chat_type": "group"},
                "topic_a": {"origin": {"platform": "telegram", "chat_id": "-1001", "chat_name": "Coaching Chat", "thread_id": "17585"}, "chat_type": "group"},
                "topic_b": {"origin": {"platform": "telegram", "chat_id": "-1001", "chat_name": "Coaching Chat", "thread_id": "17587"}, "chat_type": "group"},
                "_metadata": {"origin": {"platform": "telegram", "chat_id": "should-not-load"}},
                "dup": {"origin": {"platform": "telegram", "chat_id": "-1001", "chat_name": "Coaching Chat"}, "chat_type": "group"},
                "discord": {"origin": {"platform": "discord", "chat_id": "999"}}
            })
            .to_string(),
        )
        .expect("write sessions");

        let entries = build_from_sessions_in(tmp.path(), "telegram");
        let ids: BTreeSet<String> = entries.iter().map(|entry| entry.id.clone()).collect();
        let names: BTreeSet<String> = entries.iter().map(|entry| entry.name.clone()).collect();
        assert_eq!(
            ids,
            BTreeSet::from(["-1001".into(), "-1001:17585".into(), "-1001:17587".into()])
        );
        assert!(names.contains("Coaching Chat"));
        assert!(names.contains("Coaching Chat / topic 17585"));
        assert!(names.contains("Coaching Chat / topic 17587"));
    }

    #[test]
    fn build_from_sessions_skips_readme_sentinel_and_non_session_values() {
        let tmp = tempdir().expect("tmp");
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
        std::fs::write(
            sessions_dir.join("sessions.json"),
            serde_json::json!({
                "_README": "Gateway routing index ONLY; all sessions live in state.db.",
                "_README_OBJECT": {"origin": {"platform": "slack", "chat_id": "bad"}},
                "not_an_object": true,
                "slack_dm": {"origin": {"platform": "slack", "chat_id": "D123", "chat_name": "Ada"}, "chat_type": "dm"}
            })
            .to_string(),
        )
        .expect("write sessions");

        let entries = build_from_sessions_in(tmp.path(), "slack");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "D123");
        assert_eq!(entries[0].name, "Ada");
        assert_eq!(entries[0].kind.as_deref(), Some("dm"));
    }

    #[test]
    fn lookup_channel_type_and_format_display() {
        let tmp = tempdir().expect("tmp");
        let mut platforms = BTreeMap::new();
        platforms.insert(
            "discord".into(),
            vec![
                ChannelEntry::new("discord", "1", "general")
                    .with_guild("Server1")
                    .with_kind("channel"),
                ChannelEntry::new("discord", "2", "ideas")
                    .with_guild("Server1")
                    .with_kind("forum"),
                ChannelEntry::new("discord", "3", "chat")
                    .with_guild("Server2")
                    .with_kind("channel"),
            ],
        );
        platforms.insert(
            "telegram".into(),
            vec![ChannelEntry::new("telegram", "123", "Alice").with_kind("dm")],
        );
        let path = write_directory(tmp.path(), platforms);

        assert_eq!(
            lookup_channel_type_from(&path, "discord", "2"),
            Some("forum".into())
        );
        assert_eq!(lookup_channel_type_from(&path, "discord", "999"), None);
        let display = format_directory_for_display_from(&path);
        assert!(display.contains("Discord (Server1):"));
        assert!(display.contains("Discord (Server2):"));
        assert!(display.contains("discord:#general"));
        assert!(display.contains("Telegram:"));
        assert!(display.contains("telegram:Alice (dm)"));
    }

    #[tokio::test]
    async fn provider_errors_do_not_block_session_discovery_and_success_merges_sessions() {
        let tmp = tempdir().expect("tmp");
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
        std::fs::write(
            sessions_dir.join("sessions.json"),
            serde_json::json!({
                "slack_dm": {"origin": {"platform": "slack", "chat_id": "D456", "chat_name": "Bob"}},
                "slack_dup": {"origin": {"platform": "slack", "chat_id": "C001", "chat_name": "first"}}
            })
            .to_string(),
        )
        .expect("write sessions");

        let path = tmp.path().join("channel_directory.json");
        let provider: Arc<dyn ChannelDirectoryProvider> = Arc::new(StaticProvider {
            platform: "slack",
            entries: vec![ChannelEntry::new("slack", "C001", "first").with_kind("channel")],
        });
        let snapshot =
            build_channel_directory_from_providers_at(&path, tmp.path(), &[provider]).await;
        let ids: BTreeSet<String> = snapshot.platforms["slack"]
            .iter()
            .map(|entry| entry.id.clone())
            .collect();
        assert_eq!(ids, BTreeSet::from(["C001".into(), "D456".into()]));

        let calls = Arc::new(AtomicUsize::new(0));
        let failing: Arc<dyn ChannelDirectoryProvider> = Arc::new(FailingProvider {
            calls: calls.clone(),
        });
        let snapshot =
            build_channel_directory_from_providers_at(&path, tmp.path(), &[failing]).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            snapshot.platforms["slack"]
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["D456".into(), "C001".into()])
        );
    }

    #[test]
    fn empty_directory_display_mentions_no_platforms() {
        let tmp = tempdir().expect("tmp");
        assert!(
            format_directory_for_display_from(tmp.path().join("missing.json"))
                .contains("No messaging platforms")
        );
    }
}
