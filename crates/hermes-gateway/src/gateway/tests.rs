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

#[tokio::test]
async fn gateway_status_updates_use_platform_status_api() {
    let updates = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(StatusUpdateAdapter {
        updates: updates.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());
    gw.register_adapter("status-test", adapter).await;

    gw.send_or_update_status(
        "status-test",
        "chat1",
        "context_pressure",
        "compressing",
        None,
    )
    .await
    .expect("first status update should succeed");
    gw.send_or_update_status("status-test", "chat1", "context_pressure", "done", None)
        .await
        .expect("second status update should succeed");

    let updates = updates.lock().unwrap();
    assert_eq!(
        *updates,
        vec![
            (
                "chat1".to_string(),
                "context_pressure".to_string(),
                "compressing".to_string()
            ),
            (
                "chat1".to_string(),
                "context_pressure".to_string(),
                "done".to_string()
            )
        ]
    );
}

#[tokio::test]
async fn gateway_route_dm_denied() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "unknown_user".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    // Should succeed (deny silently)
    let result = gw.route_message(&incoming).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn gateway_route_no_handler() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    // Should fail because no message handler is set
    let result = gw.route_message(&incoming).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn gateway_route_group_message_skips_dm_check() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "-group1".into(),
        user_id: "unknown_user".into(),
        text: "hello group".into(),
        message_id: None,
        thread_id: None,
        is_dm: false, // Group message, no DM check
    };

    // Should fail because no handler, but DM check is skipped
    let result = gw.route_message(&incoming).await;
    assert!(result.is_err()); // No handler configured
}

#[tokio::test]
async fn gateway_group_allowlist_denies_unauthorized_user() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("allowed_user".to_string());
    policies.insert("telegram".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let incoming = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-100123".into(),
        user_id: "other_user".into(),
        text: "hello group".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };

    let result = gw.route_message(&incoming).await;
    assert!(result.is_ok());
    assert_eq!(
        gw.session_transcript_len("telegram", "-100123", "other_user")
            .await,
        0
    );
}

#[tokio::test]
async fn gateway_group_allowlist_star_authorizes_any_sender() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("*".to_string());
    policies.insert("telegram".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let incoming = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-100123".into(),
        user_id: "any_user".into(),
        text: "hello group".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };

    let result = gw.route_message(&incoming).await;
    assert!(result.is_err());
    assert_eq!(
        gw.session_transcript_len("telegram", "-100123", "any_user")
            .await,
        1
    );
}

#[tokio::test]
async fn gateway_group_chat_authorization_allows_listed_chat_sender() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("999".to_string());
    policy
        .authorized_group_chats
        .insert("-1001878443972".to_string());
    policies.insert("telegram".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let legacy_chat_source = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-1001878443972".into(),
        user_id: "123".into(),
        text: "hello group".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&legacy_chat_source).await.is_err());
    assert_eq!(
        gw.session_transcript_len("telegram", "-1001878443972", "123")
            .await,
        1
    );

    let sender_source = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-1009999999999".into(),
        user_id: "999".into(),
        text: "hello group".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&sender_source).await.is_err());
    assert_eq!(
        gw.session_transcript_len("telegram", "-1009999999999", "999")
            .await,
        1
    );
}

#[tokio::test]
async fn gateway_route_unauthorized_dm_pairs_with_code_message() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("whatsapp", adapter).await;

    let incoming = IncomingMessage {
        platform: "whatsapp".into(),
        chat_id: "15551234567@s.whatsapp.net".into(),
        user_id: "15551234567@s.whatsapp.net".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    assert!(gw.route_message(&incoming).await.is_ok());
    let messages = sent.lock().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].1.contains("pairing code"));
    assert_eq!(
        gw.session_transcript_len(
            "whatsapp",
            "15551234567@s.whatsapp.net",
            "15551234567@s.whatsapp.net"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn gateway_route_rate_limited_dm_sends_no_response() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    dm_manager.record_pairing_rate_limit("whatsapp", "15551234567@s.whatsapp.net");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("whatsapp", adapter).await;

    let incoming = IncomingMessage {
        platform: "whatsapp".into(),
        chat_id: "15551234567@s.whatsapp.net".into(),
        user_id: "15551234567@s.whatsapp.net".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    assert!(gw.route_message(&incoming).await.is_ok());
    assert!(sent.lock().unwrap().is_empty());
    assert_eq!(
        gw.session_transcript_len(
            "whatsapp",
            "15551234567@s.whatsapp.net",
            "15551234567@s.whatsapp.net"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn gateway_channel_allow_and_ignore_policy_matches_discord_contract() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("discord", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("handled".to_string()) })
    }))
    .await;

    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Open,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_channels.insert("allowed".to_string());
    policy.ignored_channels.insert("ignored".to_string());
    policies.insert("discord".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let ignored = IncomingMessage {
        platform: "discord".into(),
        chat_id: "ignored".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&ignored).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "ignored", "user1")
            .await,
        0
    );

    let not_allowed = IncomingMessage {
        platform: "discord".into(),
        chat_id: "other".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&not_allowed).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "other", "user1").await,
        0
    );

    let allowed = IncomingMessage {
        platform: "discord".into(),
        chat_id: "allowed".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&allowed).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "allowed", "user1")
            .await,
        2
    );
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn gateway_allowed_channel_policy_blocks_mentions_but_not_dms() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_ignore_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("handled".to_string()) })
    }))
    .await;

    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Open,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_channels.insert("-100allowed".to_string());
    policies.insert("telegram".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let mentioned_blocked_group = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-100blocked".into(),
        user_id: "user1".into(),
        text: "@hermes_bot hello".into(),
        message_id: Some("m1".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&mentioned_blocked_group).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("telegram", "-100blocked", "user1")
            .await,
        0
    );

    let dm = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-100blocked".into(),
        user_id: "user1".into(),
        text: "dm hello".into(),
        message_id: Some("m2".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&dm).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("telegram", "-100blocked", "user1")
            .await,
        2
    );
}

#[tokio::test]
async fn gateway_discord_slash_requires_allowlist() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("discord", adapter).await;

    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Open,
        slash_requires_allowlist: true,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("allowed_user".to_string());
    policies.insert("discord".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let denied = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild:1".into(),
        user_id: "random_user".into(),
        text: "/status".into(),
        message_id: Some("m1".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&denied).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "guild:1", "random_user")
            .await,
        0
    );

    let allowed = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild:1".into(),
        user_id: "allowed_user".into(),
        text: "/status".into(),
        message_id: Some("m2".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&allowed).await.is_ok());
    let sent_msgs = sent.lock().unwrap();
    assert_eq!(sent_msgs.len(), 1);
    assert_eq!(sent_msgs[0].0, "guild:1");
    assert!(!sent_msgs[0].1.trim().is_empty());
}

#[tokio::test]
async fn gateway_discord_bot_sender_can_bypass_user_allowlist_when_enabled() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        bot_sender_bypasses_allowlist: true,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("human_user".to_string());
    policies.insert("discord".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild:1".into(),
        user_id: "worker_bot".into(),
        text: "notion event".into(),
        message_id: Some("m1".into()),
        thread_id: None,
        is_dm: false,
    };

    assert!(gw
        .route_message_from_sender(&incoming, IncomingSender::bot())
        .await
        .is_err());
    assert_eq!(
        gw.session_transcript_len("discord", "guild:1", "worker_bot")
            .await,
        1
    );
}

