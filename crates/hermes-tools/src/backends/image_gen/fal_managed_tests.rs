use super::*;
use hermes_config::managed_gateway::test_lock;

/// Hermetic env scope: HERMES_HOME → tempdir + flag/token cleared.
struct EnvScope {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    original: Vec<(&'static str, Option<String>)>,
    _g: std::sync::MutexGuard<'static, ()>,
}

impl EnvScope {
    fn new() -> Self {
        let g = test_lock::lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let keys = [
            "HERMES_HOME",
            "HOME",
            "FAL_KEY",
            "FAL_IMAGE_MODEL",
            "HERMES_FAL_IMAGE_MODEL",
            "OPENAI_IMAGE_MODEL",
            "HERMES_IMAGE_GEN_PROVIDER",
            "HERMES_IMAGE_GEN_BACKEND",
            "IMAGE_GEN_PROVIDER",
            "IMAGE_GEN_BACKEND",
            "HERMES_OPENAI_CODEX_API_KEY",
            "OPENAI_CODEX_ACCESS_TOKEN",
            "CODEX_ACCESS_TOKEN",
            "HERMES_OPENAI_CODEX_BASE_URL",
            "OPENAI_CODEX_BASE_URL",
            "HERMES_CODEX_IMAGE_CHAT_MODEL",
            "OPENAI_CODEX_IMAGE_CHAT_MODEL",
            "HERMES_AUTH_FILE",
            "OPENAI_API_KEY",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_IMAGE_BASE_URL",
            "HERMES_OPENAI_IMAGE_BASE_URL",
            "OPENAI_BASE_URL",
            "HERMES_OPENAI_BASE_URL",
            "OPENROUTER_API_KEY",
            "OPENROUTER_IMAGE_MODEL",
            "OPENROUTER_IMAGE_BASE_URL",
            "OPENROUTER_BASE_URL",
            "NOUS_API_KEY",
            "NOUS_IMAGE_MODEL",
            "NOUS_IMAGE_BASE_URL",
            "NOUS_BASE_URL",
            "HERMES_NOUS_OAUTH_FILE",
            "KREA_API_KEY",
            "KREA_IMAGE_MODEL",
            "KREA_IMAGE_CREATIVITY",
            "KREA_IMAGE_BASE_URL",
            "KREA_BASE_URL",
            "KREA_USE_GATEWAY",
            "HERMES_KREA_USE_GATEWAY",
            "KREA_GATEWAY_URL",
            "XAI_API_KEY",
            "HERMES_XAI_API_KEY",
            "XAI_IMAGE_MODEL",
            "XAI_IMAGE_RESOLUTION",
            "XAI_IMAGE_BASE_URL",
            "HERMES_XAI_IMAGE_BASE_URL",
            "XAI_BASE_URL",
            "HERMES_XAI_BASE_URL",
            "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
            "TOOL_GATEWAY_USER_TOKEN",
            "TOOL_GATEWAY_DOMAIN",
            "TOOL_GATEWAY_SCHEME",
        ];
        let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
        for k in &keys {
            std::env::remove_var(k);
        }
        std::env::set_var("HERMES_HOME", &home);
        std::env::set_var("HOME", &home);
        Self {
            _tmp: tmp,
            home,
            original,
            _g: g,
        }
    }

