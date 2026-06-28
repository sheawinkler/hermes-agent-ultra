use super::*;
use crate::hooks::{HookEvent, HookHandler, HookRegistry};
use crate::session::SessionManager;
use async_trait::async_trait;
use hermes_config::session::SessionConfig;
use std::sync::{Mutex, MutexGuard, OnceLock};

struct TestAdapter {
    messages: Arc<Mutex<Vec<(String, String)>>>,
}

struct StatusUpdateAdapter {
    updates: Arc<Mutex<Vec<(String, String, String)>>>,
}

struct ReactionTestAdapter {
    messages: Arc<Mutex<Vec<(String, String)>>>,
    reactions: Arc<Mutex<Vec<String>>>,
}

struct FileTestAdapter {
    files: Arc<Mutex<Vec<String>>>,
}

struct MediaMarkerTestAdapter {
    messages: Arc<Mutex<Vec<(String, String)>>>,
    files: Arc<Mutex<Vec<String>>>,
}

type ThreadSendRecord = (String, String, Option<String>, bool);

struct ThreadOptionTestAdapter {
    sends: Arc<Mutex<Vec<ThreadSendRecord>>>,
}

struct RecordingHook {
    seen: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
}

struct FailingHook;

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
        self.files.lock().unwrap().push(file_path.to_string());
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
        self.files
            .lock()
            .unwrap()
            .push(format!("{chat_id}:{file_path}"));
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "media-marker-test"
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

#[test]
fn gateway_config_default() {
    let cfg = GatewayConfig::default();
    assert!(cfg.ssrf_protection);
    assert!(cfg.media_cache_dir.is_none());
    assert_eq!(cfg.media_cache_max_bytes, 0);
    assert!(!cfg.streaming_enabled);
}

#[tokio::test]
async fn gateway_register_and_list_adapters() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());

    assert!(gw.adapter_names().await.is_empty());
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

    assert_eq!(
        messages.lock().unwrap().as_slice(),
        &[("chat1".to_string(), "Here is the result. Done".to_string())]
    );
    assert_eq!(
        files.lock().unwrap().as_slice(),
        &[
            format!("chat1:{}", std::fs::canonicalize(&audio).unwrap().display()),
            format!("chat1:{}", std::fs::canonicalize(&image).unwrap().display()),
        ]
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
async fn gateway_send_file_validates_and_canonicalizes_local_paths() {
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

    assert_eq!(
        files.lock().unwrap().as_slice(),
        &[std::fs::canonicalize(&report)
            .unwrap()
            .to_string_lossy()
            .to_string()]
    );

    let err = gw
        .send_file("file-test", "chat1", "/etc/passwd", None)
        .await
        .expect_err("system file should be rejected");
    assert!(err.to_string().contains("unsafe local file path"));
    assert_eq!(files.lock().unwrap().len(), 1);
}

include!("tests/status_profiles.rs");

mod hook_lifecycle;
mod runtime_controls;