#[tokio::test]
async fn gateway_discord_bot_sender_still_rejected_when_bypass_disabled() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        bot_sender_bypasses_allowlist: false,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("human_user".to_string());
    policies.insert("discord".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild:1".into(),
        user_id: "worker_bot".into(),
        text: "notion event".into(),
        message_id: Some("m1".into()),
        thread_id: None,
        is_dm: false,
    };

    assert!(gw
        .route_message_from_sender(&incoming, IncomingSender::bot())
        .await
        .is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "guild:1", "worker_bot")
            .await,
        0
    );
}

#[tokio::test]
async fn gateway_discord_bot_bypass_does_not_apply_to_humans_or_other_platforms() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut discord_policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        bot_sender_bypasses_allowlist: true,
        ..PlatformAccessPolicy::default()
    };
    discord_policy
        .allowed_users
        .insert("human_user".to_string());
    policies.insert("discord".to_string(), discord_policy);
    let mut telegram_policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        bot_sender_bypasses_allowlist: true,
        ..PlatformAccessPolicy::default()
    };
    telegram_policy
        .allowed_users
        .insert("human_user".to_string());
    policies.insert("telegram".to_string(), telegram_policy);
    gw.set_platform_access_policies(policies).await;

    let discord_human = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild:1".into(),
        user_id: "other_human".into(),
        text: "hello".into(),
        message_id: Some("m1".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw
        .route_message_from_sender(&discord_human, IncomingSender::human())
        .await
        .is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "guild:1", "other_human")
            .await,
        0
    );

    let telegram_bot = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-100123".into(),
        user_id: "worker_bot".into(),
        text: "hello".into(),
        message_id: Some("m2".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw
        .route_message_from_sender(&telegram_bot, IncomingSender::bot())
        .await
        .is_ok());
    assert_eq!(
        gw.session_transcript_len("telegram", "-100123", "worker_bot")
            .await,
        0
    );
}

#[tokio::test]
async fn gateway_executes_status_command_without_agent_handler() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/status".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    let result = gw.route_message(&incoming).await;
    assert!(result.is_ok());

    let msgs = sent.lock().unwrap();
    assert!(msgs.iter().any(|(_, text)| text.contains("Gateway status")));
}

#[tokio::test]
async fn gateway_compress_command_appends_warning_when_summary_unavailable() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    let _ = gw
        .session_manager
        .get_or_create_session("test", "chat1", "user1")
        .await;
    gw.session_manager
        .add_message(&session_key, Message::system("sys"))
        .await;
    for _ in 0..40 {
        gw.session_manager
            .add_message(
                &session_key,
                Message {
                    role: MessageRole::Tool,
                    content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                    anthropic_content_blocks: None,
                    cache_control: None,
                },
            )
            .await;
    }

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/compress".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    assert!(gw.route_message(&incoming).await.is_ok());

    let msgs = sent.lock().unwrap();
    let reply = msgs.last().map(|(_, t)| t.clone()).unwrap_or_default();
    assert!(reply.contains("Context compressed"));
    assert!(reply.contains("⚠️ Context compression summary failed"));
    assert!(reply.contains("historical message(s) were removed"));
}

#[tokio::test]
async fn gateway_compress_command_emits_summary_without_warning() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    let _ = gw
        .session_manager
        .get_or_create_session("test", "chat1", "user1")
        .await;
    gw.session_manager
        .add_message(&session_key, Message::system("sys"))
        .await;
    for i in 0..40 {
        let message = if i % 2 == 0 {
            Message::user(format!("turn {i} content"))
        } else {
            Message::assistant(format!("turn {i} content"))
        };
        gw.session_manager.add_message(&session_key, message).await;
    }

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/compress".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    assert!(gw.route_message(&incoming).await.is_ok());

    let msgs = sent.lock().unwrap();
    let reply = msgs.last().map(|(_, t)| t.clone()).unwrap_or_default();
    assert!(reply.contains("Context compressed"));
    assert!(!reply.contains("⚠️"));
    drop(msgs);

    let updated = gw.session_manager.get_messages(&session_key).await;
    assert!(
        updated.iter().any(|m| {
            m.content
                .as_deref()
                .unwrap_or("")
                .contains("[CONTEXT COMPACTION] Earlier conversation was compacted")
        }),
        "summary marker should be persisted into compressed transcript"
    );
}

#[tokio::test]
async fn gateway_usage_text_includes_last_nous_credits_state() {
    hermes_core::credits::clear_last_nous_credits_state();
    hermes_core::credits::capture_nous_credits_from_pairs([
        ("x-nous-credits-version", "1"),
        ("x-nous-credits-remaining-micros", "12000000"),
        ("x-nous-credits-remaining-usd", "12.00"),
        ("x-nous-credits-subscription-micros", "5000000"),
        ("x-nous-credits-subscription-usd", "5.00"),
        ("x-nous-credits-subscription-limit-micros", "10000000"),
        ("x-nous-credits-subscription-limit-usd", "10.00"),
        ("x-nous-credits-rollover-micros", "0"),
        ("x-nous-credits-purchased-micros", "7000000"),
        ("x-nous-credits-purchased-usd", "7.00"),
        ("x-nous-credits-denominator-kind", "subscription_cap"),
        ("x-nous-credits-paid-access", "true"),
    ])
    .expect("capture credits");

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let text = gw.build_usage_text("test:chat:user").await;

    assert!(text.contains("Usage"));
    assert!(text.contains("Nous credits"));
    assert!(text.contains("Subscription: 50% remaining (50% used)"));
    hermes_core::credits::clear_last_nous_credits_state();
}

#[tokio::test]
async fn gateway_background_task_lifecycle_commands_work() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|messages| {
        Box::pin(async move {
            let prompt = messages
                .last()
                .and_then(|m| m.content.clone())
                .unwrap_or_else(|| "none".to_string());
            Ok(format!("done: {}", prompt))
        })
    }))
    .await;

    let start = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/background ping".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&start).await.is_ok());

    let task_id = {
        let msgs = sent.lock().unwrap();
        let queued = msgs
            .iter()
            .find(|(_, text)| text.contains("Background task started"))
            .expect("queue ack should exist");
        queued
            .1
            .lines()
            .find_map(|line| line.strip_prefix("Task ID: ").map(str::trim))
            .expect("task id line")
            .to_string()
    };

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let status = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: format!("/background status {}", task_id),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&status).await.is_ok());

    let msgs = sent.lock().unwrap();
    assert!(msgs.iter().any(|(_, text)| text.contains("completed")));
}

#[tokio::test]
async fn gateway_admin_approve_and_deny_affects_dm_authorization() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_ignore_behavior();
    dm_manager.add_admin("admin1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let approve = IncomingMessage {
        platform: "test".into(),
        chat_id: "admin-chat".into(),
        user_id: "admin1".into(),
        text: "/approve user2".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&approve).await.is_ok());

    // user2 should now pass DM authorization, then fail because no handler is configured.
    let authorized_dm = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-u2".into(),
        user_id: "user2".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&authorized_dm).await.is_err());

    let deny = IncomingMessage {
        platform: "test".into(),
        chat_id: "admin-chat".into(),
        user_id: "admin1".into(),
        text: "/deny user2".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&deny).await.is_ok());

    // user2 should be denied again, and route should return Ok (silently denied).
    let denied_dm = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-u2".into(),
        user_id: "user2".into(),
        text: "hello again".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&denied_dm).await.is_ok());
}