    fn auth_path(&self) -> PathBuf {
        self.home.join("auth.json")
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (k, v) in &self.original {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn image_request(prompt: &str) -> ImageGenerateRequest {
    ImageGenerateRequest {
        prompt: prompt.to_string(),
        size: None,
        style: None,
        n: None,
        image_url: None,
        reference_image_urls: Vec::new(),
    }
}

#[test]
fn from_env_or_managed_prefers_direct_key() {
    let _g = EnvScope::new();
    std::env::set_var("FAL_KEY", "direct-key");
    let b = FalImageGenBackend::from_env_or_managed().unwrap();
    assert_eq!(b.transport_label(), "direct");
    assert_eq!(b.model_path(), DEFAULT_FAL_MODEL_PATH);
}

#[test]
fn from_env_or_managed_falls_back_to_nous_gateway() {
    let _g = EnvScope::new();
    std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
    std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "nous-tok");
    let b = FalImageGenBackend::from_env_or_managed().unwrap();
    assert_eq!(b.transport_label(), "managed");
}

#[test]
fn from_env_or_managed_errors_when_neither_configured() {
    let _g = EnvScope::new();
    let err = FalImageGenBackend::from_env_or_managed().unwrap_err();
    assert!(err.to_string().contains("FAL_KEY"));
    assert!(err.to_string().contains("fal-queue"));
}

#[test]
fn managed_submit_url_uses_run_path() {
    let cfg = ManagedToolGatewayConfig {
        vendor: "fal-queue".into(),
        gateway_origin: "https://fal-queue.gw.example.com".into(),
        nous_user_token: "tok".into(),
        managed_mode: true,
    };
    let b = FalImageGenBackend::from_managed(&cfg);
    assert_eq!(
        b.transport.submit_url("fal-ai/flux/dev"),
        "https://fal-queue.gw.example.com/run/fal-ai/flux/dev"
    );
    let (name, value) = b.transport.auth_header();
    assert_eq!(name, "Authorization");
    assert_eq!(value, "Bearer tok");
}

#[test]
fn direct_submit_url_uses_fal_run_root() {
    let b = FalImageGenBackend::new("k".into());
    assert_eq!(
        b.transport.submit_url("fal-ai/flux/dev"),
        "https://fal.run/fal-ai/flux/dev"
    );
    let (_, value) = b.transport.auth_header();
    assert_eq!(value, "Key k");
}

#[test]
fn with_model_path_overrides_default() {
    let b = FalImageGenBackend::new("k".into()).with_model_path("fal-ai/flux-pro");
    assert_eq!(b.model_path(), "fal-ai/flux-pro");
}

#[test]
fn fal_model_path_reads_env_and_config() {
    let _g = EnvScope::new();
    std::env::set_var("FAL_KEY", "direct-key");
    std::env::set_var("FAL_IMAGE_MODEL", "fal-ai/gpt-image-2");
    let b = FalImageGenBackend::from_env_or_managed().unwrap();
    assert_eq!(b.model_path(), "fal-ai/gpt-image-2");

    std::env::remove_var("FAL_IMAGE_MODEL");
    std::fs::write(
        hermes_config::paths::config_path(),
        "image_gen:\n  provider: fal\n  fal:\n    model: fal-ai/nano-banana-pro\n",
    )
    .expect("write config");
    let b = FalImageGenBackend::from_env_or_managed().unwrap();
    assert_eq!(b.model_path(), "fal-ai/nano-banana-pro");

    std::fs::write(
        hermes_config::paths::config_path(),
        "image_gen:\n  provider: fal\n  model: gpt-image-2-high\n",
    )
    .expect("write config");
    let b = FalImageGenBackend::from_env_or_managed().unwrap();
    assert_eq!(b.model_path(), DEFAULT_FAL_MODEL_PATH);
}

#[test]
fn fal_text_payload_uses_catalog_endpoint_and_supported_keys() {
    let backend = FalImageGenBackend::new("k".into()).with_model_path("fal-ai/gpt-image-2");
    let mut request = image_request("draw launch typography");
    request.size = Some("landscape".to_string());
    request.n = Some(2);

    let prepared = backend.prepare_request(&request).unwrap();
    assert_eq!(prepared.endpoint, "fal-ai/gpt-image-2");
    assert_eq!(prepared.modality, "text");
    assert_eq!(prepared.body["prompt"], "draw launch typography");
    assert_eq!(prepared.body["image_size"], "landscape_4_3");
    assert_eq!(prepared.body["quality"], "medium");
    assert_eq!(prepared.body["num_images"], 2);
    assert!(prepared.body.get("image_urls").is_none());
}

#[test]
fn fal_edit_payload_uses_edit_endpoint_and_clamps_references() {
    let backend = FalImageGenBackend::new("k".into()).with_model_path("fal-ai/nano-banana-pro");
    let mut request = image_request("replace the sign text");
    request.size = Some("portrait".to_string());
    request.image_url = Some("https://example.test/source.png".to_string());
    request.reference_image_urls = vec![
        "https://example.test/ref-a.png".to_string(),
        "https://example.test/ref-b.png".to_string(),
        "https://example.test/ref-c.png".to_string(),
    ];

    let prepared = backend.prepare_request(&request).unwrap();
    assert_eq!(prepared.endpoint, "fal-ai/nano-banana-pro/edit");
    assert_eq!(prepared.modality, "image");
    assert_eq!(prepared.source_image_count, 2);
    assert_eq!(prepared.body["prompt"], "replace the sign text");
    assert_eq!(prepared.body["aspect_ratio"], "9:16");
    assert_eq!(
        prepared.body["image_urls"],
        json!([
            "https://example.test/source.png",
            "https://example.test/ref-a.png"
        ])
    );
}

#[test]
fn fal_text_only_model_rejects_image_inputs() {
    let backend = FalImageGenBackend::new("k".into()).with_model_path("fal-ai/z-image/turbo");
    let mut request = image_request("edit source");
    request.image_url = Some("https://example.test/source.png".to_string());
    let err = backend.prepare_request(&request).unwrap_err();
    assert!(err.to_string().contains("not capable of image-to-image"));
}

#[test]
fn image_capabilities_reflect_fal_edit_support() {
    let edit_backend = FalImageGenBackend::new("k".into()).with_model_path("fal-ai/flux-2-pro");
    let caps = edit_backend.capabilities();
    assert_eq!(caps.provider.as_deref(), Some("FAL.ai"));
    assert!(caps.supports_image_input());
    assert_eq!(caps.max_reference_images, 9);

    let text_backend = FalImageGenBackend::new("k".into()).with_model_path("fal-ai/z-image/turbo");
    let caps = text_backend.capabilities();
    assert!(!caps.supports_image_input());
    assert_eq!(caps.max_reference_images, 0);
}

#[test]
fn empty_direct_key_falls_through_to_error_when_no_managed() {
    let _g = EnvScope::new();
    std::env::set_var("FAL_KEY", "  ");
    let err = FalImageGenBackend::from_env_or_managed().unwrap_err();
    assert!(err.to_string().contains("FAL_KEY"));
}

#[test]
fn selected_image_provider_reads_env_and_config() {
    let _g = EnvScope::new();
    std::env::set_var("HERMES_IMAGE_GEN_PROVIDER", "codex");
    assert_eq!(selected_image_provider(), Some("openai-codex"));

    std::env::remove_var("HERMES_IMAGE_GEN_PROVIDER");
    std::fs::write(
        hermes_config::paths::config_path(),
        "image_gen:\n  provider: openai-codex\n",
    )
    .expect("write config");
    assert_eq!(selected_image_provider(), Some("openai-codex"));

    std::fs::write(
        hermes_config::paths::config_path(),
        "image_gen:\n  backend: fal\n",
    )
    .expect("write config");
    assert_eq!(selected_image_provider(), Some("fal"));

    std::fs::write(
        hermes_config::paths::config_path(),
        "image_gen:\n  provider: openrouter\n",
    )
    .expect("write config");
    assert_eq!(selected_image_provider(), Some("openrouter"));

    std::env::set_var("HERMES_IMAGE_GEN_PROVIDER", "nous-portal");
    assert_eq!(selected_image_provider(), Some("nous"));

    std::env::set_var("HERMES_IMAGE_GEN_PROVIDER", "krea-ai");
    assert_eq!(selected_image_provider(), Some("krea"));

    std::env::set_var("HERMES_IMAGE_GEN_PROVIDER", "openai");
    assert_eq!(selected_image_provider(), Some("openai"));

    std::env::set_var("HERMES_IMAGE_GEN_PROVIDER", "grok-imagine");
    assert_eq!(selected_image_provider(), Some("xai"));
}

#[test]
fn krea_config_resolves_model_creativity_and_gateway_preference() {
    let _g = EnvScope::new();
    std::env::set_var("KREA_API_KEY", "krea-env-key");
    std::env::set_var("KREA_IMAGE_MODEL", "krea-2-large");
    std::env::set_var("KREA_IMAGE_CREATIVITY", "HIGH");
    let cfg = KreaImageGenConfig::direct(resolve_krea_api_key().unwrap());
    assert_eq!(cfg.transport_label(), "direct");
    assert_eq!(cfg.model(), "krea-2-large");
    assert_eq!(cfg.creativity(), "high");

    std::env::remove_var("KREA_IMAGE_MODEL");
    std::env::remove_var("KREA_IMAGE_CREATIVITY");
    std::fs::write(
        hermes_config::paths::config_path(),
        "image_gen:\n  provider: krea\n  krea:\n    model: krea-2-medium-turbo\n    creativity: raw\n    use_gateway: true\n",
    )
    .expect("write config");
    let cfg = KreaImageGenConfig::direct("direct".into());
    assert_eq!(cfg.model(), "krea-2-medium-turbo");
    assert_eq!(cfg.creativity(), "raw");
    assert!(krea_prefers_gateway());
}

#[test]
fn krea_payload_converts_reference_urls_to_style_objects() {
    let _g = EnvScope::new();
    let mut request = image_request("style-transfer pet");
    request.size = Some("portrait".to_string());
    request.image_url = Some("https://example.test/source.png".to_string());
    request.reference_image_urls = vec![
        "https://example.test/ref-a.png".to_string(),
        "https://example.test/ref-a.png".to_string(),
        "https://example.test/ref-b.png".to_string(),
    ];

    let refs = krea_style_reference_objects(&request);
    assert_eq!(
        refs,
        vec![
            json!({"url": "https://example.test/source.png", "strength": DEFAULT_KREA_STYLE_REFERENCE_STRENGTH}),
            json!({"url": "https://example.test/ref-a.png", "strength": DEFAULT_KREA_STYLE_REFERENCE_STRENGTH}),
            json!({"url": "https://example.test/ref-b.png", "strength": DEFAULT_KREA_STYLE_REFERENCE_STRENGTH}),
        ]
    );
    let payload = krea_submit_payload(
        request.prompt.as_str(),
        krea_aspect_from_tool_size(request.size.as_deref()),
        "medium",
        refs.as_slice(),
    );
    assert_eq!(payload["aspect_ratio"], "9:16");
    assert_eq!(payload["resolution"], DEFAULT_KREA_RESOLUTION);
    assert_eq!(
        payload["image_style_references"][0],
        json!({"url": "https://example.test/source.png", "strength": DEFAULT_KREA_STYLE_REFERENCE_STRENGTH})
    );
}

#[test]
fn openrouter_config_resolves_env_and_scoped_config() {
    let _g = EnvScope::new();
    std::env::set_var("OPENROUTER_API_KEY", "sk-or-env");
    std::env::set_var("OPENROUTER_IMAGE_MODEL", "black-forest-labs/flux.2-pro");
    let cfg = OpenRouterCompatImageGenConfig::from_env_or_config(
        OpenRouterCompatImageProviderKind::OpenRouter,
    )
    .unwrap();
    assert_eq!(
        cfg.provider(),
        OpenRouterCompatImageProviderKind::OpenRouter
    );
    assert_eq!(cfg.model(), "black-forest-labs/flux.2-pro");
    assert_eq!(cfg.base_url(), DEFAULT_OPENROUTER_IMAGE_BASE_URL);

    std::env::remove_var("OPENROUTER_IMAGE_MODEL");
    std::env::remove_var("OPENROUTER_API_KEY");
    std::fs::write(
        hermes_config::paths::config_path(),
        "image_gen:\n  provider: openrouter\n  openrouter:\n    model: google/gemini-3.1-flash-image-preview\n    api_key: config-openrouter-key\n    base_url: https://or.example/v1/\n",
    )
    .expect("write config");
    let cfg = OpenRouterCompatImageGenConfig::from_env_or_config(
        OpenRouterCompatImageProviderKind::OpenRouter,
    )
    .unwrap();
    assert_eq!(cfg.model(), "google/gemini-3.1-flash-image-preview");
    assert_eq!(cfg.base_url(), "https://or.example/v1");
}

#[test]
fn nous_config_reads_auth_store_agent_key_and_inference_base_url() {
    let g = EnvScope::new();
    let auth_path = g.auth_path();
    std::env::set_var("HERMES_AUTH_FILE", &auth_path);
    std::fs::write(
        &auth_path,
        r#"{
          "version": 1,
          "providers": {
            "nous": {
              "access_token": "portal-access",
              "agent_key": "nous-agent-key",
              "inference_base_url": "https://inference.nousresearch.com/v1/"
            }
          }
        }"#,
    )
    .expect("write auth");

    let cfg =
        OpenRouterCompatImageGenConfig::from_env_or_config(OpenRouterCompatImageProviderKind::Nous)
            .unwrap();
    assert_eq!(cfg.provider(), OpenRouterCompatImageProviderKind::Nous);
    assert_eq!(cfg.base_url(), "https://inference.nousresearch.com/v1");
    assert_eq!(cfg.api_key.as_deref(), Some("nous-agent-key"));
}

#[test]
fn openrouter_reference_images_inline_local_files_and_clamp() {
    let g = EnvScope::new();
    let ref_a = g.home.join("base.png");
    let ref_b = g.home.join("ref-b.png");
    let ref_c = g.home.join("ref-c.png");
    let ref_d = g.home.join("ref-d.png");
    std::fs::write(&ref_a, b"\x89PNG\r\n").expect("write ref a");
    std::fs::write(&ref_b, b"b").expect("write ref b");
    std::fs::write(&ref_c, b"c").expect("write ref c");
    std::fs::write(&ref_d, b"d").expect("write ref d");
    let mut request = image_request("same pet sprite");
    request.image_url = Some(ref_a.to_string_lossy().to_string());
    request.reference_image_urls = vec![
        ref_b.to_string_lossy().to_string(),
        ref_c.to_string_lossy().to_string(),
        ref_d.to_string_lossy().to_string(),
    ];

    let parts = openrouter_compat_reference_image_parts(&request).unwrap();
    assert_eq!(parts.len(), OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES);
    assert!(parts[0].starts_with("data:image/png;base64,"));
    assert_eq!(
        STANDARD
            .decode(parts[0].split_once(',').unwrap().1)
            .unwrap(),
        b"\x89PNG\r\n"
    );

    let payload = openrouter_compat_chat_payload(
        DEFAULT_OPENROUTER_COMPAT_IMAGE_MODEL,
        "same pet sprite",
        openrouter_compat_aspect_from_tool_size(Some("portrait")),
        parts.as_slice(),
    );
    assert_eq!(payload["modalities"], json!(["image", "text"]));
    assert_eq!(payload["image_config"]["aspect_ratio"], "9:16");
    assert_eq!(
        payload["messages"][0]["content"][0],
        json!({"type": "text", "text": "same pet sprite"})
    );
    assert_eq!(
        payload["messages"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "image_url")
            .count(),
        OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES
    );
}

#[test]
fn openrouter_capabilities_advertise_reference_grounding() {
    let backend = OpenRouterCompatImageGenBackend::unconfigured(
        OpenRouterCompatImageProviderKind::OpenRouter,
    );
    let caps = backend.capabilities();
    assert_eq!(caps.provider.as_deref(), Some("openrouter"));
    assert_eq!(
        caps.model.as_deref(),
        Some(DEFAULT_OPENROUTER_COMPAT_IMAGE_MODEL)
    );
    assert!(caps.supports_image_input());
    assert_eq!(
        caps.max_reference_images,
        OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES
    );

    let runtime: ImageGenRuntimeBackend = backend.into();
    assert_eq!(runtime.provider_label(), "openrouter");
    assert_eq!(runtime.required_env_vars(), vec!["OPENROUTER_API_KEY"]);
}

#[test]
fn openrouter_timeout_budget_matches_slow_quality_first_models() {
    assert_eq!(OPENROUTER_COMPAT_TIMEOUT_SECS, 300);
}

#[test]
fn codex_image_model_precedence_matches_plugin_contract() {
    let _g = EnvScope::new();
    std::fs::write(
        hermes_config::paths::config_path(),
        "image_gen:\n  model: gpt-image-2-low\n  openai-codex:\n    model: gpt-image-2-high\n",
    )
    .expect("write config");
    let tier = resolve_codex_image_tier();
    assert_eq!(tier.id, "gpt-image-2-high");
    assert_eq!(tier.quality, "high");

    std::env::set_var("OPENAI_IMAGE_MODEL", "gpt-image-2-low");
    let tier = resolve_codex_image_tier();
    assert_eq!(tier.id, "gpt-image-2-low");
    assert_eq!(tier.quality, "low");

    std::env::set_var("OPENAI_IMAGE_MODEL", "bogus");
    std::fs::write(hermes_config::paths::config_path(), "image_gen: {}\n").expect("write config");
    let tier = resolve_codex_image_tier();
    assert_eq!(tier.id, DEFAULT_CODEX_IMAGE_MODEL);
    assert_eq!(tier.quality, "medium");
}

#[test]
fn codex_image_auth_reads_hermes_auth_store() {
    let g = EnvScope::new();
    let auth_path = g.auth_path();
    std::env::set_var("HERMES_AUTH_FILE", &auth_path);
    std::fs::write(
        &auth_path,
        r#"{
          "active_provider": "openai-codex",
          "providers": {
            "openai-codex": {
              "tokens": {"access_token": "codex-access-token"},
              "base_url": "https://chatgpt.example/backend-api/codex"
            }
          }
        }"#,
    )
    .expect("write auth");

