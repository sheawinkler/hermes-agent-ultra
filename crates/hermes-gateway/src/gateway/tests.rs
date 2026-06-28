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

mod hook_lifecycle;
mod runtime_controls;