#[tokio::test]
async fn gateway_reload_mcp_and_status_reflect_runtime_state() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let provider = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/provider openrouter".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&provider).await.is_ok());

    let profile = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/profile prod".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&profile).await.is_ok());

    let reload = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/reload_mcp".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reload).await.is_ok());

    let status = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/status".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&status).await.is_ok());

    let msgs = sent.lock().unwrap();
    let status_text = msgs
        .iter()
        .rev()
        .find_map(|(_, text)| {
            if text.contains("Gateway status") {
                Some(text.clone())
            } else {
                None
            }
        })
        .expect("status response should exist");
    assert!(status_text.contains("provider: openrouter"));
    assert!(status_text.contains("profile: prod"));
    assert!(status_text.contains("mcp generation: 1"));
}

#[tokio::test]
async fn gateway_title_command_persists_and_surfaces_session_title() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let hooks = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr.clone(), dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    let mut registry = HookRegistry::new();
    registry.register_in_process(
        "session:title",
        Arc::new(RecordingHook {
            seen: hooks.clone(),
        }),
    );
    gw.set_hook_registry(Arc::new(registry)).await;

    let title = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-title".into(),
        user_id: "user1".into(),
        text: "/title Release readiness".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&title).await.is_ok());
    let session_key = session_mgr.compose_session_key("test", "chat-title", "user1");
    assert_eq!(
        session_mgr.get_title(&session_key).await.as_deref(),
        Some("Release readiness")
    );

    let show_title = IncomingMessage {
        text: "/title".into(),
        ..title.clone()
    };
    assert!(gw.route_message(&show_title).await.is_ok());

    let status = IncomingMessage {
        text: "/status".into(),
        ..title.clone()
    };
    assert!(gw.route_message(&status).await.is_ok());

    let sessions = IncomingMessage {
        text: "/sessions".into(),
        ..title
    };
    assert!(gw.route_message(&sessions).await.is_ok());

    let msgs = sent.lock().unwrap();
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("Session title set to: Release readiness")));
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("Current session title: Release readiness")));
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("- title: Release readiness")));
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("title `Release readiness`")));
    drop(msgs);

    let hooks = hooks.lock().unwrap();
    let title_event = hooks
        .iter()
        .find(|(name, _)| name == "session:title")
        .expect("title hook emitted");
    assert_eq!(title_event.1["session_id"], serde_json::json!(session_key));
    assert_eq!(
        title_event.1["title"],
        serde_json::json!("Release readiness")
    );
}

#[tokio::test]
async fn gateway_profile_command_applies_profile_yaml_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = HermesHomeEnvGuard::set(tmp.path());
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    let profile_home = tmp.path().join("profile-home");
    std::fs::create_dir_all(&profile_home).unwrap();
    std::fs::write(
        profiles_dir.join("prod.yaml"),
        format!(
            "name: prod\nmodel: openrouter:qwen/qwen3-coder\npersonality: strict\nhome_dir: {}\n",
            profile_home.display()
        ),
    )
    .unwrap();
    std::fs::write(profiles_dir.join("aliases.json"), r#"{"work":"prod"}"#).unwrap();

    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let profile = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/profile work".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&profile).await.is_ok());

    let status = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/status".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&status).await.is_ok());

    let msgs = sent.lock().unwrap();
    let profile_reply = msgs
        .iter()
        .find_map(|(_, text)| text.contains("Profile switched").then_some(text.clone()))
        .expect("profile reply should exist");
    assert!(profile_reply.contains("prod"));
    assert!(profile_reply.contains("requested 'work'"));
    assert!(profile_reply.contains("model=openrouter:qwen/qwen3-coder"));
    assert!(profile_reply.contains("provider=openrouter"));
    assert!(profile_reply.contains("personality=strict"));

    let status_text = msgs
        .iter()
        .rev()
        .find_map(|(_, text)| text.contains("Gateway status").then_some(text.clone()))
        .expect("status response should exist");
    assert!(status_text.contains("model: openrouter:qwen/qwen3-coder"));
    assert!(status_text.contains("provider: openrouter"));
    assert!(status_text.contains("profile: prod"));
    assert!(status_text.contains("personality: strict"));
    assert!(status_text.contains(&format!("home: {}", profile_home.display())));
}

#[tokio::test]
async fn gateway_profile_command_missing_file_preserves_label_with_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = HermesHomeEnvGuard::set(tmp.path());

    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let profile = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/profile scratch".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&profile).await.is_ok());

    let status = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/status".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&status).await.is_ok());

    let msgs = sent.lock().unwrap();
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("Profile file not applied")));
    let status_text = msgs
        .iter()
        .rev()
        .find_map(|(_, text)| text.contains("Gateway status").then_some(text.clone()))
        .expect("status response should exist");
    assert!(status_text.contains("profile: scratch"));
}

#[tokio::test]
async fn gateway_runtime_state_is_injected_into_agent_messages() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|messages| {
        Box::pin(async move {
            let hint = messages
                .iter()
                .find(|m| {
                    m.role == MessageRole::System
                        && m.content
                            .as_deref()
                            .unwrap_or("")
                            .contains("[gateway_runtime]")
                })
                .and_then(|m| m.content.clone())
                .unwrap_or_else(|| "no-runtime-hints".to_string());
            Ok(hint)
        })
    }))
    .await;

    let configured_model = format!("dynamic-runtime-model-{}", std::process::id());
    let set_provider = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/provider openai".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&set_provider).await.is_ok());

    let set_model = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: format!("/model {configured_model}"),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&set_model).await.is_ok());

    let set_profile = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/profile prod".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&set_profile).await.is_ok());

    let set_branch = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/branch feature/parity".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&set_branch).await.is_ok());

    let set_fast = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/fast".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&set_fast).await.is_ok());

    let normal = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&normal).await.is_ok());

    let msgs = sent.lock().unwrap();
    let echoed = msgs
        .iter()
        .rev()
        .find_map(|(_, text)| {
            if text.contains("[gateway_runtime]") {
                Some(text.clone())
            } else {
                None
            }
        })
        .expect("runtime hint response should exist");

    assert!(echoed.contains(&format!("model={configured_model}")));
    assert!(!echoed.contains("gpt-4o"));
    assert!(echoed.contains("provider=openai"));
    assert!(echoed.contains("profile=prod"));
    assert!(echoed.contains("branch=feature/parity"));
    assert!(echoed.contains("service_tier=priority"));
}

#[tokio::test]
async fn gateway_model_switch_persists_default_and_applies_to_new_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "model: nous:nousresearch/hermes-4-70b\nmodel_switch:\n  persist_switch_by_default: true\n",
    )
    .unwrap();

    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    dm_manager.authorize_user("user2");
    let cfg = GatewayConfig {
        model: Some("nous:nousresearch/hermes-4-70b".to_string()),
        model_switch_config_path: Some(config_path.to_string_lossy().to_string()),
        ..GatewayConfig::default()
    };
    let gw = Gateway::new(session_mgr, dm_manager, cfg);
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler_with_context(Arc::new(|_messages, ctx| {
        Box::pin(async move { Ok(format!("ctx model={:?}", ctx.model)) })
    }))
    .await;

    let switch = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/model openrouter:zai/glm-5.2".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&switch).await.is_ok());
    let disk = hermes_config::load_user_config_file(&config_path).unwrap();
    assert_eq!(disk.model.as_deref(), Some("openrouter:zai/glm-5.2"));

    let new_session_message = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat2".into(),
        user_id: "user2".into(),
        text: "hello from a fresh gateway session".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&new_session_message).await.is_ok());

    let msgs = sent.lock().unwrap();
    assert!(msgs.iter().any(|(_, text)| text.contains("Saved to")));
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("ctx model=Some(\"openrouter:zai/glm-5.2\")")));
}

