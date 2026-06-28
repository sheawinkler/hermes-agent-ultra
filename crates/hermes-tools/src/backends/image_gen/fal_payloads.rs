fn env_optional_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn resolve_fal_model_path() -> String {
    for key in ["FAL_IMAGE_MODEL", "HERMES_FAL_IMAGE_MODEL"] {
        if let Some(value) =
            env_optional_nonempty(key).and_then(|value| normalize_fal_model_path(&value))
        {
            return value;
        }
    }
    if let Some(cfg) = load_image_gen_config() {
        if let Some(fal_cfg) = yaml_get(&cfg, "fal") {
            if let Some(value) = yaml_get_str(fal_cfg, "model").and_then(normalize_fal_model_path) {
                return value;
            }
        }
        if let Some(value) = yaml_get_str(&cfg, "model").and_then(normalize_fal_model_path) {
            return value;
        }
    }
    DEFAULT_FAL_MODEL_PATH.to_string()
}

fn resolve_krea_api_key() -> Option<String> {
    if let Some(value) = scoped_krea_config()
        .as_ref()
        .and_then(resolve_api_key_from_yaml_provider_section)
    {
        return Some(value);
    }
    env_optional_nonempty("KREA_API_KEY")
}

fn resolve_krea_base_url() -> String {
    if let Some(value) = scoped_krea_config()
        .as_ref()
        .and_then(|cfg| yaml_get_any_str(cfg, &["base_url", "api_base_url"]).map(ToOwned::to_owned))
    {
        return value.trim_end_matches('/').to_string();
    }
    for key in ["KREA_IMAGE_BASE_URL", "KREA_BASE_URL"] {
        if let Some(value) = env_optional_nonempty(key) {
            return value.trim_end_matches('/').to_string();
        }
    }
    DEFAULT_KREA_IMAGE_BASE_URL.to_string()
}

fn resolve_krea_model(explicit: Option<&str>) -> KreaModelSpec {
    if let Some(spec) = explicit.and_then(krea_model_spec) {
        return spec;
    }
    if let Some(spec) = env_optional_nonempty("KREA_IMAGE_MODEL").and_then(|v| krea_model_spec(&v))
    {
        return spec;
    }
    if let Some(spec) = scoped_krea_config()
        .as_ref()
        .and_then(|cfg| yaml_get_str(cfg, "model"))
        .and_then(krea_model_spec)
    {
        return spec;
    }
    if let Some(spec) = load_image_gen_config()
        .as_ref()
        .and_then(|cfg| yaml_get_str(cfg, "model"))
        .and_then(krea_model_spec)
    {
        return spec;
    }
    krea_model_spec(DEFAULT_KREA_IMAGE_MODEL).expect("default Krea model")
}

fn resolve_krea_creativity(explicit: Option<&str>) -> String {
    if let Some(value) = explicit.and_then(normalize_krea_creativity) {
        return value.to_string();
    }
    if let Some(value) =
        env_optional_nonempty("KREA_IMAGE_CREATIVITY").and_then(|v| normalize_krea_creativity(&v))
    {
        return value.to_string();
    }
    if let Some(value) = scoped_krea_config()
        .as_ref()
        .and_then(|cfg| yaml_get_str(cfg, "creativity"))
        .and_then(normalize_krea_creativity)
    {
        return value.to_string();
    }
    DEFAULT_KREA_CREATIVITY.to_string()
}

fn scoped_krea_config() -> Option<serde_yaml::Value> {
    let cfg = load_image_gen_config()?;
    yaml_get(&cfg, "krea").cloned()
}

fn krea_prefers_gateway() -> bool {
    for key in ["KREA_USE_GATEWAY", "HERMES_KREA_USE_GATEWAY"] {
        if env_optional_nonempty(key).is_some_and(|value| truthy_or_managed(&value)) {
            return true;
        }
    }
    scoped_krea_config().as_ref().is_some_and(|cfg| {
        yaml_get_boolish(cfg, &["use_gateway", "gateway", "managed"])
            || yaml_get_any_str(cfg, &["backend", "mode", "transport"])
                .is_some_and(truthy_or_managed)
    })
}

fn krea_model_spec(value: &str) -> Option<KreaModelSpec> {
    let normalized = value.trim().to_ascii_lowercase();
    KREA_MODELS
        .iter()
        .copied()
        .find(|spec| spec.id == normalized)
}

fn normalize_krea_creativity(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "raw" => Some("raw"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        _ => None,
    }
}