    let auth = codex_image_auth_from_env_or_store();
    assert_eq!(auth.access_token.as_deref(), Some("codex-access-token"));
    assert_eq!(
        auth.base_url.as_deref(),
        Some("https://chatgpt.example/backend-api/codex")
    );
    let backend = OpenAICodexImageGenBackend::from_env_or_auth_store().unwrap();
    assert_eq!(backend.config().tier_id(), DEFAULT_CODEX_IMAGE_MODEL);
    assert_eq!(backend.config().quality(), "medium");
    assert_eq!(
        backend.config().base_url,
        "https://chatgpt.example/backend-api/codex"
    );
}

#[test]
fn codex_cloudflare_headers_extract_chatgpt_account_id() {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-image-123"
            }
        }))
        .unwrap(),
    );
    let token = format!("{header}.{payload}.sig");
    let headers = codex_cloudflare_headers(Some(token.as_str()));
    assert!(headers
        .iter()
        .any(|(name, value)| name == "originator" && value == "codex_cli_rs"));
    assert!(headers
        .iter()
        .any(|(name, value)| name == "ChatGPT-Account-ID" && value == "acct-image-123"));
}

#[test]
fn codex_image_sse_parser_keeps_latest_partial_or_result() {
    let raw = concat!(
        "event: response.image_generation_call.partial_image\n",
        "data: {\"partial_image_b64\":\"first\"}\n\n",
        "data: {\"output\":[{\"type\":\"image_generation_call\",\"result\":\"final\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let image = collect_codex_image_b64_from_sse(raw).unwrap();
    assert_eq!(image.as_deref(), Some("final"));
}

#[tokio::test]
async fn codex_image_generate_posts_responses_and_saves_png() {
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _g = EnvScope::new();
    let server = MockServer::start().await;
    std::env::set_var("HERMES_OPENAI_CODEX_API_KEY", "codex-token");
    std::env::set_var("HERMES_OPENAI_CODEX_BASE_URL", server.uri());
    std::env::set_var("OPENAI_IMAGE_MODEL", "gpt-image-2-high");

    let png_b64 = STANDARD.encode(b"\x89PNG\r\n\x1a\n");
    let sse = format!(
        "event: response.image_generation_call.completed\n\
         data: {{\"type\":\"image_generation_call\",\"result\":\"{png_b64}\"}}\n\n\
         data: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(header("Authorization", "Bearer codex-token"))
        .and(header("Accept", "text/event-stream"))
        .and(header("originator", "codex_cli_rs"))
        .and(body_partial_json(json!({
            "model": DEFAULT_CODEX_IMAGE_CHAT_MODEL,
            "tools": [{
                "type": "image_generation",
                "model": CODEX_IMAGE_API_MODEL,
                "size": "1536x1024",
                "quality": "high",
                "output_format": "png",
                "background": "opaque",
                "partial_images": 1
            }],
            "stream": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string(sse))
        .mount(&server)
        .await;

    let backend = OpenAICodexImageGenBackend::from_env_or_auth_store().unwrap();
    let output = backend
        .generate(ImageGenerateRequest {
            prompt: "paint a launch".to_string(),
            size: Some("landscape".to_string()),
            style: None,
            n: None,
            image_url: None,
            reference_image_urls: Vec::new(),
        })
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(payload["success"], true);
    assert_eq!(payload["provider"], "openai-codex");
    assert_eq!(payload["model"], "gpt-image-2-high");
    assert_eq!(payload["quality"], "high");
    let image = payload["image"].as_str().expect("image path");
    assert!(image.contains("cache/images/openai_codex_gpt_image_2_high_"));
    assert_eq!(std::fs::read(image).unwrap(), b"\x89PNG\r\n\x1a\n");
}

#[tokio::test]
async fn openai_image_generate_posts_images_api_and_saves_b64() {
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _g = EnvScope::new();
    let server = MockServer::start().await;
    std::env::set_var("OPENAI_API_KEY", "sk-openai-test");
    std::env::set_var("OPENAI_IMAGE_BASE_URL", server.uri());
    std::env::set_var("OPENAI_IMAGE_MODEL", "gpt-image-2-low");

    let image_b64 = STANDARD.encode(b"openai-image-data");
    Mock::given(method("POST"))
        .and(path("/images/generations"))
        .and(header("Authorization", "Bearer sk-openai-test"))
        .and(body_partial_json(json!({
            "model": OPENAI_IMAGE_API_MODEL,
            "prompt": "paint a direct provider",
            "size": "1024x1024",
            "n": 1,
            "quality": "low"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "b64_json": image_b64,
                "revised_prompt": "paint a direct provider, polished"
            }]
        })))
        .mount(&server)
        .await;

    let backend = OpenAiImageGenBackend::from_env_or_config().unwrap();
    assert_eq!(backend.config().model(), "gpt-image-2-low");
    assert_eq!(backend.config().quality(), "low");

    let output = backend
        .generate(ImageGenerateRequest {
            prompt: "paint a direct provider".to_string(),
            size: Some("square".to_string()),
            style: None,
            n: None,
            image_url: None,
            reference_image_urls: Vec::new(),
        })
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(payload["success"], true);
    assert_eq!(payload["provider"], "openai");
    assert_eq!(payload["transport"], "direct");
    assert_eq!(payload["model"], "gpt-image-2-low");
    assert_eq!(payload["api_model"], OPENAI_IMAGE_API_MODEL);
    assert_eq!(payload["quality"], "low");
    assert_eq!(
        payload["revised_prompt"],
        "paint a direct provider, polished"
    );
    let image = payload["image"].as_str().expect("image path");
    assert!(image.contains("cache/images/openai_gen_"));
    assert_eq!(std::fs::read(image).unwrap(), b"openai-image-data");

    let runtime: ImageGenRuntimeBackend = backend.into();
    assert_eq!(runtime.provider_label(), "openai");
    assert_eq!(runtime.required_env_vars(), vec!["OPENAI_API_KEY"]);
}

#[tokio::test]
async fn openai_image_edit_uses_multipart_images_endpoint() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _g = EnvScope::new();
    let server = MockServer::start().await;
    std::env::set_var("OPENAI_API_KEY", "sk-openai-edit");
    std::env::set_var("OPENAI_IMAGE_BASE_URL", server.uri());

    let image_b64 = STANDARD.encode(b"openai-edit-data");
    Mock::given(method("POST"))
        .and(path("/images/edits"))
        .and(header("Authorization", "Bearer sk-openai-edit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"b64_json": image_b64}]
        })))
        .mount(&server)
        .await;

    let source = STANDARD.encode(b"source-image");
    let backend = OpenAiImageGenBackend::from_env_or_config().unwrap();
    let output = backend
        .generate(ImageGenerateRequest {
            prompt: "edit the source".to_string(),
            size: Some("portrait".to_string()),
            style: None,
            n: None,
            image_url: Some(format!("data:image/png;base64,{source}")),
            reference_image_urls: Vec::new(),
        })
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(payload["provider"], "openai");
    assert_eq!(payload["modality"], "image");
    assert_eq!(payload["size"], "1024x1536");
    assert_eq!(payload["source_images"], 1);
    let image = payload["image"].as_str().expect("image path");
    assert_eq!(std::fs::read(image).unwrap(), b"openai-edit-data");
}