#[tokio::test]
async fn gateway_model_switch_session_scope_does_not_persist_and_warns_on_large_context() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "model: nous:nousresearch/hermes-4-70b\nmodel_switch:\n  persist_switch_by_default: true\n",
    )
    .unwrap();

    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let session_key = session_mgr.compose_session_key("test", "chat1", "user1");
    session_mgr
        .get_or_create_session("test", "chat1", "user1")
        .await;
    session_mgr
        .add_message(&session_key, Message::user("large-context ".repeat(40_000)))
        .await;
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let cfg = GatewayConfig {
        model: Some("nous:nousresearch/hermes-4-70b".to_string()),
        model_switch_config_path: Some(config_path.to_string_lossy().to_string()),
        ..GatewayConfig::default()
    };
    let gw = Gateway::new(session_mgr, dm_manager, cfg);
    gw.register_adapter("test", adapter).await;

    let switch = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/model compact-runtime-model --global --session".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&switch).await.is_ok());

    let disk = hermes_config::load_user_config_file(&config_path).unwrap();
    assert_eq!(
        disk.model.as_deref(),
        Some("nous:nousresearch/hermes-4-70b")
    );
    let msgs = sent.lock().unwrap();
    let reply = msgs
        .iter()
        .find_map(|(_, text)| {
            text.contains("compact-runtime-model")
                .then_some(text.clone())
        })
        .expect("model switch reply should exist");
    assert!(reply.contains("Session only"));
    assert!(reply.contains("Context warning"));
    assert!(reply.contains("preflight compression"));
}

#[tokio::test]
async fn gateway_verbose_command_is_config_gated_and_cycles_tool_progress() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter.clone()).await;

    let verbose = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "chat-verbose".into(),
        user_id: "user1".into(),
        text: "/verbose".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&verbose).await.is_ok());
    assert!(sent
        .lock()
        .unwrap()
        .last()
        .expect("disabled reply")
        .1
        .contains("tool_progress_command"));

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.tool_progress_command = true;
    cfg.display.tool_progress = Some("all".to_string());
    let gw = Gateway::new(session_mgr, dm_manager, cfg);
    gw.register_adapter("telegram", adapter.clone()).await;

    assert!(gw.route_message(&verbose).await.is_ok());
    let first = sent
        .lock()
        .unwrap()
        .last()
        .expect("verbose reply")
        .1
        .clone();
    assert!(first.contains("telegram"));
    assert!(first.contains("VERBOSE"));

    let session_key = gw
        .session_manager
        .compose_session_key("telegram", "chat-verbose", "user1");
    let states = gw.runtime_state.read().await;
    let state = states.get(&session_key).expect("runtime state");
    assert_eq!(state.tool_progress.as_deref(), Some("verbose"));
    assert!(state.verbose);
    drop(states);

    assert!(gw.route_message(&verbose).await.is_ok());
    let second = sent.lock().unwrap().last().expect("off reply").1.clone();
    assert!(second.contains("OFF"));
}

#[tokio::test]
async fn gateway_new_clears_yolo_only_for_target_session() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key_1 = gw
        .session_manager
        .compose_session_key("test", "chat-yolo-new-1", "user1");
    let session_key_2 = gw
        .session_manager
        .compose_session_key("test", "chat-yolo-new-2", "user1");
    hermes_tools::approval::clear_session(&session_key_1);
    hermes_tools::approval::clear_session(&session_key_2);
    hermes_tools::approval::approve_session(&session_key_1, "recursive delete");
    hermes_tools::approval::approve_session(&session_key_2, "recursive delete");

    let yolo_chat1 = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-yolo-new-1".into(),
        user_id: "user1".into(),
        text: "/yolo".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&yolo_chat1).await.is_ok());

    let yolo_chat2 = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-yolo-new-2".into(),
        user_id: "user1".into(),
        text: "/yolo".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&yolo_chat2).await.is_ok());

    {
        let states = gw.runtime_state.read().await;
        assert_eq!(states.get(&session_key_1).map(|s| s.yolo), Some(true));
        assert_eq!(states.get(&session_key_2).map(|s| s.yolo), Some(true));
    }
    assert!(hermes_tools::approval::is_session_yolo_enabled(
        &session_key_1
    ));
    assert!(hermes_tools::approval::is_session_yolo_enabled(
        &session_key_2
    ));
    assert!(hermes_tools::approval::is_approved(
        &session_key_1,
        "recursive delete"
    ));
    assert!(hermes_tools::approval::is_approved(
        &session_key_2,
        "recursive delete"
    ));

    let reset_chat1 = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-yolo-new-1".into(),
        user_id: "user1".into(),
        text: "/new".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset_chat1).await.is_ok());

    let states = gw.runtime_state.read().await;
    assert_eq!(states.get(&session_key_1).map(|s| s.yolo), Some(false));
    assert_eq!(states.get(&session_key_2).map(|s| s.yolo), Some(true));
    assert!(!hermes_tools::approval::is_session_yolo_enabled(
        &session_key_1
    ));
    assert!(hermes_tools::approval::is_session_yolo_enabled(
        &session_key_2
    ));
    assert!(!hermes_tools::approval::is_approved(
        &session_key_1,
        "recursive delete"
    ));
    assert!(hermes_tools::approval::is_approved(
        &session_key_2,
        "recursive delete"
    ));
    hermes_tools::approval::clear_session(&session_key_2);
}

#[tokio::test]
async fn telegram_topic_chat_ids_are_independent_session_lanes() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr.clone(), dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter).await;
    gw.set_message_handler(Arc::new(|messages| {
        Box::pin(async move {
            let user_turns = messages
                .iter()
                .filter(|message| message.role == MessageRole::User)
                .count();
            Ok(format!("topic-turns={user_turns}"))
        })
    }))
    .await;

    for (chat_id, text) in [
        ("208214988:111", "topic a first"),
        ("208214988:222", "topic b first"),
        ("208214988:111", "topic a second"),
    ] {
        gw.route_message(&IncomingMessage {
            platform: "telegram".into(),
            chat_id: chat_id.into(),
            user_id: "user1".into(),
            text: text.into(),
            message_id: None,
            thread_id: None,
            is_dm: true,
        })
        .await
        .expect("route telegram topic message");
    }

    let topic_a_key = session_mgr.compose_session_key("telegram", "208214988:111", "user1");
    let topic_b_key = session_mgr.compose_session_key("telegram", "208214988:222", "user1");
    let topic_a = session_mgr.get_messages(&topic_a_key).await;
    let topic_b = session_mgr.get_messages(&topic_b_key).await;

    assert_eq!(topic_a.len(), 4);
    assert_eq!(topic_b.len(), 2);
    assert_eq!(
        topic_a
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>(),
        vec!["topic a first", "topic a second"]
    );
    assert_eq!(
        topic_b
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>(),
        vec!["topic b first"]
    );

    let sent = sent.lock().expect("sent lock");
    assert_eq!(
        sent.iter()
            .map(|(chat_id, text)| (chat_id.as_str(), text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("208214988:111", "topic-turns=1"),
            ("208214988:222", "topic-turns=1"),
            ("208214988:111", "topic-turns=2"),
        ]
    );
}

