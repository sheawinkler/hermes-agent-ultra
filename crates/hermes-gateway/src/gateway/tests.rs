use super::*;
use crate::hooks::{HookEvent, HookHandler, HookRegistry};
use crate::session::SessionManager;
use async_trait::async_trait;
use hermes_config::session::SessionConfig;
use hermes_core::errors::PartialDelivery;
use hermes_core::traits::NativeMediaFailureClass;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

struct TestAdapter {
    messages: Arc<Mutex<Vec<(String, String)>>>,
}

struct FailingSendAdapter {
    messages: Arc<Mutex<Vec<(String, String)>>>,
    error: &'static str,
}

struct FailFirstSendAdapter {
    messages: Arc<Mutex<Vec<(String, String)>>>,
    failures_remaining: std::sync::atomic::AtomicUsize,
}

struct BlockingFirstSendAdapter {
    messages: Arc<Mutex<Vec<(String, String)>>>,
    block_first: std::sync::atomic::AtomicBool,
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

struct PanickingSendAdapter;

struct PanickingLifecycleAdapter;

#[derive(Clone, Copy)]
enum StopProbeOutcome {
    Ok,
    Error,
    Panic,
}

struct StopProbeAdapter {
    platform: &'static str,
    outcome: StopProbeOutcome,
    stop_calls: Arc<std::sync::atomic::AtomicUsize>,
}

struct StatusUpdateAdapter {
    updates: Arc<Mutex<Vec<(String, String, String)>>>,
}

struct ReactionTestAdapter {
    messages: Arc<Mutex<Vec<(String, String)>>>,
    reactions: Arc<Mutex<Vec<String>>>,
}

struct BlockingReactionRemoveAdapter {
    reactions: Arc<Mutex<Vec<String>>>,
    remove_started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release_remove: Arc<tokio::sync::Notify>,
    completion_started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release_completion: Option<Arc<tokio::sync::Notify>>,
}

type RecordedFile = (String, Vec<u8>);
type RecordedMarkerFile = (String, String, Vec<u8>, bool);

struct FileTestAdapter {
    files: Arc<Mutex<Vec<RecordedFile>>>,
}

struct MediaMarkerTestAdapter {
    messages: Arc<Mutex<Vec<(String, String)>>>,
    files: Arc<Mutex<Vec<RecordedMarkerFile>>>,
}

#[derive(Clone, Copy)]
enum NativeMediaFixtureFailure {
    AcceptedButMalformed,
    ExplicitlyUnsupported,
    ExplicitlyRejected,
    Unclassified,
}

struct NativeMediaFailureAdapter {
    text_sends: Arc<Mutex<Vec<String>>>,
    file_attempts: Arc<Mutex<Vec<String>>>,
    failure: NativeMediaFixtureFailure,
}

type ThreadSendRecord = (String, String, Option<String>, bool);

struct ThreadOptionTestAdapter {
    sends: Arc<Mutex<Vec<ThreadSendRecord>>>,
}

struct RecordingHook {
    seen: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
}

struct FailingHook;
struct PanickingHook;

struct BlockingHook {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Arc<tokio::sync::Notify>,
}

#[derive(Default)]
struct BusyControlProbe {
    interrupts: Mutex<Vec<String>>,
    steers: Mutex<Vec<String>>,
}

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct HermesHomeEnvGuard {
    prev_home: Option<String>,
    prev_ultra_home: Option<String>,
    _guard: MutexGuard<'static, ()>,
}

impl HermesHomeEnvGuard {
    fn set(path: &std::path::Path) -> Self {
        let guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned");
        let prev_home = std::env::var("HERMES_HOME").ok();
        let prev_ultra_home = std::env::var("HERMES_AGENT_ULTRA_HOME").ok();
        std::env::set_var("HERMES_HOME", path);
        std::env::remove_var("HERMES_AGENT_ULTRA_HOME");
        Self {
            prev_home,
            prev_ultra_home,
            _guard: guard,
        }
    }
}

impl Drop for HermesHomeEnvGuard {
    fn drop(&mut self) {
        match &self.prev_home {
            Some(value) => std::env::set_var("HERMES_HOME", value),
            None => std::env::remove_var("HERMES_HOME"),
        }
        match &self.prev_ultra_home {
            Some(value) => std::env::set_var("HERMES_AGENT_ULTRA_HOME", value),
            None => std::env::remove_var("HERMES_AGENT_ULTRA_HOME"),
        }
    }
}

struct EnvVarsGuard {
    previous: Vec<(String, Option<String>)>,
    _guard: MutexGuard<'static, ()>,
}

impl EnvVarsGuard {
    fn set(vars: &[(&str, &str)]) -> Self {
        let guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned");
        let previous = vars
            .iter()
            .map(|(key, _)| ((*key).to_string(), std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in vars {
            std::env::set_var(key, value);
        }
        Self {
            previous,
            _guard: guard,
        }
    }
}

impl Drop for EnvVarsGuard {
    fn drop(&mut self) {
        for (key, value) in &self.previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn test_incoming(text: impl Into<String>) -> IncomingMessage {
    IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: text.into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    }
}

#[test]
fn gateway_runtime_context_derives_private_subject_and_rejects_missing_identity() {
    let context = GatewayRuntimeContext {
        platform: "discord".to_string(),
        tenant_id: Some("discord-tenant".to_string()),
        team_id: Some("guild-42".to_string()),
        session_id: Some("session-42".to_string()),
        is_dm: false,
        chat_id: "channel-42".to_string(),
        user_id: "user-42".to_string(),
        ..GatewayRuntimeContext::default()
    };
    let subject = context.memory_subject().expect("authenticated subject");
    assert_eq!(subject.visibility(), hermes_core::MemoryVisibility::Private);
    assert_eq!(
        subject.team_id().map(hermes_core::MemoryTeamId::as_str),
        Some("guild-42")
    );

    let mut missing = context.clone();
    missing.session_id = None;
    assert!(matches!(
        missing.memory_subject(),
        Err(hermes_core::MemorySubjectError::MissingAuthenticatedField {
            field: "session id"
        })
    ));

    let mut missing_platform = context.clone();
    missing_platform.platform.clear();
    assert!(matches!(
        missing_platform.memory_subject(),
        Err(hermes_core::MemorySubjectError::MissingAuthenticatedField {
            field: "platform"
        })
    ));

    let mut delimiter = context.clone();
    delimiter.user_id = "user:other".to_string();
    assert!(delimiter.memory_subject().is_err());

    let mut invalid_thread = context;
    invalid_thread.thread_id = Some("thread:other".to_string());
    assert!(invalid_thread.memory_subject().is_err());
}

#[async_trait]
impl HookHandler for RecordingHook {
    async fn handle(&self, event: &HookEvent) -> Result<(), String> {
        self.seen
            .lock()
            .unwrap()
            .push((event.event_type.clone(), event.context.clone()));
        Ok(())
    }

    fn name(&self) -> &str {
        "recording-hook"
    }
}

#[async_trait]
impl HookHandler for FailingHook {
    async fn handle(&self, _event: &HookEvent) -> Result<(), String> {
        Err("boom".to_string())
    }

    fn name(&self) -> &str {
        "failing-hook"
    }
}

#[async_trait]
impl HookHandler for PanickingHook {
    async fn handle(&self, _event: &HookEvent) -> Result<(), String> {
        panic!("deterministic hook panic");
    }

    fn name(&self) -> &str {
        panic!("deterministic hook name panic");
    }
}

#[async_trait]
impl HookHandler for BlockingHook {
    async fn handle(&self, _event: &HookEvent) -> Result<(), String> {
        if let Some(started) = self.started.lock().unwrap().take() {
            let _ = started.send(());
        }
        self.release.notified().await;
        Ok(())
    }

    fn name(&self) -> &str {
        "blocking-hook"
    }
}

impl ActiveSessionControl for BusyControlProbe {
    fn interrupt(&self, message: &str) {
        self.interrupts.lock().unwrap().push(message.to_string());
    }

    fn steer(&self, message: &str) -> bool {
        self.steers.lock().unwrap().push(message.to_string());
        true
    }
}

#[async_trait]
impl PlatformAdapter for TestAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.messages
            .lock()
            .unwrap()
            .push((chat_id.to_string(), text.to_string()));
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let mut marker = format!("[image] {image_url}");
        if let Some(cap) = caption.map(str::trim).filter(|s| !s.is_empty()) {
            marker.push_str(&format!(" | caption={cap}"));
        }
        self.messages
            .lock()
            .unwrap()
            .push((chat_id.to_string(), marker));
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "test"
    }
}

#[async_trait]
impl PlatformAdapter for StopProbeAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        self.stop_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.outcome {
            StopProbeOutcome::Ok => Ok(()),
            StopProbeOutcome::Error => Err(GatewayError::Auth(
                "top-secret shutdown credential must not be exposed".to_string(),
            )),
            StopProbeOutcome::Panic => panic!("deterministic shutdown panic"),
        }
    }

