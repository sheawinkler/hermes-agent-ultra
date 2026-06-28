struct BackendBestPracticeProfile {
    provider: &'static str,
    profile: &'static str,
    summary: &'static str,
    launch_hint: &'static str,
    env_overrides: &'static [(&'static str, &'static str)],
}

const VLLM_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("VLLM_GPU_MEMORY_UTILIZATION", "0.88"),
    ("VLLM_ENABLE_PREFIX_CACHING", "1"),
    ("VLLM_ENABLE_CHUNKED_PREFILL", "1"),
];
const VLLM_PROFILE_THROUGHPUT_ENV: &[(&str, &str)] = &[
    ("VLLM_GPU_MEMORY_UTILIZATION", "0.92"),
    ("VLLM_MAX_NUM_SEQS", "256"),
    ("VLLM_ENABLE_PREFIX_CACHING", "1"),
];
const VLLM_PROFILE_RELIABILITY_ENV: &[(&str, &str)] = &[
    ("VLLM_GPU_MEMORY_UTILIZATION", "0.80"),
    ("VLLM_MAX_NUM_SEQS", "64"),
    ("VLLM_ENABLE_CHUNKED_PREFILL", "0"),
];
const LLAMA_CPP_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("LLAMA_CPP_THREADS", "8"),
    ("LLAMA_CPP_CTX_SIZE", "8192"),
    ("LLAMA_CPP_BATCH", "512"),
];
const MLX_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("MLX_QUANT", "4bit"),
    ("MLX_MAX_BATCH_SIZE", "16"),
    ("MLX_ENABLE_PROMPT_CACHE", "1"),
];
const SGLANG_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("SGLANG_ENABLE_RADIX_CACHE", "1"),
    ("SGLANG_MAX_RUNNING_REQUESTS", "256"),
];
const TGI_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("TGI_MAX_BATCH_TOTAL_TOKENS", "32768"),
    ("TGI_WAITING_SERVED_RATIO", "0.30"),
];
const APPLE_ANE_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("APPLE_ANE_ENABLE_LOW_LATENCY", "1"),
    ("APPLE_ANE_PREFILL_TOKENS", "1024"),
];
const MISTRAL_RS_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("MISTRAL_RS_PAGED_ATTENTION", "1"),
    ("MISTRAL_RS_KV_CACHE_DTYPE", "fp16"),
    ("MISTRAL_RS_SPECULATIVE_DECODING", "0"),
];

const BACKEND_BEST_PRACTICE_PROFILES: &[BackendBestPracticeProfile] = &[
    BackendBestPracticeProfile {
        provider: "vllm",
        profile: "balanced",
        summary: "Default performance profile for stable throughput and latency.",
        launch_hint:
            "vllm serve MODEL --enable-prefix-caching --enable-chunked-prefill --gpu-memory-utilization 0.88",
        env_overrides: VLLM_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "vllm",
        profile: "throughput",
        summary: "Higher concurrency profile for heavy parallel workloads.",
        launch_hint:
            "vllm serve MODEL --enable-prefix-caching --max-num-seqs 256 --gpu-memory-utilization 0.92",
        env_overrides: VLLM_PROFILE_THROUGHPUT_ENV,
    },
    BackendBestPracticeProfile {
        provider: "vllm",
        profile: "reliability",
        summary: "Lower-pressure profile tuned for long sessions and fewer OOM events.",
        launch_hint:
            "vllm serve MODEL --max-num-seqs 64 --gpu-memory-utilization 0.80 --disable-chunked-prefill",
        env_overrides: VLLM_PROFILE_RELIABILITY_ENV,
    },
    BackendBestPracticeProfile {
        provider: "llama-cpp",
        profile: "balanced",
        summary: "General local GGUF serving profile with predictable latency.",
        launch_hint:
            "llama-server -m MODEL.gguf -c 8192 -t 8 -b 512 --host 127.0.0.1 --port 8080",
        env_overrides: LLAMA_CPP_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "mlx",
        profile: "balanced",
        summary: "Apple Silicon profile prioritizing cache reuse and compact memory.",
        launch_hint:
            "python -m mlx_lm.server --model mlx-community/Qwen3-8B-4bit --host 127.0.0.1 --port 8080",
        env_overrides: MLX_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "apple-ane",
        profile: "balanced",
        summary: "ANE-optimized low-latency settings for on-device endpoints.",
        launch_hint: "Use your ANE OpenAI-compatible server with low-latency prefill settings.",
        env_overrides: APPLE_ANE_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "sglang",
        profile: "balanced",
        summary: "SGLang cache-first profile for sustained request loads.",
        launch_hint:
            "python -m sglang.launch_server --model-path MODEL --host 127.0.0.1 --port 30000",
        env_overrides: SGLANG_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "tgi",
        profile: "balanced",
        summary: "Text-Generation-Inference profile balancing batch depth and tail latency.",
        launch_hint:
            "text-generation-launcher --model-id MODEL --port 8082 --max-batch-total-tokens 32768",
        env_overrides: TGI_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "lmstudio",
        profile: "balanced",
        summary: "Desktop local serving profile for LM Studio's OpenAI-compatible server.",
        launch_hint: "Start LM Studio Local Server on 127.0.0.1:1234 and load a model.",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "lmdeploy",
        profile: "balanced",
        summary: "LMDeploy OpenAI-compatible serving profile for local or workstation GPUs.",
        launch_hint: "lmdeploy serve api_server MODEL --server-port 23333",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "localai",
        profile: "balanced",
        summary: "LocalAI OpenAI-compatible serving profile for mixed local backends.",
        launch_hint: "local-ai run --address 127.0.0.1:8080",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "koboldcpp",
        profile: "balanced",
        summary: "KoboldCpp single-binary profile for GGUF local serving.",
        launch_hint: "koboldcpp --model MODEL.gguf --host 127.0.0.1 --port 5001",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "text-generation-webui",
        profile: "balanced",
        summary: "oobabooga text-generation-webui OpenAI extension profile.",
        launch_hint: "python server.py --extensions openai --api --api-port 5000",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "tabbyapi",
        profile: "balanced",
        summary: "TabbyAPI / ExLlamaV2 profile for quantized GPU serving.",
        launch_hint: "python main.py --host 127.0.0.1 --port 5000",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "mistral-rs",
        profile: "balanced",
        summary: "mistral.rs runtime baseline for robust local serving.",
        launch_hint: "mistralrs-server --model MODEL --port 8083 --paged-attention",
        env_overrides: MISTRAL_RS_PROFILE_BALANCED_ENV,
    },
];