#[tokio::test]
async fn telegram_topic_new_resets_only_current_topic_lane() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr.clone(), dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter).await;
    gw.set_message_handler(Arc::new(|messages| {
        Box::pin(async move {
            let user_turns = messages
                .iter()
                .filter(|message| message.role == MessageRole::User)
                .count();
            Ok(format!("topic-turns={user_turns}"))
        })
    }))
    .await;

    let topic_a = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "208214988:111".into(),
        user_id: "user1".into(),
        text: "topic a before reset".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    let topic_b = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "208214988:222".into(),
        user_id: "user1".into(),
        text: "topic b remains".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    gw.route_message(&topic_a).await.expect("route topic a");
    gw.route_message(&topic_b).await.expect("route topic b");

    gw.route_message(&IncomingMessage {
        text: "/new".into(),
        ..topic_a.clone()
    })
    .await
    .expect("reset topic a");

    let topic_a_key = session_mgr.compose_session_key("telegram", "208214988:111", "user1");
    let topic_b_key = session_mgr.compose_session_key("telegram", "208214988:222", "user1");
    assert!(session_mgr.get_messages(&topic_a_key).await.is_empty());
    assert_eq!(session_mgr.get_messages(&topic_b_key).await.len(), 2);

    gw.route_message(&IncomingMessage {
        text: "topic a after reset".into(),
        ..topic_a
    })
    .await
    .expect("route topic a after reset");

    let topic_a_messages = session_mgr.get_messages(&topic_a_key).await;
    let topic_b_messages = session_mgr.get_messages(&topic_b_key).await;
    assert_eq!(topic_a_messages.len(), 2);
    assert_eq!(topic_b_messages.len(), 2);
    assert_eq!(
        topic_a_messages
            .iter()
            .find(|message| message.role == MessageRole::User)
            .and_then(|message| message.content.as_deref()),
        Some("topic a after reset")
    );
    assert_eq!(
        topic_b_messages
            .iter()
            .find(|message| message.role == MessageRole::User)
            .and_then(|message| message.content.as_deref()),
        Some("topic b remains")
    );
}

#[tokio::test]
async fn telegram_topic_restore_reuses_session_switch_path() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr.clone(), dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter).await;

    let target_key = session_mgr.compose_session_key("telegram", "208214988:111", "user1");
    let current_key = session_mgr.compose_session_key("telegram", "208214988:222", "user1");
    let sibling_key = session_mgr.compose_session_key("telegram", "208214988:333", "user1");

    let _ = session_mgr
        .get_or_create_session("telegram", "208214988:111", "user1")
        .await;
    session_mgr
        .add_message(&target_key, Message::user("restored topic history"))
        .await;
    let _ = session_mgr
        .get_or_create_session("telegram", "208214988:333", "user1")
        .await;
    session_mgr
        .add_message(&sibling_key, Message::user("sibling history"))
        .await;

    gw.route_message(&IncomingMessage {
        platform: "telegram".into(),
        chat_id: "208214988:222".into(),
        user_id: "user1".into(),
        text: format!("/topic {}", target_key),
        message_id: None,
        thread_id: None,
        is_dm: true,
    })
    .await
    .expect("restore topic session");

    let current_messages = session_mgr.get_messages(&current_key).await;
    let sibling_messages = session_mgr.get_messages(&sibling_key).await;
    assert_eq!(
        current_messages
            .iter()
            .find(|message| message.role == MessageRole::User)
            .and_then(|message| message.content.as_deref()),
        Some("restored topic history")
    );
    assert_eq!(
        sibling_messages
            .iter()
            .find(|message| message.role == MessageRole::User)
            .and_then(|message| message.content.as_deref()),
        Some("sibling history")
    );
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text.contains("Switched to session")));
}

#[tokio::test]
async fn gateway_switch_session_clears_yolo_for_current_chat_context() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let current_key =
        gw.session_manager
            .compose_session_key("test", "chat-yolo-switch-current", "user1");
    let target_key =
        gw.session_manager
            .compose_session_key("test", "chat-yolo-switch-target", "user1");
    hermes_tools::approval::clear_session(&current_key);
    hermes_tools::approval::approve_session(&current_key, "recursive delete");

    let _ = gw
        .session_manager
        .get_or_create_session("test", "chat-yolo-switch-target", "user1")
        .await;
    gw.session_manager
        .add_message(&target_key, Message::user("history from another session"))
        .await;

    let yolo_chat1 = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-yolo-switch-current".into(),
        user_id: "user1".into(),
        text: "/yolo".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&yolo_chat1).await.is_ok());
    {
        let states = gw.runtime_state.read().await;
        assert_eq!(states.get(&current_key).map(|s| s.yolo), Some(true));
    }
    assert!(hermes_tools::approval::is_session_yolo_enabled(
        &current_key
    ));
    assert!(hermes_tools::approval::is_approved(
        &current_key,
        "recursive delete"
    ));

    let switch = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-yolo-switch-current".into(),
        user_id: "user1".into(),
        text: format!("/sessions {}", target_key),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&switch).await.is_ok());

    let states = gw.runtime_state.read().await;
    assert_eq!(states.get(&current_key).map(|s| s.yolo), Some(false));
    assert!(!hermes_tools::approval::is_session_yolo_enabled(
        &current_key
    ));
    assert!(!hermes_tools::approval::is_approved(
        &current_key,
        "recursive delete"
    ));
}

#[tokio::test]
async fn gateway_approve_resolves_oldest_blocking_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key =
        gw.session_manager
            .compose_session_key("test", "chat-approve-command", "user1");
    hermes_tools::approval::clear_session(&session_key);
    let (tx, rx) = std::sync::mpsc::channel();
    hermes_tools::approval::register_gateway_notify(&session_key, move |request| {
        tx.send(request).expect("approval request should send");
    });

    let session_for_thread = session_key.clone();
    let handle = std::thread::spawn(move || {
        hermes_tools::approval::check_all_command_guards_with_context(
            "rm -rf /tmp/gateway-approve-command",
            "local",
            hermes_tools::approval::CommandGuardContext {
                gateway: true,
                ask: true,
                session_key: Some(session_for_thread),
                gateway_approval_timeout: std::time::Duration::from_secs(5),
                tirith_result: Ok(Some(hermes_tools::approval::TirithResult::allow())),
                ..hermes_tools::approval::CommandGuardContext::default()
            },
            None,
        )
        .expect("approval guard should return")
    });

    let request = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("gateway approval notify should fire");
    assert_eq!(request.command, "rm -rf /tmp/gateway-approve-command");
    assert!(hermes_tools::approval::has_blocking_approval(&session_key));

    let approve = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-approve-command".into(),
        user_id: "user1".into(),
        text: "/approve".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&approve).await.is_ok());

    let result = handle.join().expect("approval guard thread should join");
    assert!(result.approved);
    assert!(result.user_approved);
    assert!(!hermes_tools::approval::has_blocking_approval(&session_key));

    let replies = sent.lock().unwrap();
    assert!(replies.iter().any(|(_, text)| {
        text.to_ascii_lowercase().contains("approved") && text.contains("Resuming")
    }));
    hermes_tools::approval::unregister_gateway_notify(&session_key);
    hermes_tools::approval::clear_session(&session_key);
}