    async fn send_message(
        &self,
        _chat_id: &str,
        _text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        self.platform
    }
}

#[async_trait]
impl PlatformAdapter for FailFirstSendAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.messages
            .lock()
            .unwrap()
            .push((chat_id.to_string(), text.to_string()));
        let should_fail = self
            .failures_remaining
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok();
        if should_fail {
            Err(GatewayError::SendFailed(
                "deterministic first send failure".to_string(),
            ))
        } else {
            Ok(())
        }
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "test"
    }
}

#[async_trait]
impl PlatformAdapter for BlockingFirstSendAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.messages
            .lock()
            .unwrap()
            .push((chat_id.to_string(), text.to_string()));
        if self
            .block_first
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            if let Some(started) = self.started.lock().unwrap().take() {
                let _ = started.send(());
            }
            std::future::pending::<()>().await;
        }
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "test"
    }
}

#[async_trait]
impl PlatformAdapter for FailingSendAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.messages
            .lock()
            .unwrap()
            .push((chat_id.to_string(), text.to_string()));
        Err(GatewayError::SendFailed(self.error.to_string()))
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "test"
    }
}

#[async_trait]
impl PlatformAdapter for PanickingSendAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        _chat_id: &str,
        _text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        panic!("deterministic adapter panic");
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "test"
    }
}

#[async_trait]
impl PlatformAdapter for PanickingLifecycleAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        panic!("deterministic adapter startup panic");
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        panic!("deterministic adapter shutdown panic");
    }

    async fn send_message(
        &self,
        _chat_id: &str,
        _text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    fn is_running(&self) -> bool {
        false
    }

    fn platform_name(&self) -> &str {
        "test"
    }
}

#[async_trait]
impl PlatformAdapter for FileTestAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        _chat_id: &str,
        _text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let contents = std::fs::read(file_path).map_err(|error| {
            GatewayError::SendFailed(format!(
                "file test adapter failed to read staged file: {error}"
            ))
        })?;
        self.files
            .lock()
            .unwrap()
            .push((file_path.to_string(), contents));
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "file-test"
    }
}

#[async_trait]
impl PlatformAdapter for MediaMarkerTestAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.messages
            .lock()
            .unwrap()
            .push((chat_id.to_string(), text.to_string()));
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let contents = std::fs::read(file_path).map_err(|error| {
            GatewayError::SendFailed(format!(
                "media marker test adapter failed to read staged file: {error}"
            ))
        })?;
        self.files.lock().unwrap().push((
            chat_id.to_string(),
            file_path.to_string(),
            contents,
            false,
        ));
        Ok(())
    }

    async fn send_file_with_options(
        &self,
        chat_id: &str,
        file_path: &str,
        _caption: Option<&str>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let contents = std::fs::read(file_path).map_err(|error| {
            GatewayError::SendFailed(format!(
                "media marker test adapter failed to read staged file: {error}"
            ))
        })?;
        self.files.lock().unwrap().push((
            chat_id.to_string(),
            file_path.to_string(),
            contents,
            options.send_as_voice,
        ));
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "media-marker-test"
    }
}

impl NativeMediaFailureAdapter {
    fn media_failure(&self) -> GatewayError {
        match self.failure {
            NativeMediaFixtureFailure::AcceptedButMalformed => {
                GatewayError::PartialDelivery(PartialDelivery {
                    platform: "discord".to_string(),
                    chat_id: "chat1".to_string(),
                    thread_id: None,
                    reply_to_message_id: None,
                    reply_to_mode: "always".to_string(),
                    non_conversational: false,
                    next_chunk_index: 1,
                    remaining_text: String::new(),
                    accepted_message_ids: vec!["accepted-media".to_string()],
                    accepted_chunks: 1,
                    retryable: false,
                    retry_after: None,
                    failure: "accepted Discord response body was malformed".to_string(),
                    repair_required: None,
                })
            }
            NativeMediaFixtureFailure::ExplicitlyUnsupported => {
                GatewayError::Platform("native attachment unsupported".to_string())
            }
            NativeMediaFixtureFailure::ExplicitlyRejected => {
                GatewayError::Platform("native attachment rejected".to_string())
            }
            NativeMediaFixtureFailure::Unclassified => {
                GatewayError::SendFailed("native attachment transport failed".to_string())
            }
        }
    }
}