#[tokio::test]
async fn openrouter_image_generate_posts_chat_completions_and_saves_data_uri() {
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _g = EnvScope::new();
    let server = MockServer::start().await;
    std::env::set_var("OPENROUTER_API_KEY", "sk-or-test");
    std::env::set_var("OPENROUTER_IMAGE_BASE_URL", server.uri());
    std::env::set_var("OPENROUTER_IMAGE_MODEL", "google/gemini-2.5-flash-image");

    let image_b64 = STANDARD.encode(b"test-image-data");
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("Authorization", "Bearer sk-or-test"))
        .and(header("HTTP-Referer", OPENROUTER_COMPAT_HTTP_REFERER))
        .and(header("X-Title", OPENROUTER_COMPAT_X_TITLE))
        .and(body_partial_json(json!({
            "model": "google/gemini-2.5-flash-image",
            "modalities": ["image", "text"],
            "image_config": {"aspect_ratio": "1:1"},
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "a tiny rust crab pet"}]
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "images": [{
                        "type": "image_url",
                        "image_url": {"url": format!("data:image/png;base64,{image_b64}")}
                    }]
                }
            }]
        })))
        .mount(&server)
        .await;

    let backend = OpenRouterCompatImageGenBackend::from_env_or_config(
        OpenRouterCompatImageProviderKind::OpenRouter,
    )
    .unwrap();
    let output = backend
        .generate(ImageGenerateRequest {
            prompt: "a tiny rust crab pet".to_string(),
            size: Some("square".to_string()),
            style: None,
            n: None,
            image_url: None,
            reference_image_urls: Vec::new(),
        })
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(payload["success"], true);
    assert_eq!(payload["provider"], "openrouter");
    assert_eq!(payload["transport"], "openrouter-compatible");
    assert_eq!(payload["model"], "google/gemini-2.5-flash-image");
    assert_eq!(payload["aspect_ratio"], "1:1");
    let image = payload["image"].as_str().expect("image path");
    assert!(image.contains("cache/images/openrouter_gen_"));
    assert_eq!(std::fs::read(image).unwrap(), b"test-image-data");
}