#[tokio::test]
async fn gateway_deny_all_resolves_all_blocking_commands() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key =
        gw.session_manager
            .compose_session_key("test", "chat-deny-all-command", "user1");
    hermes_tools::approval::clear_session(&session_key);
    let (tx, rx) = std::sync::mpsc::channel();
    hermes_tools::approval::register_gateway_notify(&session_key, move |request| {
        tx.send(request).expect("approval request should send");
    });

    let mut handles = Vec::new();
    for suffix in ["a", "b"] {
        let session_for_thread = session_key.clone();
        handles.push(std::thread::spawn(move || {
            hermes_tools::approval::check_all_command_guards_with_context(
                &format!("rm -rf /tmp/gateway-deny-{suffix}"),
                "local",
                hermes_tools::approval::CommandGuardContext {
                    gateway: true,
                    ask: true,
                    session_key: Some(session_for_thread),
                    gateway_approval_timeout: std::time::Duration::from_secs(5),
                    tirith_result: Ok(Some(hermes_tools::approval::TirithResult::allow())),
                    ..hermes_tools::approval::CommandGuardContext::default()
                },
                None,
            )
            .expect("approval guard should return")
        }));
    }

    for _ in 0..2 {
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("gateway approval notify should fire");
    }
    assert_eq!(
        hermes_tools::approval::pending_gateway_approval_count(&session_key),
        2
    );

    let deny = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-deny-all-command".into(),
        user_id: "user1".into(),
        text: "/deny all".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&deny).await.is_ok());

    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("approval guard thread should join"))
        .collect::<Vec<_>>();
    assert!(results.iter().all(|result| !result.approved));
    assert!(results.iter().all(|result| {
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("User denied")
    }));
    assert!(!hermes_tools::approval::has_blocking_approval(&session_key));

    let replies = sent.lock().unwrap();
    assert!(replies
        .iter()
        .any(|(_, text)| text.contains("Denied 2 pending commands")));
    hermes_tools::approval::unregister_gateway_notify(&session_key);
    hermes_tools::approval::clear_session(&session_key);
}

#[tokio::test]
async fn gateway_new_denies_blocked_approval_for_target_session() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key =
        gw.session_manager
            .compose_session_key("test", "chat-boundary-approval", "user1");
    hermes_tools::approval::clear_session(&session_key);
    let (tx, rx) = std::sync::mpsc::channel();
    hermes_tools::approval::register_gateway_notify(&session_key, move |request| {
        tx.send(request).expect("approval request should send");
    });

    let session_for_thread = session_key.clone();
    let handle = std::thread::spawn(move || {
        hermes_tools::approval::check_all_command_guards_with_context(
            "rm -rf /tmp/gateway-boundary-approval",
            "local",
            hermes_tools::approval::CommandGuardContext {
                gateway: true,
                ask: true,
                session_key: Some(session_for_thread),
                gateway_approval_timeout: std::time::Duration::from_secs(5),
                tirith_result: Ok(Some(hermes_tools::approval::TirithResult::allow())),
                ..hermes_tools::approval::CommandGuardContext::default()
            },
            None,
        )
        .expect("approval guard should return")
    });

    rx.recv_timeout(std::time::Duration::from_secs(2))
        .expect("gateway approval notify should fire");
    assert!(hermes_tools::approval::has_blocking_approval(&session_key));

    let reset = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-boundary-approval".into(),
        user_id: "user1".into(),
        text: "/new".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset).await.is_ok());

    let result = handle.join().expect("approval guard thread should join");
    assert!(!result.approved);
    assert!(result
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("User denied"));
    assert!(!hermes_tools::approval::has_blocking_approval(&session_key));
    hermes_tools::approval::unregister_gateway_notify(&session_key);
    hermes_tools::approval::clear_session(&session_key);
}