#[async_trait]
impl PlatformAdapter for NativeMediaFailureAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        _chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.text_sends.lock().unwrap().push(text.to_string());
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.file_attempts.lock().unwrap().push(file_path.to_string());
        Err(self.media_failure())
    }

    async fn send_file_with_options(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        _options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        self.send_file(chat_id, file_path, caption).await
    }

    fn classify_native_media_failure(
        &self,
        _error: &GatewayError,
    ) -> NativeMediaFailureClass {
        match self.failure {
            NativeMediaFixtureFailure::ExplicitlyUnsupported => {
                NativeMediaFailureClass::Unsupported
            }
            NativeMediaFixtureFailure::ExplicitlyRejected => NativeMediaFailureClass::Rejected,
            NativeMediaFixtureFailure::AcceptedButMalformed
            | NativeMediaFixtureFailure::Unclassified => NativeMediaFailureClass::Uncertain,
        }
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "native-media-failure-test"
    }
}

#[async_trait]
impl PlatformAdapter for ThreadOptionTestAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.sends
            .lock()
            .unwrap()
            .push((chat_id.to_string(), text.to_string(), None, false));
        Ok(())
    }

    async fn send_message_with_options(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        self.sends.lock().unwrap().push((
            chat_id.to_string(),
            text.to_string(),
            options.thread_id,
            options.notify,
        ));
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }


    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "thread-option-test"
    }
}

#[test]
fn gateway_prefill_loader_parses_json_message_array() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prefill.json");
    std::fs::write(
            &path,
            r#"[{"role":"system","content":"gateway system"},{"role":"assistant","content":"gateway assistant"}]"#,
        )
        .unwrap();

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let messages = gw.load_prefill_messages(&path);

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::System);
    assert_eq!(messages[0].content.as_deref(), Some("gateway system"));
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[1].content.as_deref(), Some("gateway assistant"));
}

#[tokio::test]
async fn gateway_quick_exec_returns_stdout_from_config() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    let mut cfg = GatewayConfig::default();
    cfg.quick_commands.insert(
        "limits".to_string(),
        QuickCommandConfig {
            kind: "exec".to_string(),
            command: Some("printf ok".to_string()),
            ..Default::default()
        },
    );
    let gw = Gateway::new(session_mgr, dm_manager, cfg);

    let reply = gw
        .resolve_quick_command("/limits")
        .await
        .expect("quick command")
        .expect("reply");

    assert_eq!(reply, "ok");
}

#[tokio::test]
async fn gateway_quick_exec_sanitizes_credential_environment() {
    let _env = EnvVarsGuard::set(&[
        ("OPENAI_API_KEY", "sk-should-not-leak"),
        ("GH_TOKEN", "ghp-should-not-leak"),
        ("SAFE_QUICK_ENV", "keep-me"),
    ]);
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    let mut cfg = GatewayConfig::default();
    cfg.quick_commands.insert(
        "envcheck".to_string(),
        QuickCommandConfig {
            kind: "exec".to_string(),
            command: Some(
                "printf '%s|%s|%s' \"${OPENAI_API_KEY:-}\" \"${GH_TOKEN:-}\" \"${SAFE_QUICK_ENV:-}\""
                    .to_string(),
            ),
            ..Default::default()
        },
    );
    let gw = Gateway::new(session_mgr, dm_manager, cfg);

    let reply = gw
        .resolve_quick_command("/envcheck")
        .await
        .expect("quick command")
        .expect("reply");

    assert_eq!(reply, "||keep-me");
}

#[tokio::test]
async fn gateway_quick_exec_reports_timeout_and_missing_command() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    let mut cfg = GatewayConfig::default();
    cfg.quick_commands.insert(
        "slow".to_string(),
        QuickCommandConfig {
            kind: "exec".to_string(),
            command: Some("sleep 1".to_string()),
            timeout_secs: Some(0),
            ..Default::default()
        },
    );
    cfg.quick_commands.insert(
        "oops".to_string(),
        QuickCommandConfig {
            kind: "exec".to_string(),
            ..Default::default()
        },
    );
    let gw = Gateway::new(session_mgr, dm_manager, cfg);

    let timeout = gw
        .resolve_quick_command("/slow")
        .await
        .expect("timeout command")
        .expect("timeout reply");
    assert!(timeout.contains("timed out"));

    let missing = gw
        .resolve_quick_command("/oops")
        .await
        .expect("missing command")
        .expect("missing reply");
    assert!(missing.contains("no command defined"));
}

#[tokio::test]
async fn gateway_quick_alias_routes_to_builtin_command() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    let mut cfg = GatewayConfig::default();
    cfg.quick_commands.insert(
        "stat".to_string(),
        QuickCommandConfig {
            kind: "alias".to_string(),
            target: Some("/status".to_string()),
            ..Default::default()
        },
    );
    let gw = Gateway::new(session_mgr, dm_manager, cfg);

    let reply = gw
        .resolve_quick_command("/stat")
        .await
        .expect("alias command")
        .expect("alias reply");

    assert!(reply.contains("Status information"));
}

#[test]
fn split_slash_command_normalizes_ios_dashes_in_args_only() {
    let (cmd, args) =
        Gateway::split_slash_command("  /queue  deploy —fast ——dry-run –target prod ‒scope −1");

    assert_eq!(cmd, "/queue");
    assert_eq!(args, "deploy --fast --dry-run -target prod -scope -1");

    let (cmd, args) = Gateway::split_slash_command("/model glm-5.2 —provider zai —session");
    assert_eq!(cmd, "/model");
    assert_eq!(args, "glm-5.2 --provider zai --session");
    assert_eq!(
        Gateway::normalize_slash_command_text(" /model glm-5.2 —provider zai —session "),
        "/model glm-5.2 --provider zai --session"
    );
}

#[tokio::test]
async fn gateway_unknown_slash_invokes_installed_skill_command() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _home_guard = HermesHomeEnvGuard::set(tmp.path());
    let skill_dir = tmp.path().join("skills").join("release-captain");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Release Captain\ndescription: Release workflow\n---\n# Release Captain\n1. Inspect changed files\n2. Run deterministic gates\n",
        )
        .expect("write skill");

    let sent = Arc::new(Mutex::new(Vec::new()));
    let seen_user_messages = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    let seen_for_handler = seen_user_messages.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let seen = seen_for_handler.clone();
        Box::pin(async move {
            let user = messages
                .iter()
                .rev()
                .find(|m| m.role == MessageRole::User)
                .and_then(|m| m.content.clone())
                .unwrap_or_default();
            seen.lock().expect("seen lock").push(user);
            Ok("skill-ran".to_string())
        })
    }))
    .await;

    gw.route_message(&IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/release_captain ship it".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    })
    .await
    .expect("route skill command");

    assert_eq!(
        sent.lock().expect("sent lock").as_slice(),
        &[("chat1".to_string(), "skill-ran".to_string())]
    );
    let seen = seen_user_messages.lock().expect("seen lock");
    assert_eq!(seen.len(), 1);
    assert!(seen[0].contains("Release Captain"));
    assert!(seen[0].contains("Inspect changed files"));
    assert!(seen[0].contains("ship it"));
}

