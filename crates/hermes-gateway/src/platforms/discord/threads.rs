//! Thread participation tracking and auto-thread helpers (P1-10).

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::warn;

use hermes_core::errors::GatewayError;

use super::config::ChannelIdSet;
use super::parse::RawDiscordMessage;

const THREAD_STATE_FILE: &str = "discord_threads.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct ThreadState {
    thread_ids: Vec<String>,
}

/// Tracks thread channel IDs the bot has participated in (persisted under HERMES_HOME).
#[derive(Debug)]
pub struct ThreadParticipationTracker {
    path: PathBuf,
    ids: RwLock<HashSet<String>>,
}

impl ThreadParticipationTracker {
    pub fn load() -> Self {
        Self::load_at(hermes_home_path())
    }

    fn load_at(home: PathBuf) -> Self {
        let path = home.join(THREAD_STATE_FILE);
        let ids = load_ids(&path).unwrap_or_default();
        Self {
            path,
            ids: RwLock::new(ids),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test_home(home: PathBuf) -> Self {
        Self::load_at(home)
    }

    pub async fn contains(&self, thread_id: &str) -> bool {
        self.ids.read().await.contains(thread_id)
    }

    pub async fn mark(&self, thread_id: &str) {
        let mut guard = self.ids.write().await;
        if guard.insert(thread_id.to_string()) {
            drop(guard);
            if let Err(e) = self.persist().await {
                warn!(error = %e, thread_id = %thread_id, "Discord thread state persist failed");
            }
        }
    }

    async fn persist(&self) -> Result<(), GatewayError> {
        let ids: Vec<String> = self.ids.read().await.iter().cloned().collect();
        let state = ThreadState { thread_ids: ids };
        let json = serde_json::to_string_pretty(&state).map_err(|e| {
            GatewayError::Platform(format!("discord_threads serialize: {e}"))
        })?;
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                GatewayError::Platform(format!("discord_threads mkdir: {e}"))
            })?;
        }
        tokio::fs::write(&self.path, json).await.map_err(|e| {
            GatewayError::Platform(format!("discord_threads write: {e}"))
        })?;
        Ok(())
    }
}

fn hermes_home_path() -> PathBuf {
    hermes_config::hermes_home()
}

fn load_ids(path: &PathBuf) -> Result<HashSet<String>, GatewayError> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| GatewayError::Platform(format!("discord_threads read: {e}")))?;
    let state: ThreadState = serde_json::from_str(&text)
        .map_err(|e| GatewayError::Platform(format!("discord_threads parse: {e}")))?;
    Ok(state.thread_ids.into_iter().collect())
}

/// Whether auto-thread should run for this guild @mention message.
pub fn should_auto_thread(
    raw: &RawDiscordMessage,
    auto_thread: bool,
    no_thread_channels: &ChannelIdSet,
    free_response_channels: &ChannelIdSet,
    bot_user_id: Option<&str>,
) -> bool {
    if !auto_thread {
        return false;
    }
    if raw.guild_id.is_none() {
        return false;
    }
    if raw.parent_channel_id.is_some() {
        return false;
    }
    if no_thread_channels.contains(&raw.channel_id) {
        return false;
    }
    if free_response_channels.contains(&raw.channel_id) {
        return false;
    }
    let Some(bot_id) = bot_user_id else {
        return false;
    };
    raw.mentions.iter().any(|m| m == bot_id)
        || super::filter::content_mentions_bot(&raw.content, bot_id)
}

pub fn auto_thread_name(raw: &RawDiscordMessage) -> String {
    let base = raw
        .username
        .as_deref()
        .or(raw.user_id.as_deref())
        .unwrap_or("user");
    let trimmed: String = raw
        .content
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    if trimmed.is_empty() {
        format!("hermes-{base}")
    } else {
        format!("hermes-{base}: {trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn raw_guild_mention(content: &str) -> RawDiscordMessage {
        RawDiscordMessage {
            channel_id: "ch1".into(),
            message_id: "m1".into(),
            user_id: Some("u1".into()),
            username: Some("alice".into()),
            content: content.into(),
            is_bot: false,
            guild_id: Some("g1".into()),
            mentions: vec!["bot99".into()],
            message_type: 0,
            attachments: vec![],
            role_ids: vec![],
            parent_channel_id: None,
        }
    }

    #[test]
    fn auto_thread_requires_guild_mention() {
        assert!(should_auto_thread(
            &raw_guild_mention("hi"),
            true,
            &ChannelIdSet::new(),
            &ChannelIdSet::new(),
            Some("bot99")
        ));
    }

    #[test]
    fn auto_thread_skips_existing_thread() {
        let mut raw = raw_guild_mention("hi");
        raw.parent_channel_id = Some("parent".into());
        assert!(!should_auto_thread(
            &raw,
            true,
            &ChannelIdSet::new(),
            &ChannelIdSet::new(),
            Some("bot99")
        ));
    }

    fn with_test_home<F, Fut>(suffix: &str, f: F)
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let dir = std::env::temp_dir().join(format!("hermes-discord-thread-{suffix}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let home = dir.clone();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(f(home));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tracker_starts_empty_without_state_file() {
        with_test_home("empty", |home| async move {
            let tracker = ThreadParticipationTracker::for_test_home(home);
            assert!(!tracker.contains("111").await);
        });
    }

    #[test]
    fn tracker_mark_persists_to_disk() {
        with_test_home("persist", |home| async move {
            let tracker = ThreadParticipationTracker::for_test_home(home.clone());
            tracker.mark("111").await;
            tracker.mark("222").await;
            let state_file = home.join(THREAD_STATE_FILE);
            assert!(state_file.exists());
            let saved: ThreadState =
                serde_json::from_str(&std::fs::read_to_string(&state_file).unwrap()).unwrap();
            let ids: HashSet<String> = saved.thread_ids.into_iter().collect();
            assert_eq!(ids, HashSet::from(["111".into(), "222".into()]));
        });
    }

    #[test]
    fn tracker_persists_across_load() {
        with_test_home("reload", |home| async move {
            let t1 = ThreadParticipationTracker::for_test_home(home.clone());
            t1.mark("thread-abc").await;
            let t2 = ThreadParticipationTracker::for_test_home(home);
            assert!(t2.contains("thread-abc").await);
        });
    }

    #[test]
    fn tracker_duplicate_mark_writes_once() {
        with_test_home("dup", |home| async move {
            let tracker = ThreadParticipationTracker::for_test_home(home.clone());
            tracker.mark("111").await;
            tracker.mark("111").await;
            let state_file = home.join(THREAD_STATE_FILE);
            let saved: ThreadState =
                serde_json::from_str(&std::fs::read_to_string(&state_file).unwrap()).unwrap();
            assert_eq!(saved.thread_ids.iter().filter(|id| *id == "111").count(), 1);
        });
    }
}