fn normalize_backend_provider(value: &str) -> String {
    let raw = value.trim().to_ascii_lowercase();
    match raw.as_str() {
        "llvm" | "ollvm" => "vllm".to_string(),
        "llama.cpp" | "llamacpp" | "llamafile" => "llama-cpp".to_string(),
        "mlx-lm" | "apple-mlx" | "vmlx" | "omlx" | "mlx-vlm" | "mlxvlm" | "mlx-openai-server" => {
            "mlx".to_string()
        }
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "lm-studio" | "lm_studio" | "lm studio" => "lmstudio".to_string(),
        "lm-deploy" | "lm_deploy" => "lmdeploy".to_string(),
        "local-ai" | "local_ai" => "localai".to_string(),
        "kobold-cpp" | "kobold" => "koboldcpp".to_string(),
        "oobabooga" | "textgen-webui" | "textgen_webui" | "text-generation-web-ui" => {
            "text-generation-webui".to_string()
        }
        "tabby-api" | "tabby_api" | "exllama" | "exllamav2" => "tabbyapi".to_string(),
        other => other.to_string(),
    }
}

fn backend_profile_lookup(
    provider: &str,
    profile: Option<&str>,
) -> Option<&'static BackendBestPracticeProfile> {
    let normalized = normalize_backend_provider(provider);
    let profile = profile.unwrap_or("balanced").trim().to_ascii_lowercase();
    BACKEND_BEST_PRACTICE_PROFILES.iter().find(|row| {
        row.provider.eq_ignore_ascii_case(&normalized) && row.profile.eq_ignore_ascii_case(&profile)
    })
}

fn render_backend_profiles(provider: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("Backend best-practice profiles\n");
    out.push_str("-------------------------------\n");
    let filtered: Vec<&BackendBestPracticeProfile> = if let Some(provider) = provider {
        let normalized = normalize_backend_provider(provider);
        BACKEND_BEST_PRACTICE_PROFILES
            .iter()
            .filter(|row| row.provider.eq_ignore_ascii_case(&normalized))
            .collect()
    } else {
        BACKEND_BEST_PRACTICE_PROFILES.iter().collect()
    };
    if filtered.is_empty() {
        let selected = provider.unwrap_or("(none)");
        let _ = writeln!(out, "No backend profile presets found for '{}'.", selected);
        return out.trim_end().to_string();
    }
    for row in filtered {
        let _ = writeln!(
            out,
            "- {}:{}\n  {}\n  launch: {}\n  env: {}",
            row.provider,
            row.profile,
            row.summary,
            row.launch_hint,
            row.env_overrides
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<String>>()
                .join(", ")
        );
    }
    out.push_str("\nUse `/model backend apply <provider> [profile]` to load env overrides for current runtime.");
    out.trim_end().to_string()
}

fn persist_backend_profile_env(
    provider: &str,
    profile: &str,
    env_pairs: &[(&str, &str)],
) -> Result<PathBuf, AgentError> {
    let dir = hermes_config::hermes_home()
        .join("runtime")
        .join("backend_profiles");
    std::fs::create_dir_all(&dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create backend profile directory {}: {}",
            dir.display(),
            e
        ))
    })?;
    let path = dir.join(format!(
        "{}-{}.env",
        normalize_backend_provider(provider),
        profile.trim().to_ascii_lowercase()
    ));
    let mut body = String::new();
    for (key, value) in env_pairs {
        let _ = writeln!(body, "{}={}", key, value);
    }
    std::fs::write(&path, body).map_err(|e| {
        AgentError::Io(format!(
            "Failed to write backend profile file {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(path)
}

fn model_current_provider_and_id(model: &str) -> (String, String) {
    if let Some((provider, model_id)) = model.split_once(':') {
        (
            provider.trim().to_ascii_lowercase(),
            model_id.trim().to_string(),
        )
    } else {
        ("openai".to_string(), model.trim().to_string())
    }
}