#[tokio::test]
async fn gateway_reload_skills_replies_and_injects_one_shot_note() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _home_guard = HermesHomeEnvGuard::set(tmp.path());
    let skill_dir = tmp.path().join("skills").join("release-captain");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Release Captain\ndescription: Release workflow\n---\n# Release Captain\n1. Inspect changed files\n",
        )
        .expect("write skill");

    let sent = Arc::new(Mutex::new(Vec::new()));
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    let seen_for_handler = seen_messages.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let seen = seen_for_handler.clone();
        Box::pin(async move {
            seen.lock().expect("seen lock").push(messages);
            Ok("agent-ok".to_string())
        })
    }))
    .await;

    gw.route_message(&IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/reload-skills".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    })
    .await
    .expect("reload skills");
    gw.route_message(&IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "next turn".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    })
    .await
    .expect("next turn");
    gw.route_message(&IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "later turn".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    })
    .await
    .expect("later turn");

    let sent = sent.lock().expect("sent lock");
    assert!(sent[0].1.contains("Reloaded installed skill commands"));
    assert!(sent[0].1.contains("/release-captain"));
    assert_eq!(sent[1].1, "agent-ok");
    assert_eq!(sent[2].1, "agent-ok");
    drop(sent);

    let seen = seen_messages.lock().expect("seen lock");
    assert_eq!(seen.len(), 2);
    let first_note_count = seen[0]
        .iter()
        .filter(|message| {
            message.role == MessageRole::System
                && message
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("/reload-skills"))
        })
        .count();
    let second_note_count = seen[1]
        .iter()
        .filter(|message| {
            message.role == MessageRole::System
                && message
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("/reload-skills"))
        })
        .count();
    assert_eq!(first_note_count, 1);
    assert_eq!(second_note_count, 0);
}

#[async_trait]
impl PlatformAdapter for StatusUpdateAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.updates.lock().unwrap().push((
            chat_id.to_string(),
            "message".to_string(),
            text.to_string(),
        ));
        Ok(())
    }

    async fn send_or_update_status(
        &self,
        chat_id: &str,
        status_key: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.updates.lock().unwrap().push((
            chat_id.to_string(),
            status_key.to_string(),
            text.to_string(),
        ));
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "status-test"
    }
}

#[async_trait]
impl PlatformAdapter for ReactionTestAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.messages
            .lock()
            .unwrap()
            .push((chat_id.to_string(), text.to_string()));
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn add_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        self.reactions
            .lock()
            .unwrap()
            .push(format!("add:{chat_id}:{message_id}:{emoji}"));
        Ok(())
    }

    async fn remove_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        self.reactions
            .lock()
            .unwrap()
            .push(format!("remove:{chat_id}:{message_id}:{emoji}"));
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "slack"
    }
}

#[async_trait]
impl PlatformAdapter for BlockingReactionRemoveAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        _chat_id: &str,
        _text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn add_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        if emoji != "eyes" {
            if let Some(started) = self.completion_started.lock().unwrap().take() {
                let _ = started.send(());
            }
            if let Some(release) = &self.release_completion {
                release.notified().await;
            }
        }
        self.reactions
            .lock()
            .unwrap()
            .push(format!("add:{chat_id}:{message_id}:{emoji}"));
        Ok(())
    }

    async fn remove_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        if let Some(started) = self.remove_started.lock().unwrap().take() {
            let _ = started.send(());
        }
        self.release_remove.notified().await;
        self.reactions
            .lock()
            .unwrap()
            .push(format!("remove:{chat_id}:{message_id}:{emoji}"));
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "slack"
    }
}

#[test]
fn gateway_config_default() {
    let cfg = GatewayConfig::default();
    assert!(cfg.ssrf_protection);
    assert!(cfg.media_cache_dir.is_none());
    assert_eq!(cfg.media_cache_max_bytes, 0);
    assert!(!cfg.streaming_enabled);
    assert_eq!(cfg.turn_timeout_secs, 30 * 60);
}

#[tokio::test]
async fn gateway_register_and_list_adapters() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());

    assert!(gw.adapter_names().await.is_empty());
}

#[tokio::test]
async fn gateway_contains_adapter_lifecycle_panics() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());
    gw.register_adapter("panic", Arc::new(PanickingLifecycleAdapter))
        .await;

    let start_error = tokio::time::timeout(std::time::Duration::from_secs(2), gw.start_all())
        .await
        .expect("startup panic must remain bounded")
        .expect_err("startup panic must become an error");
    assert!(start_error.to_string().contains("panicked during startup"));
    let shutdown_error = tokio::time::timeout(std::time::Duration::from_secs(2), gw.stop_all())
        .await
        .expect("shutdown panic must remain bounded")
        .expect_err("shutdown panic must be returned after cleanup");
    assert!(
        shutdown_error.to_string().contains("panic:panic"),
        "shutdown must report the sanitized panic outcome: {shutdown_error}"
    );
}

#[tokio::test]
async fn gateway_stop_all_attempts_every_adapter_and_aggregates_sanitized_failures() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gateway = Gateway::with_defaults(session_mgr, GatewayConfig::default());
    let failed_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let successful_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let panicked_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    gateway
        .register_adapter(
            "failing",
            Arc::new(StopProbeAdapter {
                platform: "failing",
                outcome: StopProbeOutcome::Error,
                stop_calls: Arc::clone(&failed_stops),
            }),
        )
        .await;
    gateway
        .register_adapter(
            "successful",
            Arc::new(StopProbeAdapter {
                platform: "successful",
                outcome: StopProbeOutcome::Ok,
                stop_calls: Arc::clone(&successful_stops),
            }),
        )
        .await;
    gateway
        .register_adapter(
            "panicking",
            Arc::new(StopProbeAdapter {
                platform: "panicking",
                outcome: StopProbeOutcome::Panic,
                stop_calls: Arc::clone(&panicked_stops),
            }),
        )
        .await;

    let shutdown_error = gateway
        .stop_all()
        .await
        .expect_err("adapter shutdown failures must fail the gateway shutdown");
    let summary = shutdown_error.to_string();
    assert_eq!(
        failed_stops.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the failing adapter is attempted once"
    );
    assert_eq!(
        successful_stops.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a later healthy adapter must still be stopped"
    );
    assert_eq!(
        panicked_stops.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a panicking adapter is attempted once"
    );
    assert!(summary.contains("failing:auth"), "missing typed error: {summary}");
    assert!(
        summary.contains("panicking:panic"),
        "missing typed panic: {summary}"
    );
    assert!(
        !summary.contains("top-secret"),
        "raw adapter failure details must not leak: {summary}"
    );
}

