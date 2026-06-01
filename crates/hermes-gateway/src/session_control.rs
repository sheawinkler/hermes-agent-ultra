//! Gateway session-control primitives.
//!
//! These helpers model the Python gateway's active-session guard without
//! coupling platform adapters to a concrete Rust agent implementation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::commands::should_bypass_active_session;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionSource {
    pub platform: String,
    pub chat_id: String,
    pub chat_type: String,
    pub user_id: Option<String>,
    pub thread_id: Option<String>,
}

impl SessionSource {
    pub fn new(
        platform: impl Into<String>,
        chat_id: impl Into<String>,
        chat_type: impl Into<String>,
    ) -> Self {
        Self {
            platform: platform.into(),
            chat_id: chat_id.into(),
            chat_type: chat_type.into(),
            user_id: None,
            thread_id: None,
        }
    }

    pub fn with_user(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    pub fn with_thread(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }
}

pub fn build_session_key(source: &SessionSource) -> String {
    let mut parts = vec![source.platform.trim(), source.chat_id.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if let Some(thread_id) = source
        .thread_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(thread_id.to_string());
    }
    parts.join(":")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Text,
    Image,
    File,
    Voice,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageEvent {
    pub text: String,
    pub message_type: MessageType,
    pub source: SessionSource,
    pub message_id: Option<String>,
    pub metadata: BTreeMap<String, Value>,
}

impl MessageEvent {
    pub fn text(text: impl Into<String>, source: SessionSource) -> Self {
        Self {
            text: text.into(),
            message_type: MessageType::Text,
            source,
            message_id: None,
            metadata: BTreeMap::new(),
        }
    }

    pub fn is_command(&self) -> bool {
        self.text.trim_start().starts_with('/')
    }

    pub fn get_command(&self) -> Option<String> {
        command_name_from_text(&self.text)
    }

    pub fn get_command_args(&self) -> String {
        let trimmed = self.text.trim_start();
        if !trimmed.starts_with('/') {
            return self.text.clone();
        }
        trimmed
            .split_once(char::is_whitespace)
            .map(|(_, args)| args.trim().to_string())
            .unwrap_or_default()
    }
}

pub fn command_name_from_text(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let token = trimmed.strip_prefix('/')?.split_whitespace().next()?;
    let command = token.split('@').next().unwrap_or(token).trim();
    if command.is_empty() {
        Some(String::new())
    } else {
        Some(command.to_ascii_lowercase().replace('-', "_"))
    }
}

pub fn event_bypasses_active_session(event: &MessageEvent) -> bool {
    event
        .get_command()
        .as_deref()
        .is_some_and(|command| should_bypass_active_session(Some(command)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingOutcome {
    Success,
    Failure,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyInputMode {
    Queue,
    Interrupt,
    Steer,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentActivitySummary {
    pub api_call_count: Option<u32>,
    pub max_iterations: Option<u32>,
    pub current_tool: Option<String>,
    pub elapsed: Option<Duration>,
    pub seconds_since_activity: Option<f64>,
}

pub trait ActiveSessionControl: Send + Sync {
    fn interrupt(&self, _message: &str) {}
    fn steer(&self, _message: &str) -> bool {
        false
    }
    fn activity_summary(&self) -> Option<AgentActivitySummary> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusyMessageDecision {
    pub handled: bool,
    pub queued: bool,
    pub interrupted: bool,
    pub steered: bool,
    pub ack: Option<String>,
}

impl BusyMessageDecision {
    fn ignored() -> Self {
        Self {
            handled: false,
            queued: false,
            interrupted: false,
            steered: false,
            ack: None,
        }
    }
}

#[derive(Clone)]
struct ActiveSession {
    control: Option<Arc<dyn ActiveSessionControl>>,
    started_at: Instant,
}

pub struct BusySessionCoordinator {
    active: HashMap<String, ActiveSession>,
    pending: HashMap<String, MessageEvent>,
    busy_ack_ts: HashMap<String, Instant>,
    ack_cooldown: Duration,
}

impl Default for BusySessionCoordinator {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

impl BusySessionCoordinator {
    pub fn new(ack_cooldown: Duration) -> Self {
        Self {
            active: HashMap::new(),
            pending: HashMap::new(),
            busy_ack_ts: HashMap::new(),
            ack_cooldown,
        }
    }

    pub fn mark_active_at(
        &mut self,
        session_key: impl Into<String>,
        control: Option<Arc<dyn ActiveSessionControl>>,
        now: Instant,
    ) {
        self.active.insert(
            session_key.into(),
            ActiveSession {
                control,
                started_at: now,
            },
        );
    }

    pub fn mark_active(
        &mut self,
        session_key: impl Into<String>,
        control: Option<Arc<dyn ActiveSessionControl>>,
    ) {
        self.mark_active_at(session_key, control, Instant::now());
    }

    pub fn finish(
        &mut self,
        session_key: &str,
        _outcome: ProcessingOutcome,
    ) -> Option<MessageEvent> {
        self.active.remove(session_key);
        self.busy_ack_ts.remove(session_key);
        self.pending.remove(session_key)
    }

    pub fn is_active(&self, session_key: &str) -> bool {
        self.active.contains_key(session_key)
    }

    pub fn pending(&self, session_key: &str) -> Option<&MessageEvent> {
        self.pending.get(session_key)
    }

    fn should_ack_at(&mut self, session_key: &str, now: Instant) -> bool {
        let should = self
            .busy_ack_ts
            .get(session_key)
            .map(|last| now.duration_since(*last) >= self.ack_cooldown)
            .unwrap_or(true);
        if should {
            self.busy_ack_ts.insert(session_key.to_string(), now);
        }
        should
    }

    fn queue_event(&mut self, session_key: &str, event: MessageEvent) {
        self.pending.insert(session_key.to_string(), event);
    }

    pub fn handle_busy_message_at(
        &mut self,
        session_key: &str,
        event: MessageEvent,
        mode: BusyInputMode,
        now: Instant,
    ) -> BusyMessageDecision {
        let Some(active) = self.active.get(session_key).cloned() else {
            return BusyMessageDecision::ignored();
        };

        if event_bypasses_active_session(&event) {
            return BusyMessageDecision::ignored();
        }

        match mode {
            BusyInputMode::Queue => {
                self.queue_event(session_key, event);
                BusyMessageDecision {
                    handled: true,
                    queued: true,
                    interrupted: false,
                    steered: false,
                    ack: self.should_ack_at(session_key, now).then(|| {
                        "Queued for the next turn. I will respond once the current task finishes."
                            .to_string()
                    }),
                }
            }
            BusyInputMode::Interrupt => {
                if let Some(control) = active.control.as_ref() {
                    control.interrupt(&event.text);
                }
                BusyMessageDecision {
                    handled: true,
                    queued: false,
                    interrupted: active.control.is_some(),
                    steered: false,
                    ack: self
                        .should_ack_at(session_key, now)
                        .then(|| interrupt_ack(active.started_at, active.control.as_deref(), now)),
                }
            }
            BusyInputMode::Steer => {
                let steered = active
                    .control
                    .as_ref()
                    .map(|control| control.steer(&event.text))
                    .unwrap_or(false);
                if steered {
                    BusyMessageDecision {
                        handled: true,
                        queued: false,
                        interrupted: false,
                        steered: true,
                        ack: self.should_ack_at(session_key, now).then(|| {
                            "Steered the running task with your latest message.".to_string()
                        }),
                    }
                } else {
                    self.queue_event(session_key, event);
                    BusyMessageDecision {
                        handled: true,
                        queued: true,
                        interrupted: false,
                        steered: false,
                        ack: self.should_ack_at(session_key, now).then(|| {
                            "Queued for the next turn. I will respond once the current task finishes."
                                .to_string()
                        }),
                    }
                }
            }
        }
    }

    pub fn handle_busy_message(
        &mut self,
        session_key: &str,
        event: MessageEvent,
        mode: BusyInputMode,
    ) -> BusyMessageDecision {
        self.handle_busy_message_at(session_key, event, mode, Instant::now())
    }
}

fn interrupt_ack(
    started_at: Instant,
    control: Option<&dyn ActiveSessionControl>,
    now: Instant,
) -> String {
    let mut text = "Interrupting current task with your latest message.".to_string();
    let elapsed = now.duration_since(started_at);
    if elapsed >= Duration::from_secs(60) {
        text.push_str(&format!(" Running for {} min.", elapsed.as_secs() / 60));
    }
    if let Some(summary) = control.and_then(|control| control.activity_summary()) {
        if let (Some(done), Some(max)) = (summary.api_call_count, summary.max_iterations) {
            text.push_str(&format!(" Progress: {done}/{max}."));
        }
        if let Some(tool) = summary.current_tool.filter(|s| !s.trim().is_empty()) {
            text.push_str(&format!(" Current tool: {tool}."));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn source(thread_id: &str) -> SessionSource {
        SessionSource::new("telegram", "-1001", "group").with_thread(thread_id)
    }

    fn event(text: &str, thread_id: &str) -> MessageEvent {
        MessageEvent::text(text, source(thread_id))
    }

    #[test]
    fn session_keys_preserve_distinct_topics() {
        assert_eq!(build_session_key(&source("10")), "telegram:-1001:10");
        assert_ne!(
            build_session_key(&source("10")),
            build_session_key(&source("11"))
        );
    }

    #[test]
    fn command_parsing_strips_bot_suffix_and_lowercases() {
        let event = event("/RESET@TigerNanoBot now", "10");
        assert!(event.is_command());
        assert_eq!(event.get_command().as_deref(), Some("reset"));
        assert_eq!(event.get_command_args(), "now");
        assert_eq!(
            command_name_from_text("/reload-mcp").as_deref(),
            Some("reload_mcp")
        );
        assert_eq!(
            command_name_from_text("/path/to/file.py").as_deref(),
            Some("path/to/file.py")
        );
    }

    #[test]
    fn bypass_only_recognized_commands() {
        assert!(event_bypasses_active_session(&event("/stop", "10")));
        assert!(event_bypasses_active_session(&event("/model claude", "10")));
        assert!(event_bypasses_active_session(&event("/reload-mcp", "10")));
        assert!(!event_bypasses_active_session(&event("/foobar", "10")));
        assert!(!event_bypasses_active_session(&event(
            "/path/to/file.py",
            "10"
        )));
    }

    #[derive(Default)]
    struct MockControl {
        interrupts: Mutex<Vec<String>>,
        steer_accepts: bool,
    }

    impl ActiveSessionControl for MockControl {
        fn interrupt(&self, message: &str) {
            self.interrupts.lock().unwrap().push(message.to_string());
        }

        fn steer(&self, _message: &str) -> bool {
            self.steer_accepts
        }

        fn activity_summary(&self) -> Option<AgentActivitySummary> {
            Some(AgentActivitySummary {
                api_call_count: Some(21),
                max_iterations: Some(60),
                current_tool: Some("terminal".to_string()),
                elapsed: None,
                seconds_since_activity: Some(0.5),
            })
        }
    }

    #[test]
    fn queue_mode_queues_without_interrupt_and_debounces_ack() {
        let mut coord = BusySessionCoordinator::default();
        let key = build_session_key(&source("10"));
        let now = Instant::now();
        let control = Arc::new(MockControl::default());
        coord.mark_active_at(&key, Some(control.clone()), now);

        let first =
            coord.handle_busy_message_at(&key, event("follow up", "10"), BusyInputMode::Queue, now);
        assert!(first.handled);
        assert!(first.queued);
        assert!(!first.interrupted);
        assert!(first.ack.unwrap().contains("Queued for the next turn"));
        assert_eq!(coord.pending(&key).unwrap().text, "follow up");
        assert!(control.interrupts.lock().unwrap().is_empty());

        let second = coord.handle_busy_message_at(
            &key,
            event("still there", "10"),
            BusyInputMode::Queue,
            now + Duration::from_secs(1),
        );
        assert!(second.ack.is_none());
        assert_eq!(coord.pending(&key).unwrap().text, "still there");
    }

    #[test]
    fn interrupt_mode_calls_agent_and_includes_status_detail() {
        let mut coord = BusySessionCoordinator::default();
        let key = build_session_key(&source("10"));
        let now = Instant::now();
        let control = Arc::new(MockControl::default());
        coord.mark_active_at(&key, Some(control.clone()), now - Duration::from_secs(600));

        let decision = coord.handle_busy_message_at(
            &key,
            event("Are you working?", "10"),
            BusyInputMode::Interrupt,
            now,
        );
        assert!(decision.handled);
        assert!(decision.interrupted);
        assert!(!decision.queued);
        assert_eq!(
            control.interrupts.lock().unwrap().as_slice(),
            ["Are you working?"]
        );
        let ack = decision.ack.unwrap();
        assert!(ack.contains("Interrupting"));
        assert!(ack.contains("21/60"));
        assert!(ack.contains("terminal"));
        assert!(ack.contains("10 min"));
    }

    #[test]
    fn steer_mode_skips_queue_when_accepted_and_falls_back_when_rejected() {
        let key = build_session_key(&source("10"));
        let now = Instant::now();

        let mut accepted = BusySessionCoordinator::default();
        accepted.mark_active_at(
            &key,
            Some(Arc::new(MockControl {
                steer_accepts: true,
                ..Default::default()
            })),
            now,
        );
        let steered = accepted.handle_busy_message_at(
            &key,
            event("also check tests", "10"),
            BusyInputMode::Steer,
            now,
        );
        assert!(steered.steered);
        assert!(!steered.queued);
        assert!(accepted.pending(&key).is_none());

        let mut rejected = BusySessionCoordinator::default();
        rejected.mark_active_at(&key, Some(Arc::new(MockControl::default())), now);
        let queued = rejected.handle_busy_message_at(
            &key,
            event("cannot steer", "10"),
            BusyInputMode::Steer,
            now,
        );
        assert!(queued.queued);
        assert!(!queued.steered);
        assert_eq!(rejected.pending(&key).unwrap().text, "cannot steer");
    }

    #[test]
    fn bypass_commands_are_not_consumed_by_busy_guard() {
        let mut coord = BusySessionCoordinator::default();
        let key = build_session_key(&source("10"));
        coord.mark_active(&key, None);
        let decision =
            coord.handle_busy_message(&key, event("/status", "10"), BusyInputMode::Queue);
        assert!(!decision.handled);
        assert!(coord.pending(&key).is_none());
    }

    #[test]
    fn finish_returns_pending_and_clears_active_state() {
        let mut coord = BusySessionCoordinator::default();
        let key = build_session_key(&source("10"));
        coord.mark_active(&key, None);
        coord.handle_busy_message(&key, event("queued", "10"), BusyInputMode::Queue);
        let pending = coord.finish(&key, ProcessingOutcome::Success).unwrap();
        assert_eq!(pending.text, "queued");
        assert!(!coord.is_active(&key));
        assert!(coord.pending(&key).is_none());
    }
}