#[tokio::test]
async fn xai_image_generate_posts_json_and_saves_b64() {
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _g = EnvScope::new();
    let server = MockServer::start().await;
    std::env::set_var("XAI_API_KEY", "xai-test-key");
    std::env::set_var("XAI_IMAGE_BASE_URL", server.uri());
    std::env::set_var("XAI_IMAGE_RESOLUTION", "2k");

    let image_b64 = STANDARD.encode(b"xai-image-data");
    Mock::given(method("POST"))
        .and(path("/images/generations"))
        .and(header("Authorization", "Bearer xai-test-key"))
        .and(header("User-Agent", "hermes-agent/image_gen"))
        .and(body_partial_json(json!({
            "model": DEFAULT_XAI_IMAGE_MODEL,
            "prompt": "grok a skyline",
            "aspect_ratio": "16:9",
            "resolution": "2k"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"b64_json": image_b64}],
            "usage": {"input_tokens": 7}
        })))
        .mount(&server)
        .await;

    let backend = XaiImageGenBackend::from_env_or_auth_store().unwrap();
    assert_eq!(backend.config().model(), DEFAULT_XAI_IMAGE_MODEL);
    assert_eq!(backend.config().resolution(), "2k");
    let mut request = image_request("grok a skyline");
    request.size = Some("landscape".to_string());
    let output = backend.generate(request).await.unwrap();
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(payload["success"], true);
    assert_eq!(payload["provider"], "xai");
    assert_eq!(payload["transport"], "direct");
    assert_eq!(payload["model"], DEFAULT_XAI_IMAGE_MODEL);
    assert_eq!(payload["aspect_ratio"], "landscape");
    assert_eq!(payload["xai_aspect_ratio"], "16:9");
    assert_eq!(payload["resolution"], "2k");
    assert_eq!(payload["usage"]["input_tokens"], 7);
    let image = payload["image"].as_str().expect("image path");
    assert!(image.contains("cache/images/xai_gen_"));
    assert_eq!(std::fs::read(image).unwrap(), b"xai-image-data");

    let runtime: ImageGenRuntimeBackend = backend.into();
    assert_eq!(runtime.provider_label(), "xai");
    assert_eq!(runtime.required_env_vars(), vec!["XAI_API_KEY"]);
}

