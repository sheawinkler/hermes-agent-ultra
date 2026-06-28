#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;
    use serde_json::json;

    struct EnvScope {
        _tmp: tempfile::TempDir,
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvScope {
        fn new() -> Self {
            let g = test_lock::lock();
            let tmp = tempfile::tempdir().unwrap();
            let keys = [
                "HERMES_HOME",
                "HOME",
                "HERMES_AUTH_FILE",
                "FAL_KEY",
                "FAL_VIDEO_MODEL",
                "FAL_VIDEO_TIMEOUT_SECONDS",
                "FAL_VIDEO_POLL_INTERVAL_SECONDS",
                "HERMES_VIDEO_GEN_BACKEND",
                "VIDEO_GEN_BACKEND",
                "HERMES_XAI_API_KEY",
                "XAI_API_KEY",
                "HERMES_XAI_BASE_URL",
                "XAI_BASE_URL",
                "XAI_VIDEO_TIMEOUT_SECONDS",
                "XAI_VIDEO_POLL_INTERVAL_SECONDS",
                "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
                "TOOL_GATEWAY_USER_TOKEN",
                "TOOL_GATEWAY_DOMAIN",
                "TOOL_GATEWAY_SCHEME",
            ];
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in keys {
                std::env::remove_var(k);
            }
            std::env::set_var("HERMES_HOME", tmp.path());
            Self {
                _tmp: tmp,
                original,
                _g: g,
            }
        }
    }

    impl Drop for EnvScope {
        fn drop(&mut self) {
            for (key, value) in &self.original {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn request(model: Option<&str>) -> VideoGenerateRequest {
        VideoGenerateRequest {
            prompt: "make a trailer".into(),
            model: model.map(ToOwned::to_owned),
            model_explicit: model.is_some(),
            image_url: None,
            reference_image_urls: Vec::new(),
            duration: None,
            aspect_ratio: "16:9".into(),
            resolution: "720p".into(),
            negative_prompt: None,
            audio: None,
            seed: None,
        }
    }

    #[test]
    fn resolve_family_prefers_explicit_then_env_then_default() {
        let _env = EnvScope::new();
        std::env::set_var("FAL_VIDEO_MODEL", "veo3.1");
        assert_eq!(resolve_family(Some("seedance-2.0")).id, "seedance-2.0");
        assert_eq!(resolve_family(None).id, "veo3.1");
        std::env::set_var("FAL_VIDEO_MODEL", "not-real");
        assert_eq!(resolve_family(None).id, DEFAULT_FAL_VIDEO_MODEL);
    }

    #[test]
    fn resolve_family_reads_config_candidates() {
        let _env = EnvScope::new();
        let config = hermes_config::config_path();
        std::fs::write(
            config,
            "video_gen:\n  fal:\n    model: kling-v3-4k\n  model: veo3.1\n",
        )
        .unwrap();
        assert_eq!(resolve_family(None).id, "kling-v3-4k");
    }

    #[test]
    fn payload_clamps_range_duration_and_uses_kling_start_image_key() {
        let family = family_by_id("kling-v3-4k").unwrap();
        let mut req = request(Some("kling-v3-4k"));
        req.image_url = Some("https://example.com/start.png".into());
        req.duration = Some(99);
        req.resolution = "1080p".into();
        req.audio = Some(true);
        let payload = build_payload(family, &req).payload;
        assert_eq!(
            payload.get("start_image_url"),
            Some(&json!("https://example.com/start.png"))
        );
        assert_eq!(payload.get("duration"), Some(&json!("15")));
        assert_eq!(payload.get("generate_audio"), Some(&json!(true)));
        assert!(payload.get("resolution").is_none());
    }

    #[test]
    fn payload_snaps_enum_duration_and_drops_unsupported_negative_prompt() {
        let family = family_by_id("seedance-2.0").unwrap();
        let mut req = request(Some("seedance-2.0"));
        req.duration = Some(2);
        req.negative_prompt = Some("low quality".into());
        req.aspect_ratio = "21:9".into();
        req.resolution = "480p".into();
        let meta = build_payload(family, &req);
        assert_eq!(meta.payload.get("duration"), Some(&json!("4")));
        assert_eq!(meta.payload.get("aspect_ratio"), Some(&json!("21:9")));
        assert_eq!(meta.payload.get("resolution"), Some(&json!("480p")));
        assert!(meta.payload.get("negative_prompt").is_none());
    }

    #[test]
    fn payload_uses_nearest_veo_duration() {
        let family = family_by_id("veo3.1").unwrap();
        let mut req = request(Some("veo3.1"));
        req.duration = Some(5);
        let meta = build_payload(family, &req);
        assert_eq!(meta.payload.get("duration"), Some(&json!("4")));
    }

    #[test]
    fn transport_urls_and_auth_match_direct_and_managed_modes() {
        let direct = FalVideoGenBackend::new("fal-key".into());
        assert_eq!(
            direct
                .transport
                .submit_url("fal-ai/pixverse/v6/text-to-video")
                .unwrap(),
            "https://queue.fal.run/fal-ai/pixverse/v6/text-to-video"
        );
        assert_eq!(direct.transport.auth_header().unwrap().1, "Key fal-key");

        let cfg = ManagedToolGatewayConfig {
            vendor: "fal-queue".into(),
            gateway_origin: "https://fal-queue.gw.example.com".into(),
            nous_user_token: "tok".into(),
            managed_mode: true,
        };
        let managed = FalVideoGenBackend::from_managed(&cfg);
        assert_eq!(
            managed
                .transport
                .submit_url("fal-ai/pixverse/v6/text-to-video")
                .unwrap(),
            "https://fal-queue.gw.example.com/run/fal-ai/pixverse/v6/text-to-video"
        );
        assert_eq!(managed.transport.auth_header().unwrap().1, "Bearer tok");
    }

    #[test]
    fn from_env_or_managed_prefers_direct_and_supports_managed() {
        let _env = EnvScope::new();
        std::env::set_var("FAL_KEY", "direct-key");
        assert_eq!(
            FalVideoGenBackend::from_env_or_managed()
                .unwrap()
                .transport_label(),
            "direct"
        );
        std::env::remove_var("FAL_KEY");
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "nous-token");
        assert_eq!(
            FalVideoGenBackend::from_env_or_managed()
                .unwrap()
                .transport_label(),
            "managed"
        );
    }

    #[tokio::test]
    async fn unconfigured_backend_errors_before_network() {
        let backend = FalVideoGenBackend::unconfigured();
        let err = backend.generate_video(request(None)).await.unwrap_err();
        assert!(err.to_string().contains("FAL_KEY"));
    }

    #[test]
    fn extract_video_handles_wrapped_and_string_shapes() {
        assert_eq!(
            extract_video(&json!({"data":{"video":{"url":"https://cdn.example/v.mp4","file_size":42,"content_type":"video/mp4"}}}))
                .unwrap(),
            VideoArtifact {
                url: "https://cdn.example/v.mp4".into(),
                file_size: Some(42),
                content_type: Some("video/mp4".into()),
            }
        );
        assert_eq!(
            extract_video(&json!({"video":"https://cdn.example/v.mp4"}))
                .unwrap()
                .url,
            "https://cdn.example/v.mp4"
        );
    }

    #[test]
    fn xai_routes_default_models_by_modality() {
        assert_eq!(
            resolve_xai_model_for_modality(Some(DEFAULT_XAI_TEXT_TO_VIDEO_MODEL), "text", false),
            DEFAULT_XAI_TEXT_TO_VIDEO_MODEL
        );
        assert_eq!(
            resolve_xai_model_for_modality(Some(DEFAULT_XAI_TEXT_TO_VIDEO_MODEL), "image", false),
            DEFAULT_XAI_IMAGE_TO_VIDEO_MODEL
        );
        assert_eq!(
            resolve_xai_model_for_modality(Some(DEFAULT_XAI_IMAGE_TO_VIDEO_MODEL), "text", false),
            DEFAULT_XAI_TEXT_TO_VIDEO_MODEL
        );
        assert_eq!(
            resolve_xai_model_for_modality(Some(DEFAULT_XAI_IMAGE_TO_VIDEO_MODEL), "text", true),
            DEFAULT_XAI_IMAGE_TO_VIDEO_MODEL
        );
    }

    #[test]
    fn xai_payload_normalizes_references_duration_resolution_and_aspect() {
        let mut req = request(None);
        req.reference_image_urls = vec!["https://example.com/a.png".into()];
        req.duration = Some(15);
        req.aspect_ratio = "not-valid".into();
        req.resolution = "1080p".into();

        let meta = build_xai_payload(&req).unwrap();
        assert_eq!(meta.model, DEFAULT_XAI_IMAGE_TO_VIDEO_MODEL);
        assert_eq!(meta.modality, "image");
        assert_eq!(meta.duration, 10);
        assert_eq!(meta.aspect_ratio, DEFAULT_XAI_ASPECT_RATIO);
        assert_eq!(meta.resolution, DEFAULT_XAI_RESOLUTION);
        assert_eq!(
            meta.payload.get("reference_images"),
            Some(&json!([
                {"url": "https://example.com/a.png"}
            ]))
        );
    }

    #[test]
    fn xai_payload_rejects_conflicting_or_too_many_image_inputs() {
        let mut req = request(None);
        req.image_url = Some("https://example.com/start.png".into());
        req.reference_image_urls = vec!["https://example.com/a.png".into()];
        assert!(build_xai_payload(&req)
            .unwrap_err()
            .to_string()
            .contains("cannot be combined"));

        let mut req = request(None);
        req.reference_image_urls = (0..=XAI_MAX_REFERENCE_IMAGES)
            .map(|idx| format!("https://example.com/{idx}.png"))
            .collect();
        assert!(build_xai_payload(&req)
            .unwrap_err()
            .to_string()
            .contains("at most"));
    }

    #[test]
    fn xai_local_image_paths_are_encoded_as_data_urls() {
        let _env = EnvScope::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tiny.png");
        std::fs::write(&path, b"png-bytes").unwrap();

        let encoded = image_ref_to_xai_url(path.to_str().unwrap());
        assert!(encoded.starts_with("data:image/png;base64,"));
        assert!(encoded.ends_with("cG5nLWJ5dGVz"));
    }

    #[test]
    fn xai_credentials_resolve_from_env_and_auth_store() {
        let _env = EnvScope::new();
        std::env::set_var("XAI_API_KEY", "env-xai-key");
        std::env::set_var("XAI_BASE_URL", "https://xai.env.test/v1/");
        let credentials = resolve_xai_video_credentials().unwrap();
        assert_eq!(credentials.api_key, "env-xai-key");
        assert_eq!(credentials.base_url, "https://xai.env.test/v1");
        assert_eq!(credentials.source, "env");

        std::env::remove_var("XAI_API_KEY");
        std::env::remove_var("XAI_BASE_URL");
        let path = hermes_config::paths::auth_json_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "providers": {
                    "xai-oauth": {
                        "access_token": "oauth-xai-token",
                        "api_base_url": "https://xai.store.test/v1/"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let credentials = resolve_xai_video_credentials().unwrap();
        assert_eq!(credentials.api_key, "oauth-xai-token");
        assert_eq!(credentials.base_url, "https://xai.store.test/v1");
        assert_eq!(credentials.source, path.display().to_string());
    }

    #[test]
    fn video_gen_backend_selection_requires_explicit_xai_choice() {
        let _env = EnvScope::new();
        std::env::set_var("XAI_API_KEY", "xai-key");
        assert_eq!(
            VideoGenBackend::from_env_or_managed().provider_label(),
            "fal"
        );

        std::env::set_var("HERMES_VIDEO_GEN_BACKEND", "xai");
        let selected = VideoGenBackend::from_env_or_managed();
        assert_eq!(selected.provider_label(), "xai");
        assert_eq!(
            selected.required_env_vars(),
            vec!["XAI_API_KEY".to_string()]
        );
    }

    #[test]
    fn video_gen_provider_selection_reads_config_candidates() {
        let _env = EnvScope::new();
        let config = hermes_config::config_path();
        std::fs::write(config, "video_gen:\n  provider: grok-imagine\n").unwrap();
        assert_eq!(selected_video_provider(), Some("xai"));
    }

    #[tokio::test]
    async fn xai_unconfigured_backend_errors_before_network() {
        let _env = EnvScope::new();
        let backend = XaiVideoGenBackend::unconfigured();
        let err = backend.generate_video(request(None)).await.unwrap_err();
        assert!(err.to_string().contains("No xAI credentials"));
    }
}