fn normalize_fal_model_path(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else if fal_model_spec(value).is_some() || value.contains('/') {
        Some(value.to_string())
    } else {
        None
    }
}

fn build_fal_text_payload(model_path: &str, request: &ImageGenerateRequest) -> Value {
    let Some(spec) = fal_model_spec(model_path) else {
        return build_legacy_fal_text_payload(request);
    };
    let mut payload = fal_model_defaults(spec.id);
    payload.insert("prompt".to_string(), json!(request.prompt.trim()));
    insert_fal_size(&mut payload, spec, spec.supports, request.size.as_deref());
    insert_common_fal_overrides(&mut payload, spec.supports, request);
    retain_supported_keys(&mut payload, spec.supports);
    Value::Object(payload)
}

fn build_fal_edit_payload(
    spec: FalModelSpec,
    request: &ImageGenerateRequest,
    source_images: &[String],
) -> Value {
    let mut payload = fal_model_defaults(spec.id);
    payload.insert("prompt".to_string(), json!(request.prompt.trim()));
    payload.insert("image_urls".to_string(), json!(source_images));
    insert_fal_size(
        &mut payload,
        spec,
        spec.edit_supports,
        request.size.as_deref(),
    );
    insert_common_fal_overrides(&mut payload, spec.edit_supports, request);
    retain_supported_keys(&mut payload, spec.edit_supports);
    Value::Object(payload)
}

fn build_legacy_fal_text_payload(request: &ImageGenerateRequest) -> Value {
    let (width, height) = match request.size.as_deref().map(str::trim) {
        Some("256x256") => (256, 256),
        Some("512x512") => (512, 512),
        _ => (1024, 1024),
    };
    json!({
        "prompt": request.prompt.trim(),
        "image_size": {
            "width": width,
            "height": height,
        },
        "num_images": request.n.unwrap_or(1),
    })
}

fn insert_fal_size(
    payload: &mut Map<String, Value>,
    spec: FalModelSpec,
    supports: &[&str],
    size: Option<&str>,
) {
    let aspect = fal_aspect_from_tool_size(size);
    let value = match aspect {
        "square" => spec.square,
        "portrait" => spec.portrait,
        _ => spec.landscape,
    };
    match spec.size_style {
        FalSizeStyle::ImageSizePreset | FalSizeStyle::GptLiteral => {
            if supports_key(supports, "image_size") {
                payload.insert("image_size".to_string(), json!(value));
            }
        }
        FalSizeStyle::AspectRatio => {
            if supports_key(supports, "aspect_ratio") {
                payload.insert("aspect_ratio".to_string(), json!(value));
            }
        }
    }
}

fn insert_common_fal_overrides(
    payload: &mut Map<String, Value>,
    supports: &[&str],
    request: &ImageGenerateRequest,
) {
    if let Some(n) = request.n {
        if supports_key(supports, "num_images") {
            payload.insert("num_images".to_string(), json!(n));
        }
    }
    if let Some(style) = request
        .style
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if supports_key(supports, "style") {
            payload.insert("style".to_string(), json!(style));
        }
    }
}

fn fal_aspect_from_tool_size(size: Option<&str>) -> &'static str {
    match size.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("square") | Some("1:1") | Some("1024x1024") | Some("512x512") | Some("256x256") => {
            "square"
        }
        Some("portrait") | Some("9:16") | Some("1024x1536") => "portrait",
        Some("landscape") | Some("16:9") | Some("1536x1024") => "landscape",
        _ => DEFAULT_FAL_ASPECT_RATIO,
    }
}

fn krea_aspect_from_tool_size(size: Option<&str>) -> &'static str {
    match size.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("landscape") | Some("16:9") | Some("1536x1024") => "16:9",
        Some("portrait") | Some("9:16") | Some("1024x1536") => "9:16",
        _ => "1:1",
    }
}

fn krea_tool_aspect_from_krea(aspect: &str) -> &'static str {
    match aspect {
        "16:9" => "landscape",
        "9:16" => "portrait",
        _ => "square",
    }
}

fn retain_supported_keys(payload: &mut Map<String, Value>, supports: &[&str]) {
    payload.retain(|key, _| supports_key(supports, key));
}

fn supports_key(supports: &[&str], key: &str) -> bool {
    supports.contains(&key)
}