#[tokio::test]
async fn xai_image_edit_uses_quality_model_and_json_image_field() {
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _g = EnvScope::new();
    let server = MockServer::start().await;
    std::env::set_var("XAI_API_KEY", "xai-edit-key");
    std::env::set_var("XAI_IMAGE_BASE_URL", server.uri());
    let image_b64 = STANDARD.encode(b"xai-edit-image");
    let source_b64 = STANDARD.encode(b"source-image");
    let source_uri = format!("data:image/png;base64,{source_b64}");

    Mock::given(method("POST"))
        .and(path("/images/edits"))
        .and(header("Authorization", "Bearer xai-edit-key"))
        .and(body_partial_json(json!({
            "model": DEFAULT_XAI_IMAGE_EDIT_MODEL,
            "prompt": "make it cinematic",
            "image": {"url": source_uri, "type": "image_url"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "b64_json": image_b64,
                "file_output": {"public_url_error": "disabled"}
            }]
        })))
        .mount(&server)
        .await;

    let backend = XaiImageGenBackend::from_env_or_auth_store().unwrap();
    let mut request = image_request("make it cinematic");
    request.image_url = Some(source_uri);
    let output = backend.generate(request).await.unwrap();
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(payload["provider"], "xai");
    assert_eq!(payload["model"], DEFAULT_XAI_IMAGE_EDIT_MODEL);
    assert_eq!(payload["modality"], "image");
    assert_eq!(payload["source_images"], 1);
    assert_eq!(payload["file_output"]["public_url_error"], "disabled");
    let image = payload["image"].as_str().expect("image path");
    assert_eq!(std::fs::read(image).unwrap(), b"xai-edit-image");
}