#[tokio::test]
async fn gateway_agent_error_sends_user_notification() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    let handler_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_for_handler = handler_calls.clone();
    gw.set_message_handler(Arc::new(move |_messages| {
        let calls = calls_for_handler.clone();
        Box::pin(async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(GatewayError::Platform("agent boom".to_string()))
        })
    }))
    .await;

    let mut incoming = test_incoming("explode");
    incoming.message_id = Some("notified-failure".to_string());
    let err = gw
        .route_message(&incoming)
        .await
        .expect_err("handler failure should still propagate");

    assert_eq!(err.to_string(), "Platform error: agent boom");
    assert!(gw.route_message(&incoming).await.is_ok());
    assert_eq!(
        handler_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a delivered error notice makes the failed message ID terminal"
    );
    let sent = sent.lock().expect("sent lock");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "chat1");
    assert!(sent[0]
        .1
        .contains("Sorry, I encountered an error (Platform)."));
    assert!(sent[0].1.contains("Platform error: agent boom"));
    assert!(sent[0].1.contains("Try again or use /reset"));
}

#[tokio::test]
async fn gateway_error_notification_failure_quarantines_original_ingress() {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailingSendAdapter {
        messages: attempts.clone(),
        error: "notify transport down",
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    let handler_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_for_handler = handler_calls.clone();
    gw.set_message_handler(Arc::new(move |_messages| {
        let calls = calls_for_handler.clone();
        Box::pin(async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(GatewayError::Platform("agent boom".to_string()))
        })
    }))
    .await;

    let mut incoming = test_incoming("explode");
    incoming.message_id = Some("unnotified-failure".to_string());
    let err = gw
        .route_message(&incoming)
        .await
        .expect_err("original handler failure should still propagate");

    assert_eq!(err.to_string(), "Platform error: agent boom");
    assert!(
        gw.route_message(&incoming).await.is_ok(),
        "uncertain error-notification delivery must retain terminal dedup ownership"
    );
    assert_eq!(
        handler_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "an unnotified handler failure is uncertain and must never re-run side effects"
    );
    let attempts = attempts.lock().expect("attempts lock");
    assert_eq!(attempts.len(), 1);
    assert!(attempts[0].1.contains("Platform error: agent boom"));
}

#[tokio::test]
async fn gateway_pre_admission_capacity_returns_typed_backpressure_without_handler_effects() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut config = GatewayConfig::default();
    config.resource_limits_enabled = true;
    let gw = Gateway::new(session_mgr, DmManager::with_pair_behavior(), config);
    let handler_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = Arc::clone(&handler_calls);
    gw.set_message_handler(Arc::new(move |_| {
        let calls = Arc::clone(&calls);
        Box::pin(async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("must not run".to_string())
        })
    }))
    .await;
    gw.active_ingress_workers
        .store(MAX_INGRESS_WORKERS, std::sync::atomic::Ordering::SeqCst);

    let outcome = gw
        .route_message_from_sender_outcome(
            &test_incoming("capacity"),
            IncomingSender::unknown(),
        )
        .await
        .expect("pre-admission backpressure is a typed route result");

    assert!(matches!(
        outcome,
        GatewayRouteOutcome::Backpressured { retry_after }
            if retry_after == INGRESS_BACKPRESSURE_RETRY_AFTER
    ));
    assert_eq!(
        handler_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "capacity admission must reject before handler/model/tool execution"
    );
    assert_eq!(
        gw.active_ingress_workers
            .load(std::sync::atomic::Ordering::SeqCst),
        MAX_INGRESS_WORKERS,
        "pre-admission rejection must not allocate a worker"
    );
}

#[tokio::test]
async fn gateway_pre_handler_rate_limit_returns_retryable_outcome_and_releases_dedup() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let handler_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = Arc::clone(&handler_calls);
    gw.set_message_handler(Arc::new(move |_| {
        let calls = Arc::clone(&calls);
        Box::pin(async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("must not run before preparation succeeds".to_string())
        })
    }))
    .await;

    let retry_after = std::time::Duration::from_millis(321);
    gw.register_accepted_ingress_preparer(
        "test",
        Arc::new(move |_, _| {
            Box::pin(async move {
                Err(AcceptedIngressPreparationError::retryable(
                    GatewayError::RateLimited {
                        retry_after: Some(retry_after),
                    },
                ))
            })
        }),
    )
    .await;
    let mut incoming = test_incoming("retry before handler");
    incoming.message_id = Some("pre-handler-rate-limit".to_string());

    for _ in 0..2 {
        let outcome = gw
            .route_message_from_sender_outcome(&incoming, IncomingSender::unknown())
            .await
            .expect("pre-handler rate limit is a typed route outcome");
        assert!(matches!(
            outcome,
            GatewayRouteOutcome::Retryable {
                error: GatewayError::RateLimited {
                    retry_after: Some(actual)
                },
                retry_after: Some(outcome_retry_after),
            } if actual == retry_after && outcome_retry_after == retry_after
        ));
    }
    assert_eq!(
        handler_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a pre-handler rate limit must not invoke model/tool work"
    );
}

#[tokio::test]
async fn gateway_send_message_extracts_inline_images() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    gw.send_message(
            "test",
            "chat1",
            "Here ![diagram](https://cdn.example.com/x.png) and <img src=\"https://fal.media/abc\"> done",
            Some(ParseMode::Markdown),
        )
        .await
        .expect("send should succeed");

    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].0, "chat1");
    assert_eq!(sent[0].1, "Here and done");
    assert_eq!(
        sent[1].1,
        "[image] https://cdn.example.com/x.png | caption=diagram"
    );
    assert_eq!(sent[2].1, "[image] https://fal.media/abc");
}

#[tokio::test]
async fn notify_hint_without_thread_keeps_native_image_delivery() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = TestAdapter {
        messages: sent.clone(),
    };

    adapter
        .send_image_url_with_options(
            "chat1",
            "https://assets.example/render?id=1",
            Some("preview"),
            SendMessageOptions::notify_threaded(None),
        )
        .await
        .expect("notify-only hint should preserve native image delivery");

    assert_eq!(
        sent.lock().unwrap().as_slice(),
        &[(
            "chat1".to_string(),
            "[image] https://assets.example/render?id=1 | caption=preview".to_string(),
        )]
    );
}

#[tokio::test]
async fn gateway_threaded_send_preserves_inline_image_target() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ThreadOptionTestAdapter {
        sends: sends.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());
    gw.register_adapter("thread-images", adapter).await;

    gw.send_message_threaded(
        "thread-images",
        "parent-channel",
        "![diagram](https://cdn.example.com/thread.png)",
        Some(ParseMode::Markdown),
        Some("thread-42"),
    )
    .await
    .expect("threaded image send should succeed");

    assert_eq!(
        sends.lock().unwrap().as_slice(),
        &[(
            "parent-channel".to_string(),
            "diagram\nhttps://cdn.example.com/thread.png".to_string(),
            Some("thread-42".to_string()),
            false,
        )]
    );
}