fn fal_model_defaults(model_path: &str) -> Map<String, Value> {
    let mut payload = Map::new();
    match model_path {
        "fal-ai/flux-2/klein/9b" => {
            payload.insert("num_inference_steps".to_string(), json!(4));
            payload.insert("output_format".to_string(), json!("png"));
            payload.insert("enable_safety_checker".to_string(), json!(false));
        }
        "fal-ai/flux-2-pro" => {
            payload.insert("num_inference_steps".to_string(), json!(50));
            payload.insert("guidance_scale".to_string(), json!(4.5));
            payload.insert("num_images".to_string(), json!(1));
            payload.insert("output_format".to_string(), json!("png"));
            payload.insert("enable_safety_checker".to_string(), json!(false));
            payload.insert("safety_tolerance".to_string(), json!("5"));
            payload.insert("sync_mode".to_string(), json!(true));
        }
        "fal-ai/z-image/turbo" => {
            payload.insert("num_inference_steps".to_string(), json!(8));
            payload.insert("num_images".to_string(), json!(1));
            payload.insert("output_format".to_string(), json!("png"));
            payload.insert("enable_safety_checker".to_string(), json!(false));
            payload.insert("enable_prompt_expansion".to_string(), json!(false));
        }
        "fal-ai/nano-banana-pro" => {
            payload.insert("num_images".to_string(), json!(1));
            payload.insert("output_format".to_string(), json!("png"));
            payload.insert("safety_tolerance".to_string(), json!("5"));
            payload.insert("resolution".to_string(), json!("1K"));
        }
        "fal-ai/gpt-image-1.5" | "fal-ai/gpt-image-2" => {
            payload.insert("quality".to_string(), json!("medium"));
            payload.insert("num_images".to_string(), json!(1));
            payload.insert("output_format".to_string(), json!("png"));
        }
        "fal-ai/ideogram/v3" => {
            payload.insert("rendering_speed".to_string(), json!("BALANCED"));
            payload.insert("expand_prompt".to_string(), json!(true));
            payload.insert("style".to_string(), json!("AUTO"));
        }
        "fal-ai/recraft/v4/pro/text-to-image" => {
            payload.insert("enable_safety_checker".to_string(), json!(false));
        }
        "fal-ai/qwen-image" => {
            payload.insert("num_inference_steps".to_string(), json!(30));
            payload.insert("guidance_scale".to_string(), json!(2.5));
            payload.insert("num_images".to_string(), json!(1));
            payload.insert("output_format".to_string(), json!("png"));
            payload.insert("acceleration".to_string(), json!("regular"));
        }
        "fal-ai/krea/v2/medium/text-to-image" | "fal-ai/krea/v2/large/text-to-image" => {
            payload.insert("creativity".to_string(), json!("medium"));
        }
        _ => {}
    }
    payload
}