#[tokio::test]
async fn nous_image_generate_posts_to_resolved_base_url_and_downloads_remote_image() {
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _g = EnvScope::new();
    let server = MockServer::start().await;
    std::env::set_var("NOUS_API_KEY", "nous-key");
    std::env::set_var("NOUS_IMAGE_BASE_URL", server.uri());

    Mock::given(method("GET"))
        .and(path("/generated.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(b"downloaded-image"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("Authorization", "Bearer nous-key"))
        .and(body_partial_json(json!({
            "model": DEFAULT_OPENROUTER_COMPAT_IMAGE_MODEL,
            "modalities": ["image", "text"],
            "image_config": {"aspect_ratio": "16:9"},
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "images": [{
                        "image_url": {"url": format!("{}/generated.png", server.uri())}
                    }]
                }
            }]
        })))
        .mount(&server)
        .await;

    let backend = OpenRouterCompatImageGenBackend::from_env_or_config(
        OpenRouterCompatImageProviderKind::Nous,
    )
    .unwrap();
    let mut request = image_request("a portal pet");
    request.size = Some("landscape".to_string());
    let output = backend.generate(request).await.unwrap();
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(payload["success"], true);
    assert_eq!(payload["provider"], "nous");
    assert_eq!(payload["aspect_ratio"], "16:9");
    let image = payload["image"].as_str().expect("image path");
    assert!(image.contains("cache/images/nous_gen_"));
    assert_eq!(std::fs::read(image).unwrap(), b"downloaded-image");
}