#[tokio::test]
async fn gateway_explicit_send_truncates_long_non_chunking_output_with_audit_label() {
    let tmp = tempfile::tempdir().unwrap();
    let _home_guard = HermesHomeEnvGuard::set(tmp.path());
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    let full_output = "x".repeat(crate::delivery::MAX_PLATFORM_OUTPUT + 1200);

    gw.send_message_explicit_with_audit_label(
        "test",
        "chat1",
        &full_output,
        None,
        None,
        Some("cron/job 42"),
    )
    .await
    .expect("send should succeed");

    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "chat1");
    assert!(sent[0].1.chars().count() <= crate::delivery::MAX_PLATFORM_OUTPUT);
    assert!(sent[0].1.contains("truncated, full output saved to"));
    assert_ne!(sent[0].1, full_output);
    drop(sent);

    let saved: Vec<_> = std::fs::read_dir(tmp.path().join("cron").join("output"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(saved.len(), 1);
    assert!(saved[0]
        .file_name()
        .to_string_lossy()
        .starts_with("cron_job_42_"));
    assert_eq!(
        std::fs::read_to_string(saved[0].path()).unwrap(),
        full_output
    );
}

#[tokio::test]
async fn gateway_send_message_extracts_media_markers_and_delivers_files() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let files = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(MediaMarkerTestAdapter {
        messages: messages.clone(),
        files: files.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());
    gw.register_adapter("media-marker-test", adapter).await;

    let tmp = tempfile::tempdir().unwrap();
    let audio = tmp.path().join("reply.ogg");
    let image = tmp.path().join("browser-shot.png");
    std::fs::write(&audio, b"ogg").unwrap();
    std::fs::write(&image, b"png").unwrap();

    gw.send_message(
        "media-marker-test",
        "chat1",
        &format!(
            "Here is the result.\n[[audio_as_voice]]\nMEDIA:{}\nMEDIA:\"{}\",\nDone",
            audio.display(),
            image.display()
        ),
        Some(ParseMode::Markdown),
    )
    .await
    .expect("send should succeed");

    gw.send_message(
        "media-marker-test",
        "chat1",
        &format!("MEDIA:{}", audio.display()),
        Some(ParseMode::Plain),
    )
    .await
    .expect("plain media delivery should succeed");

    assert_eq!(
        messages.lock().unwrap().as_slice(),
        &[("chat1".to_string(), "Here is the result.\n\nDone".to_string())]
    );
    let files = files.lock().unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].0, "chat1");
    assert_eq!(Path::new(&files[0].1).file_name(), audio.file_name());
    assert_eq!(files[0].2.as_slice(), b"ogg");
    assert!(files[0].3);
    assert_eq!(files[1].0, "chat1");
    assert_eq!(Path::new(&files[1].1).file_name(), image.file_name());
    assert_eq!(files[1].2.as_slice(), b"png");
    assert!(files[1].3);
    assert_eq!(files[2].0, "chat1");
    assert_eq!(Path::new(&files[2].1).file_name(), audio.file_name());
    assert_eq!(files[2].2.as_slice(), b"ogg");
    assert!(!files[2].3);
    assert!(
        files
            .iter()
            .all(|(_, staged_path, _, _)| !Path::new(staged_path).exists()),
        "each staged snapshot is removed after its adapter call"
    );
}

#[tokio::test]
async fn gateway_send_message_blocks_unsafe_media_marker_paths() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let files = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(MediaMarkerTestAdapter {
        messages: messages.clone(),
        files: files.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());
    gw.register_adapter("media-marker-test", adapter).await;

    gw.send_message(
        "media-marker-test",
        "chat1",
        "MEDIA:/etc/passwd",
        Some(ParseMode::Plain),
    )
    .await
    .expect("blocked media marker should not fail entire message send");

    assert!(files.lock().unwrap().is_empty());
    assert_eq!(
        messages.lock().unwrap().as_slice(),
        &[(
            "chat1".to_string(),
            "[media attachment blocked: unsafe local file path]".to_string()
        )]
    );
}