fn fal_model_spec(model_path: &str) -> Option<FalModelSpec> {
    match model_path.trim() {
        "fal-ai/flux-2/klein/9b" => Some(FalModelSpec {
            id: "fal-ai/flux-2/klein/9b",
            display: "FLUX 2 Klein 9B",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "num_inference_steps",
                "seed",
                "output_format",
                "enable_safety_checker",
            ],
            edit_endpoint: Some("fal-ai/flux-2/klein/9b/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "num_inference_steps",
                "seed",
                "output_format",
                "enable_safety_checker",
            ],
            max_reference_images: 9,
        }),
        "fal-ai/flux-2-pro" => Some(FalModelSpec {
            id: "fal-ai/flux-2-pro",
            display: "FLUX 2 Pro",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "num_inference_steps",
                "guidance_scale",
                "num_images",
                "output_format",
                "enable_safety_checker",
                "safety_tolerance",
                "sync_mode",
                "seed",
            ],
            edit_endpoint: Some("fal-ai/flux-2-pro/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "num_inference_steps",
                "guidance_scale",
                "num_images",
                "output_format",
                "enable_safety_checker",
                "safety_tolerance",
                "sync_mode",
                "seed",
            ],
            max_reference_images: 9,
        }),
        "fal-ai/z-image/turbo" => Some(FalModelSpec {
            id: "fal-ai/z-image/turbo",
            display: "Z-Image Turbo",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "num_inference_steps",
                "num_images",
                "seed",
                "output_format",
                "enable_safety_checker",
                "enable_prompt_expansion",
            ],
            edit_endpoint: None,
            edit_supports: &[],
            max_reference_images: 0,
        }),
        "fal-ai/nano-banana-pro" => Some(FalModelSpec {
            id: "fal-ai/nano-banana-pro",
            display: "Nano Banana Pro (Gemini 3 Pro Image)",
            size_style: FalSizeStyle::AspectRatio,
            landscape: "16:9",
            square: "1:1",
            portrait: "9:16",
            supports: &[
                "prompt",
                "aspect_ratio",
                "num_images",
                "output_format",
                "safety_tolerance",
                "seed",
                "sync_mode",
                "resolution",
                "enable_web_search",
                "limit_generations",
            ],
            edit_endpoint: Some("fal-ai/nano-banana-pro/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "aspect_ratio",
                "num_images",
                "output_format",
                "safety_tolerance",
                "seed",
                "sync_mode",
                "resolution",
                "enable_web_search",
                "limit_generations",
            ],
            max_reference_images: 2,
        }),
        "fal-ai/gpt-image-1.5" => Some(FalModelSpec {
            id: "fal-ai/gpt-image-1.5",
            display: "GPT Image 1.5",
            size_style: FalSizeStyle::GptLiteral,
            landscape: "1536x1024",
            square: "1024x1024",
            portrait: "1024x1536",
            supports: &[
                "prompt",
                "image_size",
                "quality",
                "num_images",
                "output_format",
                "background",
                "sync_mode",
            ],
            edit_endpoint: Some("fal-ai/gpt-image-1.5/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "image_size",
                "quality",
                "num_images",
                "output_format",
                "sync_mode",
            ],
            max_reference_images: 16,
        }),
        "fal-ai/gpt-image-2" => Some(FalModelSpec {
            id: "fal-ai/gpt-image-2",
            display: "GPT Image 2",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_4_3",
            square: "square_hd",
            portrait: "portrait_4_3",
            supports: &[
                "prompt",
                "image_size",
                "quality",
                "num_images",
                "output_format",
                "sync_mode",
            ],
            edit_endpoint: Some("openai/gpt-image-2/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "quality",
                "num_images",
                "output_format",
                "sync_mode",
                "mask_image_url",
            ],
            max_reference_images: 16,
        }),
        "fal-ai/ideogram/v3" => Some(FalModelSpec {
            id: "fal-ai/ideogram/v3",
            display: "Ideogram V3",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "rendering_speed",
                "expand_prompt",
                "style",
                "seed",
            ],
            edit_endpoint: Some("fal-ai/ideogram/v3/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "rendering_speed",
                "expand_prompt",
                "style",
                "seed",
            ],
            max_reference_images: 1,
        }),
        "fal-ai/recraft/v4/pro/text-to-image" => Some(FalModelSpec {
            id: "fal-ai/recraft/v4/pro/text-to-image",
            display: "Recraft V4 Pro",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "enable_safety_checker",
                "colors",
                "background_color",
            ],
            edit_endpoint: None,
            edit_supports: &[],
            max_reference_images: 0,
        }),
        "fal-ai/qwen-image" => Some(FalModelSpec {
            id: "fal-ai/qwen-image",
            display: "Qwen Image",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "num_inference_steps",
                "guidance_scale",
                "num_images",
                "output_format",
                "acceleration",
                "seed",
                "sync_mode",
            ],
            edit_endpoint: Some("fal-ai/qwen-image-2/pro/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "num_inference_steps",
                "guidance_scale",
                "num_images",
                "output_format",
                "acceleration",
                "seed",
                "sync_mode",
            ],
            max_reference_images: 3,
        }),
        "fal-ai/krea/v2/medium/text-to-image" => Some(FalModelSpec {
            id: "fal-ai/krea/v2/medium/text-to-image",
            display: "Krea 2 Medium",
            size_style: FalSizeStyle::AspectRatio,
            landscape: "16:9",
            square: "1:1",
            portrait: "9:16",
            supports: &[
                "prompt",
                "aspect_ratio",
                "creativity",
                "seed",
                "image_style_references",
            ],
            edit_endpoint: None,
            edit_supports: &[],
            max_reference_images: 0,
        }),
        "fal-ai/krea/v2/large/text-to-image" => Some(FalModelSpec {
            id: "fal-ai/krea/v2/large/text-to-image",
            display: "Krea 2 Large",
            size_style: FalSizeStyle::AspectRatio,
            landscape: "16:9",
            square: "1:1",
            portrait: "9:16",
            supports: &[
                "prompt",
                "aspect_ratio",
                "creativity",
                "seed",
                "image_style_references",
            ],
            edit_endpoint: None,
            edit_supports: &[],
            max_reference_images: 0,
        }),
        _ => None,
    }
}