#[tokio::test]
async fn krea_image_generate_submits_polls_and_saves_data_uri() {
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _g = EnvScope::new();
    let server = MockServer::start().await;
    let image_b64 = STANDARD.encode(b"krea-image-data");

    Mock::given(method("POST"))
        .and(path("/generate/image/krea/krea-2/large"))
        .and(header("Authorization", "Bearer krea-test-key"))
        .and(header("Content-Type", "application/json"))
        .and(body_partial_json(json!({
            "prompt": "a cinematic lamp",
            "aspect_ratio": "1:1",
            "resolution": DEFAULT_KREA_RESOLUTION,
            "creativity": "medium",
            "image_style_references": [{
                "url": "https://example.test/source.png",
                "strength": DEFAULT_KREA_STYLE_REFERENCE_STRENGTH
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "job_id": "job-krea-123",
            "status": "queued",
            "result": null
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/jobs/job-krea-123"))
        .and(header("Authorization", "Bearer krea-test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "job_id": "job-krea-123",
            "status": "completed",
            "completed_at": "2026-05-27T00:00:30Z",
            "result": {"urls": [format!("data:image/png;base64,{image_b64}")]}
        })))
        .mount(&server)
        .await;

    let cfg = KreaImageGenConfig::direct("krea-test-key".into())
        .with_base_url(server.uri())
        .with_model("krea-2-large")
        .with_poll_timing(Duration::ZERO, Duration::ZERO, Duration::from_secs(5));
    let backend = KreaImageGenBackend::from_config(cfg);
    let mut request = image_request("a cinematic lamp");
    request.size = Some("square".to_string());
    request.image_url = Some("https://example.test/source.png".to_string());

    let output = backend.generate(request).await.unwrap();
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(payload["success"], true);
    assert_eq!(payload["provider"], "krea");
    assert_eq!(payload["transport"], "direct");
    assert_eq!(payload["model"], "krea-2-large");
    assert_eq!(payload["aspect_ratio"], "square");
    assert_eq!(payload["krea_aspect_ratio"], "1:1");
    assert_eq!(payload["modality"], "image");
    assert_eq!(payload["source_images"], 1);
    let image = payload["image"].as_str().expect("image path");
    assert!(image.contains("cache/images/krea_gen_"));
    assert_eq!(std::fs::read(image).unwrap(), b"krea-image-data");
}

#[tokio::test]
async fn krea_managed_uses_gateway_token_and_idempotency_header() {
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _g = EnvScope::new();
    let server = MockServer::start().await;
    let image_b64 = STANDARD.encode(b"managed-krea-image");

    Mock::given(method("POST"))
        .and(path("/generate/image/krea/krea-2/medium"))
        .and(header("Authorization", "Bearer nous-krea-token"))
        .and(body_partial_json(json!({
            "prompt": "managed krea",
            "aspect_ratio": "16:9"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "job_id": "job-managed",
            "status": "queued"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/jobs/job-managed"))
        .and(header("Authorization", "Bearer nous-krea-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "job_id": "job-managed",
            "status": "completed",
            "result": {"urls": [format!("data:image/png;base64,{image_b64}")]}
        })))
        .mount(&server)
        .await;

    let managed = ManagedToolGatewayConfig {
        vendor: "krea".into(),
        gateway_origin: server.uri(),
        nous_user_token: "nous-krea-token".into(),
        managed_mode: true,
    };
    let cfg = KreaImageGenConfig::managed(&managed).with_poll_timing(
        Duration::ZERO,
        Duration::ZERO,
        Duration::from_secs(5),
    );
    let backend = KreaImageGenBackend::from_config(cfg);
    let mut request = image_request("managed krea");
    request.size = Some("landscape".to_string());

    let output = backend.generate(request).await.unwrap();
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(payload["provider"], "krea");
    assert_eq!(payload["transport"], "managed");
    assert_eq!(payload["krea_aspect_ratio"], "16:9");

    let requests = server.received_requests().await.unwrap();
    let submit = requests
        .iter()
        .find(|request| request.method.as_str() == "POST")
        .expect("submit request");
    assert!(submit.headers.contains_key("x-idempotency-key"));
}

#[tokio::test]
async fn krea_poll_retries_transient_status_and_fails_fast_on_auth() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _g = EnvScope::new();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jobs/retry-job"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error": "busy"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/jobs/retry-job"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "completed",
            "result": {"urls": ["https://krea.cdn/done.png"]}
        })))
        .mount(&server)
        .await;
    let cfg = KreaImageGenConfig::direct("krea-test-key".into())
        .with_base_url(server.uri())
        .with_poll_timing(Duration::ZERO, Duration::ZERO, Duration::from_secs(5));
    let backend = KreaImageGenBackend::from_config(cfg);
    let job = backend.poll_krea_job("retry-job").await.unwrap();
    assert_eq!(
        extract_krea_result_image_url(&job).as_deref(),
        Some("https://krea.cdn/done.png")
    );

    let auth_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jobs/auth-job"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "invalid key"}
        })))
        .mount(&auth_server)
        .await;
    let auth_cfg = KreaImageGenConfig::direct("krea-test-key".into())
        .with_base_url(auth_server.uri())
        .with_poll_timing(Duration::ZERO, Duration::ZERO, Duration::from_secs(5));
    let auth_backend = KreaImageGenBackend::from_config(auth_cfg);
    let err = auth_backend.poll_krea_job("auth-job").await.unwrap_err();
    assert!(err.to_string().contains("401"));
    assert!(err.to_string().contains("invalid key"));
    assert_eq!(auth_server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn openrouter_image_generate_errors_when_response_has_no_images() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _g = EnvScope::new();
    let server = MockServer::start().await;
    std::env::set_var("OPENROUTER_API_KEY", "sk-or-test");
    std::env::set_var("OPENROUTER_IMAGE_BASE_URL", server.uri());

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "no image"}}]
        })))
        .mount(&server)
        .await;

    let backend = OpenRouterCompatImageGenBackend::from_env_or_config(
        OpenRouterCompatImageProviderKind::OpenRouter,
    )
    .unwrap();
    let err = backend
        .generate(image_request("missing output"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("returned no image"));
}

#[tokio::test]
async fn codex_image_generate_rejects_image_inputs() {
    let mut request = image_request("edit source");
    request.image_url = Some("https://example.test/source.png".to_string());
    let err = OpenAICodexImageGenBackend::unconfigured()
        .generate(request)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("text-to-image only"));
}
