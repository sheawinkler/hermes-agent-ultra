fn honcho_empty_profile_hint(peer: &str) -> String {
    let peer = peer.trim();
    let label = if peer.is_empty() { "this peer" } else { peer };
    format!(
        "Honcho returned an empty profile card for {label}. Use honcho_search for raw context, honcho_context for a synthesized answer, or honcho_conclude to save a durable user fact."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration as StdDuration;

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn oauth_host_block(
        access: &str,
        refresh: &str,
        expires_at: i64,
        token_endpoint: &str,
    ) -> Value {
        json!({
            "apiKey": access,
            "oauth": {
                "refreshToken": refresh,
                "expiresAt": expires_at,
                "clientId": "hermes-agent",
                "tokenEndpoint": token_endpoint,
                "scope": "write",
                "tokenType": "Bearer"
            }
        })
    }

    fn http_request_complete(raw: &[u8]) -> bool {
        let Some(header_end) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&raw[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        raw.len() >= header_end + 4 + content_length
    }

    fn one_shot_http_server(
        status: &'static str,
        body: &'static str,
    ) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(StdDuration::from_secs(2)))
                .expect("timeout");
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        request.extend_from_slice(&buf[..n]);
                        if http_request_complete(&request) {
                            break;
                        }
                    }
                    Err(err)
                        if matches!(
                            err.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(err) => panic!("read request: {err}"),
                }
            }
            tx.send(String::from_utf8_lossy(&request).to_string())
                .expect("send request");
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).expect("write");
        });
        (format!("http://{addr}"), rx)
    }

    #[test]
    fn test_honcho_plugin_name() {
        let plugin = HonchoMemoryPlugin::new();
        assert_eq!(plugin.name(), "honcho");
    }

    #[test]
    fn test_honcho_is_not_available_without_explicit_config() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _api = EnvGuard::remove("HONCHO_API_KEY");
        let _url = EnvGuard::remove("HONCHO_BASE_URL");

        assert!(!HonchoMemoryPlugin::new().is_available());
    }

    #[test]
    fn test_honcho_config_file_activates_provider_without_env() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _api = EnvGuard::remove("HONCHO_API_KEY");
        let _url = EnvGuard::remove("HONCHO_BASE_URL");
        std::fs::write(
            tmp.path().join("honcho.json"),
            r#"{"baseUrl":"http://localhost:8000","enabled":true}"#,
        )
        .expect("write config");

        assert!(HonchoMemoryPlugin::new().is_available());
    }

    #[test]
    fn test_honcho_initialize_is_fail_open_and_does_not_contact_network() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _api = EnvGuard::remove("HONCHO_API_KEY");
        let _url = EnvGuard::remove("HONCHO_BASE_URL");
        std::fs::write(
            tmp.path().join("honcho.json"),
            r#"{"baseUrl":"http://10.255.255.1:9","enabled":true,"timeout":60,"recallMode":"hybrid"}"#,
        )
        .expect("write config");

        let plugin = HonchoMemoryPlugin::new();
        let started = std::time::Instant::now();
        plugin.initialize("session-1", &tmp.path().to_string_lossy());

        assert!(
            started.elapsed() < Duration::from_millis(250),
            "initialize should only load config and must not block on Honcho network/session startup"
        );
        assert!(plugin.config.lock().unwrap().is_some());
        assert_eq!(*plugin.session_key.lock().unwrap(), "session-1");
        assert_eq!(plugin.get_tool_schemas().len(), 4);
        assert!(plugin.system_prompt_block().contains("hybrid mode"));
    }

    #[test]
    fn test_honcho_save_config_normalizes_key_and_writes_owner_only() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let path = tmp.path().join("honcho.json");
        std::fs::write(&path, r#"{"workspace":"existing"}"#).expect("write existing");

        HonchoMemoryPlugin::new()
            .save_config(&json!({"api_key":"hc-secret","baseUrl":"http://localhost:8000"}))
            .expect("save config");

        let parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read config"))
                .expect("parse config");
        assert_eq!(parsed["workspace"], "existing");
        assert_eq!(parsed["apiKey"], "hc-secret");
        assert!(parsed.get("api_key").is_none());
        assert_eq!(parsed["baseUrl"], "http://localhost:8000");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn test_honcho_tool_schemas() {
        let plugin = HonchoMemoryPlugin::new();
        let schemas = plugin.get_tool_schemas();
        assert_eq!(schemas.len(), 4);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"honcho_profile"));
        assert!(names.contains(&"honcho_search"));
        assert!(names.contains(&"honcho_context"));
        assert!(names.contains(&"honcho_conclude"));
    }

    #[test]
    fn test_honcho_context_mode_hides_tools() {
        let plugin = HonchoMemoryPlugin::new();
        *plugin.recall_mode.lock().unwrap() = "context".to_string();
        assert!(plugin.get_tool_schemas().is_empty());
    }

    #[test]
    fn test_honcho_system_prompt_modes() {
        let plugin = HonchoMemoryPlugin::new();
        *plugin.recall_mode.lock().unwrap() = "hybrid".to_string();
        assert!(plugin.system_prompt_block().contains("hybrid mode"));

        *plugin.recall_mode.lock().unwrap() = "tools".to_string();
        assert!(plugin.system_prompt_block().contains("tools-only mode"));

        *plugin.recall_mode.lock().unwrap() = "context".to_string();
        assert!(plugin
            .system_prompt_block()
            .contains("context-injection mode"));
    }

    #[test]
    fn test_apply_template() {
        let path =
            HonchoMemoryPlugin::apply_template("/v1/sessions/{session}/peers/{peer}", "user", "s1");
        assert_eq!(path, "/v1/sessions/s1/peers/user");
    }

    fn test_config() -> HonchoConfig {
        HonchoConfig {
            api_key: String::new(),
            base_url: "https://api.honcho.dev".to_string(),
            enabled: true,
            recall_mode: "hybrid".to_string(),
            context_tokens: Some(800),
            workspace_id: "hermes".to_string(),
            peer_name: Some("eri".to_string()),
            ai_peer: "hermes".to_string(),
            pin_user_peer: false,
            user_peer_aliases: HashMap::new(),
            runtime_peer_prefix: String::new(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            endpoints: HashMap::new(),
            host_had_explicit_api_key: false,
            host: HOST.to_string(),
            config_path: HonchoConfig::default_config_path(),
            oauth: None,
        }
    }

    #[test]
    fn test_honcho_profile_host_key_uses_safe_underscore_form() {
        assert_eq!(profile_host_key(None), "hermes");
        assert_eq!(profile_host_key(Some("default")), "hermes");
        assert_eq!(profile_host_key(Some("coder")), "hermes_coder");
        assert_eq!(
            profile_host_key(Some("research.team/v1")),
            "hermes_research_team_v1"
        );
        assert_eq!(
            legacy_profile_host_key("hermes_research_team").as_deref(),
            Some("hermes.research_team")
        );
    }

    #[test]
    fn test_honcho_config_reads_legacy_dot_host_and_strips_version_suffix() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _profile = EnvGuard::set("HERMES_PROFILE", "coder");
        let _api = EnvGuard::remove("HONCHO_API_KEY");
        let _url = EnvGuard::remove("HONCHO_BASE_URL");
        std::fs::write(
            tmp.path().join("honcho.json"),
            r#"{
                "baseUrl":"https://honcho.internal/v3/",
                "enabled":true,
                "hosts":{
                    "hermes.coder":{
                        "apiKey":"local-jwt",
                        "aiPeer":"coder-ai",
                        "peerName":"operator"
                    }
                }
            }"#,
        )
        .expect("write config");

        let cfg = HonchoConfig::from_config_file(&tmp.path().to_string_lossy());
        assert_eq!(cfg.base_url, "https://honcho.internal");
        assert_eq!(cfg.api_key, "local-jwt");
        assert_eq!(cfg.ai_peer, "coder-ai");
        assert_eq!(cfg.peer_name.as_deref(), Some("operator"));
        assert!(cfg.host_had_explicit_api_key);
    }

    #[test]
    fn test_honcho_config_loads_global_fallback_and_normalizes_base_url() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let hermes_home = tmp.path().join("profile-home");
        let global_dir = tmp.path().join(".honcho");
        std::fs::create_dir_all(&global_dir).expect("mkdir global");
        let _home = EnvGuard::set("HOME", tmp.path());
        let _hermes_home = EnvGuard::set("HERMES_HOME", &hermes_home);
        let _profile = EnvGuard::remove("HERMES_PROFILE");
        let _api = EnvGuard::remove("HONCHO_API_KEY");
        let _url = EnvGuard::remove("HONCHO_BASE_URL");
        std::fs::write(
            global_dir.join("config.json"),
            r#"{"baseUrl":"honcho.example.com/v1","enabled":true,"timeout":45}"#,
        )
        .expect("write global config");

        let cfg = HonchoConfig::from_config_file(&hermes_home.to_string_lossy());

        assert_eq!(cfg.base_url, "https://honcho.example.com");
        assert_eq!(cfg.timeout_secs, 45.0);
        assert!(cfg.enabled);
    }

    #[test]
    fn test_honcho_loopback_config_skips_top_level_cloud_key_without_host_jwt() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _profile = EnvGuard::remove("HERMES_PROFILE");
        let _api = EnvGuard::remove("HONCHO_API_KEY");
        let _url = EnvGuard::remove("HONCHO_BASE_URL");
        std::fs::write(
            tmp.path().join("honcho.json"),
            r#"{
                "baseUrl":"http://localhost:8000/v3",
                "apiKey":"cloud-key",
                "enabled":true,
                "hosts":{"hermes":{"enabled":true}}
            }"#,
        )
        .expect("write config");

        let cfg = HonchoConfig::from_config_file(&tmp.path().to_string_lossy());
        assert_eq!(cfg.base_url, "http://localhost:8000");
        assert_eq!(cfg.api_key, "");
        assert!(!cfg.host_had_explicit_api_key);
    }

    #[test]
    fn test_honcho_oauth_config_loads_host_grant() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _profile = EnvGuard::remove("HERMES_PROFILE");
        let _api = EnvGuard::remove("HONCHO_API_KEY");
        let _url = EnvGuard::remove("HONCHO_BASE_URL");
        std::fs::write(
            tmp.path().join("honcho.json"),
            json!({
                "enabled": true,
                "hosts": {
                    "hermes": oauth_host_block(
                        "hch-at-old",
                        "hch-rt-old",
                        9_999_999_999,
                        "http://127.0.0.1:1/oauth/token"
                    )
                }
            })
            .to_string(),
        )
        .expect("write config");

        let cfg = HonchoConfig::from_config_file(&tmp.path().to_string_lossy());

        assert_eq!(cfg.api_key, "hch-at-old");
        assert!(cfg.host_had_explicit_api_key);
        let oauth = cfg.oauth.expect("oauth credential");
        assert_eq!(oauth.refresh_token, "hch-rt-old");
        assert_eq!(oauth.client_id, "hermes-agent");
        assert_eq!(cfg.config_path, tmp.path().join("honcho.json"));
    }

    #[test]
    fn test_honcho_oauth_fresh_token_skips_refresh() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _profile = EnvGuard::remove("HERMES_PROFILE");
        let _api = EnvGuard::remove("HONCHO_API_KEY");
        let _url = EnvGuard::remove("HONCHO_BASE_URL");
        let token_endpoint = "http://127.0.0.1:9/oauth/token";
        std::fs::write(
            tmp.path().join("honcho.json"),
            json!({
                "enabled": true,
                "hosts": {
                    "hermes": oauth_host_block(
                        "hch-at-fresh",
                        "hch-rt-fresh",
                        (epoch_seconds() + 3600.0) as i64,
                        token_endpoint
                    )
                }
            })
            .to_string(),
        )
        .expect("write config");
        let mut cfg = HonchoConfig::from_config_file(&tmp.path().to_string_lossy());

        let refreshed = cfg.refresh_oauth_if_needed().expect("refresh check");

        assert!(!refreshed);
        assert_eq!(cfg.api_key, "hch-at-fresh");
    }

    #[test]
    fn test_honcho_oauth_expired_token_refreshes_and_persists_rotation() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _profile = EnvGuard::remove("HERMES_PROFILE");
        let _api = EnvGuard::remove("HONCHO_API_KEY");
        let _url = EnvGuard::remove("HONCHO_BASE_URL");
        let (token_endpoint, rx) = one_shot_http_server(
            "200 OK",
            r#"{"access_token":"hch-at-new","refresh_token":"hch-rt-new","expires_in":3600,"scope":"write","token_type":"Bearer"}"#,
        );
        let config_path = tmp.path().join("honcho.json");
        std::fs::write(
            &config_path,
            json!({
                "apiKey": "hch-v3-root",
                "enabled": true,
                "hosts": {
                    "obsidian": {"workspace": "obsidian"},
                    "hermes": oauth_host_block("hch-at-old", "hch-rt-old", 100, &token_endpoint)
                }
            })
            .to_string(),
        )
        .expect("write config");
        let mut cfg = HonchoConfig::from_config_file(&tmp.path().to_string_lossy());

        let refreshed = cfg.refresh_oauth_if_needed().expect("refresh");

        assert!(refreshed);
        assert_eq!(cfg.api_key, "hch-at-new");
        let request = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
        assert!(request.starts_with("POST / HTTP/1.1"));
        assert!(request.contains("grant_type=refresh_token"));
        assert!(request.contains("client_id=hermes-agent"));
        assert!(request.contains("refresh_token=hch-rt-old"));
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).expect("read config"))
                .expect("json");
        assert_eq!(saved["apiKey"], "hch-v3-root");
        assert_eq!(saved["hosts"]["obsidian"]["workspace"], "obsidian");
        assert_eq!(saved["hosts"]["hermes"]["apiKey"], "hch-at-new");
        assert_eq!(
            saved["hosts"]["hermes"]["oauth"]["refreshToken"],
            "hch-rt-new"
        );
    }

    #[test]
    fn test_honcho_oauth_refresh_failure_fails_open() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _profile = EnvGuard::remove("HERMES_PROFILE");
        let _api = EnvGuard::remove("HONCHO_API_KEY");
        let _url = EnvGuard::remove("HONCHO_BASE_URL");
        let (token_endpoint, _rx) = one_shot_http_server("500 Internal Server Error", "{}");
        let config_path = tmp.path().join("honcho.json");
        std::fs::write(
            &config_path,
            json!({
                "enabled": true,
                "hosts": {
                    "hermes": oauth_host_block("hch-at-old", "hch-rt-old", 100, &token_endpoint)
                }
            })
            .to_string(),
        )
        .expect("write config");
        let mut cfg = HonchoConfig::from_config_file(&tmp.path().to_string_lossy());

        let refreshed = cfg.refresh_oauth_if_needed().expect("fail open");

        assert!(!refreshed);
        assert_eq!(cfg.api_key, "hch-at-old");
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).expect("read config"))
                .expect("json");
        assert_eq!(saved["hosts"]["hermes"]["apiKey"], "hch-at-old");
        assert_eq!(
            saved["hosts"]["hermes"]["oauth"]["refreshToken"],
            "hch-rt-old"
        );
    }

    #[test]
    fn test_honcho_send_json_uses_refreshed_oauth_token() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _profile = EnvGuard::remove("HERMES_PROFILE");
        let _api = EnvGuard::remove("HONCHO_API_KEY");
        let _url = EnvGuard::remove("HONCHO_BASE_URL");
        let (token_endpoint, _token_rx) = one_shot_http_server(
            "200 OK",
            r#"{"access_token":"hch-at-new","refresh_token":"hch-rt-new","expires_in":3600}"#,
        );
        let (api_base, api_rx) = one_shot_http_server("200 OK", r#"{"ok":true}"#);
        std::fs::write(
            tmp.path().join("honcho.json"),
            json!({
                "enabled": true,
                "baseUrl": api_base,
                "hosts": {
                    "hermes": oauth_host_block("hch-at-old", "hch-rt-old", 100, &token_endpoint)
                }
            })
            .to_string(),
        )
        .expect("write config");
        let cfg = HonchoConfig::from_config_file(&tmp.path().to_string_lossy());

        let response =
            HonchoMemoryPlugin::send_json(&cfg, Method::GET, "/ping", None, None).expect("send");

        assert_eq!(response["ok"], true);
        let api_request = api_rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("api request");
        assert!(api_request.starts_with("GET /ping HTTP/1.1"));
        let api_request_lower = api_request.to_ascii_lowercase();
        assert!(api_request_lower.contains("authorization: bearer hch-at-new"));
        assert!(api_request_lower.contains("x-api-key: hch-at-new"));
    }

    #[test]
    fn test_honcho_pin_user_peer_wins_over_runtime_identity() {
        let mut config = test_config();
        config.pin_user_peer = true;
        assert_eq!(
            HonchoMemoryPlugin::resolve_user_peer_id(
                &config,
                "telegram:chat-1",
                &["86701400".to_string()],
            ),
            "eri"
        );
    }

    #[test]
    fn test_honcho_runtime_aliases_check_primary_and_alt_ids() {
        let mut config = test_config();
        config
            .user_peer_aliases
            .insert("@eri".to_string(), "eri/main".to_string());
        assert_eq!(
            HonchoMemoryPlugin::resolve_user_peer_id(
                &config,
                "telegram:chat-1",
                &["86701400".to_string(), "@eri".to_string()],
            ),
            "eri-main"
        );
    }

    #[test]
    fn test_honcho_runtime_prefix_hashes_colliding_explicit_peer() {
        let mut config = test_config();
        config.peer_name = Some("telegram_86701400".to_string());
        config.runtime_peer_prefix = "telegram_".to_string();
        let peer = HonchoMemoryPlugin::resolve_user_peer_id(
            &config,
            "telegram:chat-1",
            &["86701400".to_string()],
        );
        assert!(peer.starts_with("telegram_86701400-"));
        assert!(peer.len() > "telegram_86701400-".len());
    }

    #[test]
    fn test_honcho_user_peer_falls_back_to_sanitized_session_key() {
        let mut config = test_config();
        config.peer_name = None;
        assert_eq!(
            HonchoMemoryPlugin::resolve_user_peer_id(&config, "telegram:chat/1", &[]),
            "user-telegram-chat-1"
        );
    }

    #[test]
    fn test_honcho_extract_peer_uses_runtime_mapping_and_sanitizes_explicit_peer() {
        let mut config = test_config();
        config
            .user_peer_aliases
            .insert("42".to_string(), "eri".to_string());
        assert_eq!(
            HonchoMemoryPlugin::extract_peer(&config, &json!({"runtime_user_id": "42"})),
            "eri"
        );
        assert_eq!(
            HonchoMemoryPlugin::extract_peer(&config, &json!({"peer": "team/user"})),
            "team-user"
        );
        assert_eq!(
            HonchoMemoryPlugin::extract_peer(&config, &json!({"peer": "ai"})),
            "hermes"
        );
    }

    #[test]
    fn test_honcho_empty_profile_hint_points_to_memory_actions() {
        let hint = honcho_empty_profile_hint("user-peer");
        assert!(hint.contains("user-peer"));
        assert!(hint.contains("honcho_search"));
        assert!(hint.contains("honcho_context"));
        assert!(hint.contains("honcho_conclude"));
    }

    #[test]
    fn test_honcho_identity_mapping_config_replaces_root_map_at_host_level() {
        let mut config = test_config();
        let root = json!({
            "pinUserPeer": true,
            "userPeerAliases": {"root-id": "root-peer"},
            "runtimePeerPrefix": "root_"
        });
        let host = json!({
            "pinPeerName": false,
            "userPeerAliases": {"host-id": "host-peer"},
            "runtimePeerPrefix": ""
        });
        HonchoConfig::apply_config_value(&mut config, &root);
        HonchoConfig::apply_config_value(&mut config, &host);

        assert!(!config.pin_user_peer);
        assert_eq!(config.user_peer_aliases.len(), 1);
        assert_eq!(
            config.user_peer_aliases.get("host-id").map(String::as_str),
            Some("host-peer")
        );
        assert_eq!(config.runtime_peer_prefix, "");
    }
}