#[tokio::test]
async fn gateway_slack_reaction_lifecycle_success() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("slack", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("done".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "slack".into(),
        chat_id: "C123".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: Some("1710000000.123".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let got = reactions.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![
            "add:C123:1710000000.123:eyes".to_string(),
            "remove:C123:1710000000.123:eyes".to_string(),
            "add:C123:1710000000.123:white_check_mark".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_discord_reaction_lifecycle_success_uses_discord_emojis() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("discord", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("done".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "C123".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: Some("123456789".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let got = reactions.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![
            "add:C123:123456789:👀".to_string(),
            "remove:C123:123456789:👀".to_string(),
            "add:C123:123456789:✅".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_discord_reactions_can_be_disabled_by_policy() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("discord", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("done".to_string()) })
    }))
    .await;

    let mut policies = HashMap::new();
    policies.insert(
        "discord".to_string(),
        PlatformAccessPolicy {
            reactions_enabled: Some(false),
            ..PlatformAccessPolicy::default()
        },
    );
    gw.set_platform_access_policies(policies).await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "C123".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: Some("123456789".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());
    assert!(reactions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn gateway_telegram_reactions_require_explicit_policy_enable() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("done".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "123".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: Some("456".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());
    assert!(reactions.lock().unwrap().is_empty());

    let mut policies = HashMap::new();
    policies.insert(
        "telegram".to_string(),
        PlatformAccessPolicy {
            reactions_enabled: Some(true),
            ..PlatformAccessPolicy::default()
        },
    );
    gw.set_platform_access_policies(policies).await;

    let second_incoming = IncomingMessage {
        message_id: Some("457".into()),
        ..incoming
    };
    assert!(gw.route_message(&second_incoming).await.is_ok());
    assert_eq!(
        reactions.lock().unwrap().clone(),
        vec![
            "add:123:457:👀".to_string(),
            "remove:123:457:👀".to_string(),
            "add:123:457:👍".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_slack_reaction_lifecycle_failure_sets_error_reaction() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("slack", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Err(GatewayError::Platform("boom".to_string())) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "slack".into(),
        chat_id: "C123".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: Some("1710000000.456".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_err());

    let got = reactions.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![
            "add:C123:1710000000.456:eyes".to_string(),
            "remove:C123:1710000000.456:eyes".to_string(),
            "add:C123:1710000000.456:x".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_slack_reactions_skip_non_dm_non_mentions() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("slack", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("done".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "slack".into(),
        chat_id: "C123".into(),
        user_id: "user1".into(),
        text: "general channel chatter".into(),
        message_id: Some("1710000000.789".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&incoming).await.is_ok());
    assert!(reactions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn gateway_context_handler_receives_structured_runtime_context() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler_with_context(Arc::new(|messages, ctx| {
            Box::pin(async move {
                let payload = format!(
                    "ctx model={:?} provider={:?} profile={:?} branch={:?} platform={} user={} session={} has_legacy_hint={}",
                    ctx.model,
                    ctx.provider,
                    ctx.profile,
                    ctx.branch,
                    ctx.platform,
                    ctx.user_id,
                    ctx.session_key,
                    messages.iter().any(|m| m
                        .content
                        .as_deref()
                        .unwrap_or("")
                        .contains("[gateway_runtime]"))
                );
                Ok(payload)
            })
        }))
        .await;

    let setup_cmds = vec![
        "/provider openai",
        "/model dynamic-structured-context-model",
        "/profile prod",
        "/branch feat-123",
    ];
    for cmd in setup_cmds {
        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: cmd.to_string(),
            message_id: None,
            thread_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&incoming).await.is_ok());
    }

    let normal = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "run".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&normal).await.is_ok());

    let msgs = sent.lock().unwrap();
    let echoed = msgs
        .iter()
        .rev()
        .find_map(|(_, text)| {
            if text.starts_with("ctx model=") {
                Some(text.clone())
            } else {
                None
            }
        })
        .expect("context response should exist");
    assert!(echoed.contains("Some(\"dynamic-structured-context-model\")"));
    assert!(echoed.contains("Some(\"openai\")"));
    assert!(echoed.contains("Some(\"prod\")"));
    assert!(echoed.contains("Some(\"feat-123\")"));
    assert!(echoed.contains("platform=test"));
    assert!(echoed.contains("user=user1"));
    assert!(echoed.contains("has_legacy_hint=false"));
}

#[tokio::test]
async fn gateway_deferred_post_delivery_messages_flush_after_main_reply() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler_with_context(Arc::new(|_messages, ctx| {
        Box::pin(async move {
            let pending = ctx
                .deferred_post_delivery_messages
                .expect("deferred queue should be present");
            let released = ctx
                .deferred_post_delivery_released
                .expect("release flag should be present");
            assert!(
                !released.load(std::sync::atomic::Ordering::Acquire),
                "release must remain false before main reply delivery"
            );
            pending
                .lock()
                .unwrap()
                .push("💾 deferred-memory-update".to_string());
            Ok("main-response".to_string())
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let msgs = sent.lock().unwrap();
    let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(
        ordered,
        vec![
            "main-response".to_string(),
            "💾 deferred-memory-update".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_replies_and_deferred_messages_preserve_source_thread() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ThreadOptionTestAdapter {
        sends: sends.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("thread-option-test", adapter).await;
    gw.set_message_handler_with_context(Arc::new(|_messages, ctx| {
        Box::pin(async move {
            let pending = ctx
                .deferred_post_delivery_messages
                .expect("deferred queue should be present");
            pending
                .lock()
                .unwrap()
                .push("deferred follow-up".to_string());
            Ok("final reply".to_string())
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "thread-option-test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "run".into(),
        message_id: Some("post-2".into()),
        thread_id: Some("root-1".into()),
        is_dm: true,
    };
    gw.route_message(&incoming)
        .await
        .expect("threaded route should succeed");

    let sent = sends.lock().unwrap().clone();
    assert_eq!(
        sent,
        vec![
            (
                "chat1".to_string(),
                "final reply".to_string(),
                Some("root-1".to_string()),
                true,
            ),
            (
                "chat1".to_string(),
                "deferred follow-up".to_string(),
                Some("root-1".to_string()),
                false,
            ),
        ]
    );
}

#[tokio::test]
async fn gateway_status_then_main_then_deferred_order_matches_python_chain() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
    gw.register_adapter("test", adapter).await;

    let gw_for_handler = gw.clone();
    gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
        let gw = gw_for_handler.clone();
        Box::pin(async move {
            let pending = ctx
                .deferred_post_delivery_messages
                .expect("deferred queue should be present");
            pending.lock().unwrap().push("💾 bg-review".to_string());

            // Mirrors Python's status_callback: status is forwarded immediately.
            gw.send_message(&ctx.platform, &ctx.chat_id, "⚠️ context pressure", None)
                .await
                .expect("status callback send should succeed");

            Ok("main-response".to_string())
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let msgs = sent.lock().unwrap();
    let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(
        ordered,
        vec![
            "⚠️ context pressure".to_string(),
            "main-response".to_string(),
            "💾 bg-review".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_streaming_flushes_deferred_after_stream_finishes() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.streaming_enabled = true;
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    gw.set_streaming_handler_with_context(Arc::new(|_messages, ctx, _on_chunk| {
        Box::pin(async move {
            let pending = ctx
                .deferred_post_delivery_messages
                .expect("deferred queue should be present");
            let released = ctx
                .deferred_post_delivery_released
                .expect("release flag should be present");
            assert!(
                !released.load(std::sync::atomic::Ordering::Acquire),
                "release must stay false while stream handler is running"
            );
            pending
                .lock()
                .unwrap()
                .push("💾 stream-bg-review".to_string());
            Ok("stream-final".to_string())
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let msgs = sent.lock().unwrap();
    let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(
        ordered,
        vec![
            "...".to_string(),
            "stream-final".to_string(),
            "💾 stream-bg-review".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_emits_agent_start_and_end_hooks() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "agent:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async move { Ok("main-response".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec!["agent:start".to_string(), "agent:end".to_string()]
    );
    let end_payload = events
        .iter()
        .find(|(name, _)| name == "agent:end")
        .map(|(_, ctx)| ctx.clone())
        .expect("agent:end payload should exist");
    assert_eq!(end_payload["success"], serde_json::json!(true));
}

#[tokio::test]
async fn gateway_busy_queue_mode_drains_fifo_followups() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_input_mode = Some("queue".to_string());
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let calls_for_handler = calls.clone();
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let calls = calls_for_handler.clone();
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|m| {
                    (m.role == MessageRole::User)
                        .then(|| m.content.clone())
                        .flatten()
                })
                .unwrap_or_default();
            calls.lock().unwrap().push(latest.clone());
            if latest == "first" {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().await.take() {
                    let _ = rx.await;
                }
            }
            Ok(format!("reply:{latest}"))
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should enter handler");
    gw.route_message(&test_incoming("second"))
        .await
        .expect("second route should queue");
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_string(), "second".to_string()]
    );
    let texts: Vec<String> = sent
        .lock()
        .unwrap()
        .iter()
        .map(|(_, text)| text.clone())
        .collect();
    assert!(texts
        .iter()
        .any(|text| text.contains("Queued for the next turn")));
    assert!(texts.iter().any(|text| text == "reply:first"));
    assert!(texts.iter().any(|text| text == "reply:second"));
}

#[tokio::test]
async fn gateway_busy_queue_ack_can_be_suppressed() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_input_mode = Some("queue".to_string());
    cfg.display.busy_ack_enabled = Some(false);
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|m| {
                    (m.role == MessageRole::User)
                        .then(|| m.content.clone())
                        .flatten()
                })
                .unwrap_or_default();
            if latest == "first" {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().await.take() {
                    let _ = rx.await;
                }
            }
            Ok(format!("reply:{latest}"))
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should enter handler");
    gw.route_message(&test_incoming("second"))
        .await
        .expect("second route should queue silently");
    assert!(
        sent.lock()
            .unwrap()
            .iter()
            .all(|(_, text)| !text.contains("Queued for the next turn")),
        "automatic busy ack should be suppressed"
    );
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
}

#[tokio::test]
async fn gateway_queue_command_bypasses_busy_guard_and_drains() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_input_mode = Some("interrupt".to_string());
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let calls_for_handler = calls.clone();
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let calls = calls_for_handler.clone();
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|m| {
                    (m.role == MessageRole::User)
                        .then(|| m.content.clone())
                        .flatten()
                })
                .unwrap_or_default();
            calls.lock().unwrap().push(latest.clone());
            if latest == "first" {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().await.take() {
                    let _ = rx.await;
                }
            }
            Ok(format!("reply:{latest}"))
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should enter handler");
    gw.route_message(&test_incoming("/queue second"))
        .await
        .expect("/queue should bypass and enqueue");
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_string(), "second".to_string()]
    );
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text.contains("Queued follow-up for the active session")));
}

#[tokio::test]
async fn gateway_steer_command_uses_attached_busy_control() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
    gw.register_adapter("test", adapter).await;

    let control = Arc::new(BusyControlProbe::default());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let control_for_handler = control.clone();
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
        let control = control_for_handler.clone();
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            if let Some(registration) = ctx.busy_control {
                assert!(registration.attach(control).await);
            }
            if let Some(tx) = entered.lock().unwrap().take() {
                let _ = tx.send(());
            }
            if let Some(rx) = release.lock().await.take() {
                let _ = rx.await;
            }
            Ok("reply:first".to_string())
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should attach control");
    gw.route_message(&test_incoming("/steer check tests"))
        .await
        .expect("/steer should use attached control");
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");

    assert_eq!(
        control.steers.lock().unwrap().as_slice(),
        ["check tests".to_string()]
    );
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text.contains("Steered the running task")));
}

#[tokio::test]
async fn gateway_hook_event_order_captures_start_status_step_end() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "agent:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;

    let gw_for_handler = gw.clone();
    gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
        let gw = gw_for_handler.clone();
        Box::pin(async move {
            gw.emit_hook_event(
                "agent:status",
                serde_json::json!({
                    "platform": ctx.platform,
                    "user_id": ctx.user_id,
                    "session_id": ctx.session_key,
                    "event_type": "lifecycle",
                    "message": "Context pressure 85%"
                }),
            )
            .await;
            gw.emit_hook_event(
                "agent:step",
                serde_json::json!({
                    "platform": ctx.platform,
                    "user_id": ctx.user_id,
                    "session_id": ctx.session_key,
                    "iteration": 1,
                    "tool_names": ["memory"],
                    "tools": [{"name":"memory","result":"ok"}]
                }),
            )
            .await;
            Ok("done".to_string())
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec![
            "agent:start".to_string(),
            "agent:status".to_string(),
            "agent:step".to_string(),
            "agent:end".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_emits_session_start_and_command_hook_events() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "session:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );
    hooks.register_in_process(
        "command:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/status".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert!(names.contains(&"session:start".to_string()));
    assert!(names.contains(&"command:status".to_string()));
}

#[tokio::test]
async fn gateway_emits_session_end_and_reset_for_reset_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "session:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async move { Ok("assistant".to_string()) })
    }))
    .await;
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");

    let normal = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&normal).await.is_ok());

    let reset = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/reset".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec![
            "session:start".to_string(),
            "session:end".to_string(),
            "session:reset".to_string()
        ]
    );
    let end_payload = events
        .iter()
        .find(|(name, _)| name == "session:end")
        .map(|(_, ctx)| ctx.clone())
        .expect("session:end payload should exist");
    let reset_payload = events
        .iter()
        .find(|(name, _)| name == "session:reset")
        .map(|(_, ctx)| ctx.clone())
        .expect("session:reset payload should exist");
    assert_eq!(end_payload["session_id"], serde_json::json!(session_key));
    assert_eq!(reset_payload["session_id"], serde_json::json!(session_key));
}

#[tokio::test]
async fn gateway_emits_plugin_session_finalize_and_reset_for_reset_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "on_session_finalize",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );
    hooks.register_in_process(
        "on_session_reset",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async move { Ok("assistant".to_string()) })
    }))
    .await;
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat-plugin-reset", "user1");

    let normal = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-plugin-reset".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&normal).await.is_ok());
    let old_logical_id = gw
        .session_manager
        .get_session(&session_key)
        .await
        .expect("session should exist")
        .id;

    let reset = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-plugin-reset".into(),
        user_id: "user1".into(),
        text: "/reset".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec![
            "on_session_finalize".to_string(),
            "on_session_reset".to_string()
        ]
    );
    let finalize_payload = &events[0].1;
    let reset_payload = &events[1].1;
    assert_eq!(
        finalize_payload["session_id"],
        serde_json::json!(old_logical_id)
    );
    assert_eq!(
        finalize_payload["session_key"],
        serde_json::json!(session_key)
    );
    assert_eq!(finalize_payload["reason"], serde_json::json!("reset"));
    assert_eq!(reset_payload["session_key"], serde_json::json!(session_key));
    assert_eq!(reset_payload["reason"], serde_json::json!("reset"));
    assert_ne!(reset_payload["session_id"], finalize_payload["session_id"]);
}

#[tokio::test]
async fn gateway_stop_all_finalizes_active_sessions() {
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "on_session_finalize",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let first = session_mgr
        .get_or_create_session("test", "stop-chat-a", "user1")
        .await;
    let second = session_mgr
        .get_or_create_session("test", "stop-chat-b", "user2")
        .await;
    let gw = Gateway::new(
        session_mgr,
        DmManager::with_pair_behavior(),
        GatewayConfig::default(),
    );
    gw.set_hook_registry(Arc::new(hooks)).await;

    gw.stop_all().await.expect("stop should succeed");

    let events = hook_seen.lock().unwrap();
    let session_ids: HashSet<String> = events
        .iter()
        .filter(|(name, _)| name == "on_session_finalize")
        .filter_map(|(_, ctx)| ctx["session_id"].as_str().map(ToOwned::to_owned))
        .collect();
    assert_eq!(
        session_ids,
        HashSet::from([first.id.clone(), second.id.clone()])
    );
    assert!(events
        .iter()
        .all(|(_, ctx)| ctx["reason"] == serde_json::json!("shutdown")));
}

#[tokio::test]
async fn gateway_idle_expiry_finalizes_removed_sessions() {
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "on_session_finalize",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_config = SessionConfig {
        reset_policy: hermes_config::session::SessionResetPolicy::Idle { timeout_minutes: 0 },
        ..SessionConfig::default()
    };
    let session_mgr = Arc::new(SessionManager::new(session_config));
    let expired = session_mgr
        .get_or_create_session("test", "idle-chat", "user1")
        .await;
    let session_key = session_mgr.compose_session_key("test", "idle-chat", "user1");
    let gw = Gateway::new(
        session_mgr.clone(),
        DmManager::with_pair_behavior(),
        GatewayConfig::default(),
    );
    gw.set_hook_registry(Arc::new(hooks)).await;

    let expired_count = gw.expire_idle_sessions_once("idle_expiry").await;

    assert_eq!(expired_count, 1);
    assert!(session_mgr.get_session(&session_key).await.is_none());
    let events = hook_seen.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "on_session_finalize");
    assert_eq!(events[0].1["session_id"], serde_json::json!(expired.id));
    assert_eq!(events[0].1["session_key"], serde_json::json!(session_key));
    assert_eq!(events[0].1["reason"], serde_json::json!("idle_expiry"));
}

#[tokio::test]
async fn gateway_hook_error_does_not_break_reset_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let mut hooks = HookRegistry::new();
    hooks.register_in_process("session:*", Arc::new(FailingHook));

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;

    let reset = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-hook-error".into(),
        user_id: "user1".into(),
        text: "/new".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset).await.is_ok());

    let replies = sent.lock().unwrap();
    assert!(replies
        .iter()
        .any(|(_, text)| { text.contains("New conversation") || text.contains("Session reset") }));
}