#[tokio::test]
async fn gateway_send_file_validates_and_stages_local_paths() {
    let files = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FileTestAdapter {
        files: files.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());
    gw.register_adapter("file-test", adapter).await;
    let tmp = tempfile::tempdir().unwrap();
    let report = tmp.path().join("report.pdf");
    std::fs::write(&report, b"%PDF-1.4").unwrap();
    let wrapped = format!("'{}'", report.display());

    gw.send_file("file-test", "chat1", &wrapped, Some("caption"))
        .await
        .expect("safe file should be delivered");

    {
        let files = files.lock().unwrap();
        assert_eq!(files.len(), 1);
        let (staged_path, contents) = &files[0];
        assert_eq!(Path::new(staged_path).file_name(), report.file_name());
        assert_eq!(contents.as_slice(), b"%PDF-1.4");
        assert!(
            !Path::new(staged_path).exists(),
            "the staged snapshot is removed after the adapter call"
        );
    }

    let err = gw
        .send_file("file-test", "chat1", "/etc/passwd", None)
        .await
        .expect_err("system file should be rejected");
    assert!(err.to_string().contains("unsafe local file path"));
    assert_eq!(files.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn message_deduplicator_coalesces_inflight_and_retries_failures() {
    let mut deduplicator = MessageDeduplicator::default();
    assert!(matches!(
        deduplicator.claim("delivery-1".to_string()),
        DedupClaimOutcome::Owner
    ));
    let DedupClaimOutcome::InFlight(retry_waiter) = deduplicator.claim("delivery-1".to_string())
    else {
        panic!("concurrent delivery must wait for its owner");
    };
    deduplicator.forget("delivery-1");
    assert!(matches!(retry_waiter.await, Ok(DedupResolution::Retry)));

    assert!(matches!(
        deduplicator.claim("delivery-1".to_string()),
        DedupClaimOutcome::Owner
    ));
    let DedupClaimOutcome::InFlight(completed_waiter) =
        deduplicator.claim("delivery-1".to_string())
    else {
        panic!("concurrent delivery must coalesce");
    };
    deduplicator.complete("delivery-1");
    assert!(matches!(
        completed_waiter.await,
        Ok(DedupResolution::Completed)
    ));
    assert!(matches!(
        deduplicator.claim("delivery-1".to_string()),
        DedupClaimOutcome::Completed
    ));
}

#[test]
fn message_deduplicator_evicts_only_bounded_completed_history() {
    let mut deduplicator = MessageDeduplicator {
        claimed: HashMap::new(),
        completed: HashSet::new(),
        completed_order: VecDeque::new(),
        capacity: 2,
    };
    for key in ["one", "two", "three"] {
        assert!(matches!(
            deduplicator.claim(key.to_string()),
            DedupClaimOutcome::Owner
        ));
        deduplicator.complete(key);
    }

    assert!(matches!(
        deduplicator.claim("one".to_string()),
        DedupClaimOutcome::Owner
    ));
    assert!(matches!(
        deduplicator.claim("two".to_string()),
        DedupClaimOutcome::Completed
    ));
    assert!(matches!(
        deduplicator.claim("three".to_string()),
        DedupClaimOutcome::Completed
    ));
}

fn verified_memory_forget_response() -> GatewayMemoryForgetResponse {
    GatewayMemoryForgetResponse {
        erased: true,
        providers: vec![
            GatewayMemoryForgetProviderStatus {
                provider: "builtin".to_string(),
                state: GatewayMemoryForgetProviderState::VerifiedAbsent,
            },
            GatewayMemoryForgetProviderStatus {
                provider: "remote".to_string(),
                state: GatewayMemoryForgetProviderState::VerifiedAbsent,
            },
        ],
        remediation: None,
    }
}

async fn memory_forget_test_gateway(
    seen: Arc<Mutex<Vec<GatewayRuntimeContext>>>,
    response: GatewayMemoryForgetResponse,
) -> (Gateway, Arc<Mutex<Vec<(String, String)>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_ignore_behavior();
    dm_manager.authorize_user("user1");
    let gateway = Gateway::new(session_manager, dm_manager, GatewayConfig::default());
    gateway.register_adapter("test", adapter).await;
    gateway
        .set_memory_forget_handler(Arc::new(move |context| {
            let seen = seen.clone();
            let response = response.clone();
            Box::pin(async move {
                seen.lock().expect("seen contexts").push(context);
                Ok(response)
            })
        }))
        .await;
    (gateway, sent)
}

#[tokio::test]
async fn authenticated_memory_forget_is_private_to_direct_and_group_subjects() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (gateway, sent) = memory_forget_test_gateway(seen.clone(), verified_memory_forget_response()).await;

    gateway
        .route_message_from_sender(&test_incoming("/memory forget"), IncomingSender::human())
        .await
        .expect("authenticated direct command");
    let mut group = test_incoming("/memory forget");
    group.chat_id = "group-1".to_string();
    group.user_id = "user-2".to_string();
    group.thread_id = Some("thread-7".to_string());
    group.is_dm = false;
    gateway
        .route_message_from_sender(&group, IncomingSender::human())
        .await
        .expect("authenticated group command");

    let contexts = seen.lock().expect("seen contexts");
    assert_eq!(contexts.len(), 2);
    let direct_subject = contexts[0].memory_subject().expect("direct subject");
    let group_subject = contexts[1].memory_subject().expect("group subject");
    assert_eq!(direct_subject.visibility(), hermes_core::MemoryVisibility::Private);
    assert_eq!(group_subject.visibility(), hermes_core::MemoryVisibility::Private);
    assert_ne!(direct_subject, group_subject);
    drop(contexts);
    let replies = sent.lock().expect("gateway replies");
    assert_eq!(replies.len(), 2);
    assert!(replies
        .iter()
        .all(|(_, text)| text.contains("Persistent memory was removed")));
    assert!(replies.iter().all(|(_, text)| {
        text.contains("builtin: verified absent") && text.contains("remote: verified absent")
    }));
}

#[tokio::test]
async fn memory_forget_command_cannot_be_shadowed_by_a_quick_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_ignore_behavior();
    dm_manager.authorize_user("user1");
    let mut config = GatewayConfig::default();
    config.quick_commands.insert(
        "memory".to_string(),
        hermes_config::QuickCommandConfig {
            kind: "alias".to_string(),
            target: Some("/status".to_string()),
            ..Default::default()
        },
    );
    let gateway = Gateway::new(session_manager, dm_manager, config);
    gateway.register_adapter("test", adapter).await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    gateway
        .set_memory_forget_handler({
            let calls = calls.clone();
            Arc::new(move |_context| {
                let calls = calls.clone();
                Box::pin(async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(verified_memory_forget_response())
                })
            })
        })
        .await;

    gateway
        .route_message_from_sender(&test_incoming("/memory forget"), IncomingSender::human())
        .await
        .expect("memory command");

    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let reply = sent.lock().expect("gateway replies").pop().expect("reply").1;
    assert!(reply.contains("Persistent memory was removed"));
    assert!(!reply.contains("Gateway status"));
}

#[tokio::test]
async fn memory_forget_rejects_spoofed_target_args_and_unauthenticated_or_missing_identity() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (gateway, sent) = memory_forget_test_gateway(seen.clone(), verified_memory_forget_response()).await;

    gateway
        .route_message_from_sender(
            &test_incoming("/memory forget --subject=user-2"),
            IncomingSender::human(),
        )
        .await
        .expect("spoofed command is handled");
    gateway
        .route_message(&test_incoming("/memory forget"))
        .await
        .expect("unknown sender is handled");
    let mut missing_user = test_incoming("/memory forget");
    missing_user.user_id.clear();
    missing_user.is_dm = false;
    gateway
        .route_message_from_sender(&missing_user, IncomingSender::human())
        .await
        .expect("missing identity is handled");

    assert!(seen.lock().expect("seen contexts").is_empty());
    let replies = sent.lock().expect("gateway replies");
    assert_eq!(replies.len(), 3);
    assert!(replies[0].1.contains("not accepted"));
    assert!(replies[1]
        .1
        .contains("requires an authenticated human gateway identity"));
    assert!(replies[2]
        .1
        .contains("could not verify the current gateway identity"));
}

#[tokio::test]
async fn incomplete_memory_forget_never_renders_success_and_keeps_provider_error_class() {
    let response = GatewayMemoryForgetResponse {
        erased: true,
        providers: vec![GatewayMemoryForgetProviderStatus {
            provider: "remote".to_string(),
            state: GatewayMemoryForgetProviderState::Incomplete {
                error: Some(hermes_core::MemoryProviderGovernanceError::unsupported(
                    "remote",
                    hermes_core::MemoryGovernanceOperation::Forget,
                )),
                readback_absent: false,
            },
        }],
        remediation: Some(GatewayMemoryForgetRemediation::Reconfigure),
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (gateway, sent) = memory_forget_test_gateway(seen.clone(), response).await;

    gateway
        .route_message_from_sender(&test_incoming("/memory forget"), IncomingSender::human())
        .await
        .expect("incomplete command response");

    assert_eq!(seen.lock().expect("seen contexts").len(), 1);
    let reply = sent.lock().expect("gateway replies").pop().expect("reply").1;
    assert!(!reply.contains("Persistent memory was removed"));
    assert!(!reply.to_ascii_lowercase().contains("erased"));
    assert!(reply.contains("remote: unsupported (forget)"));
    assert!(reply.contains("Restore the configured memory provider"));
    assert!(!reply.contains("user1"));
    assert!(!reply.contains("chat1"));
}

fn verified_memory_export_response() -> GatewayMemoryExportResponse {
    GatewayMemoryExportResponse {
        artifact: Some(GatewayMemoryExportArtifact {
            artifact_ref: hermes_core::MemoryExportArtifactRef::from_opaque(format!(
                "mea1_{}",
                "a".repeat(64)
            ))
            .expect("canonical artifact reference"),
            manifest_digest: hermes_core::MemoryExportEvidenceDigest::sha256_bytes(
                b"committed-memory-export",
            ),
            record_count: 8,
            page_count: 2,
            provider_count: 2,
        }),
        remediation: None,
    }
}

async fn memory_export_test_gateway(
    seen: Arc<Mutex<Vec<GatewayRuntimeContext>>>,
    response: GatewayMemoryExportResponse,
) -> (Gateway, Arc<Mutex<Vec<(String, String)>>>, tempfile::TempDir) {
    let temp = tempfile::tempdir().expect("export tempdir");
    let export_path = temp.path().join("committed-export.jsonl");
    std::fs::write(&export_path, b"{\"record\":\"verified export\"}\n")
        .expect("committed export bytes");
    let expected_artifact = response.artifact.clone();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_ignore_behavior();
    dm_manager.authorize_user("user1");
    let gateway = Gateway::new(session_manager, dm_manager, GatewayConfig::default());
    gateway.register_adapter("test", adapter).await;
    gateway
        .set_memory_export_handler(Arc::new(move |context| {
            let seen = seen.clone();
            let response = response.clone();
            Box::pin(async move {
                seen.lock().expect("seen contexts").push(context);
                Ok(response)
            })
        }))
        .await;
    gateway
        .set_memory_export_open_handler(Arc::new(move |_context, artifact| {
            let expected_artifact = expected_artifact.clone();
            let export_path = export_path.clone();
            Box::pin(async move {
                if expected_artifact.as_ref() != Some(&artifact) {
                    return Err(GatewayError::Auth(
                        "memory export download is unavailable".to_string(),
                    ));
                }
                std::fs::File::open(export_path).map_err(|_| {
                    GatewayError::Auth("memory export download is unavailable".to_string())
                })
            })
        }))
        .await;
    (gateway, sent, temp)
}

fn one_time_download_ref(reply: &str) -> String {
    reply
        .split("One-time download reference: `")
        .nth(1)
        .and_then(|tail| tail.split('`').next())
        .map(str::to_string)
        .expect("one-time download reference")
}

#[tokio::test]
async fn authenticated_memory_export_is_subject_bound_single_use_and_content_free() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (gateway, sent, _temp) =
        memory_export_test_gateway(seen.clone(), verified_memory_export_response()).await;

    gateway
        .route_message_from_sender(&test_incoming("/memory export"), IncomingSender::human())
        .await
        .expect("authenticated direct export");
    let mut group = test_incoming("/memory export");
    group.chat_id = "group-1".to_string();
    group.user_id = "user-2".to_string();
    group.thread_id = Some("thread-7".to_string());
    group.is_dm = false;
    gateway
        .route_message_from_sender(&group, IncomingSender::human())
        .await
        .expect("authenticated group export");

    let contexts = seen.lock().expect("seen contexts").clone();
    assert_eq!(contexts.len(), 2);
    assert_ne!(
        contexts[0].memory_subject().expect("direct subject"),
        contexts[1].memory_subject().expect("group subject")
    );
    let replies = sent.lock().expect("gateway replies");
    assert_eq!(replies.len(), 2);
    assert!(replies.iter().all(|(_, reply)| {
        reply.contains("Persistent-memory export is ready")
            && reply.contains("Manifest digest: `sha256:")
            && reply.contains("Records: 8")
            && reply.contains("Pages: 2")
            && reply.contains("Providers: 2")
            && !reply.contains("mea1_")
            && !reply.contains("user1")
            && !reply.contains("group-1")
    }));
    let direct_download_ref = one_time_download_ref(&replies[0].1);
    let group_download_ref = one_time_download_ref(&replies[1].1);
    assert_ne!(direct_download_ref, group_download_ref);
    drop(replies);

    assert!(gateway
        .redeem_memory_export_download(&contexts[1], &direct_download_ref)
        .await
        .is_err());
    let tampered_download_ref = format!("{direct_download_ref}x");
    assert!(gateway
        .redeem_memory_export_download(&contexts[0], &tampered_download_ref)
        .await
        .is_err());
    let mut file = gateway
        .redeem_memory_export_download(&contexts[0], &direct_download_ref)
        .await
        .expect("bound one-time verified download");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).expect("read verified export descriptor");
    assert_eq!(bytes, b"{\"record\":\"verified export\"}\n");
    assert!(gateway
        .redeem_memory_export_download(&contexts[0], &direct_download_ref)
        .await
        .is_err());
}

#[tokio::test]
async fn memory_export_rejects_spoofed_args_and_unverified_identity_before_handler() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (gateway, sent, _temp) =
        memory_export_test_gateway(seen.clone(), verified_memory_export_response()).await;

    gateway
        .route_message_from_sender(
            &test_incoming("/memory export --subject=user-2"),
            IncomingSender::human(),
        )
        .await
        .expect("spoofed export is handled");
    gateway
        .route_message(&test_incoming("/memory export"))
        .await
        .expect("unknown sender is handled");
    let mut missing_user = test_incoming("/memory export");
    missing_user.user_id.clear();
    missing_user.is_dm = false;
    gateway
        .route_message_from_sender(&missing_user, IncomingSender::human())
        .await
        .expect("missing identity is handled");

    assert!(seen.lock().expect("seen contexts").is_empty());
    let replies = sent.lock().expect("gateway replies");
    assert_eq!(replies.len(), 3);
    assert!(replies[0].1.contains("not accepted"));
    assert!(replies[1]
        .1
        .contains("requires an authenticated human gateway identity"));
    assert!(replies[2]
        .1
        .contains("could not verify the current gateway identity"));
}

#[tokio::test]
async fn incomplete_memory_export_never_issues_or_renders_a_download_reference() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut response = verified_memory_export_response();
    response.remediation = Some(GatewayMemoryExportRemediation::Reconfigure);
    let (gateway, sent, _temp) = memory_export_test_gateway(seen.clone(), response).await;

    gateway
        .route_message_from_sender(&test_incoming("/memory export"), IncomingSender::human())
        .await
        .expect("incomplete export response");

    assert_eq!(seen.lock().expect("seen contexts").len(), 1);
    let reply = sent.lock().expect("gateway replies").pop().expect("reply").1;
    assert!(reply.contains("Persistent-memory export was not completed"));
    assert!(reply.contains("Restore the configured memory provider"));
    assert!(!reply.contains("One-time download reference"));
    assert!(!reply.contains("Manifest digest"));
    assert!(!reply.contains("mea1_"));
}

include!("tests/status_profiles.rs");

mod hook_lifecycle;
mod queue_dedup;
mod runtime_controls;
