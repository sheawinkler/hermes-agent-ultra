#[derive(Debug, Clone)]
struct OpenVikingConfig {
    endpoint: String,
    api_key: String,
    api_key_type: String,
    account: String,
    user: String,
    agent: String,
    recall: OpenVikingRecallConfig,
}

#[derive(Debug, Clone)]
struct OpenVikingRecallConfig {
    limit: usize,
    score_threshold: f64,
    max_injected_chars: usize,
    timeout: Duration,
    request_timeout: Duration,
    full_read_limit: usize,
    prefer_abstract: bool,
    include_resources: bool,
}

impl Default for OpenVikingRecallConfig {
    fn default() -> Self {
        Self {
            limit: DEFAULT_RECALL_LIMIT,
            score_threshold: DEFAULT_RECALL_SCORE_THRESHOLD,
            max_injected_chars: DEFAULT_RECALL_MAX_INJECTED_CHARS,
            timeout: DEFAULT_RECALL_TIMEOUT,
            request_timeout: DEFAULT_RECALL_REQUEST_TIMEOUT,
            full_read_limit: DEFAULT_RECALL_FULL_READ_LIMIT,
            prefer_abstract: false,
            include_resources: false,
        }
    }
}

impl OpenVikingConfig {
    fn config_path(hermes_home: &str) -> PathBuf {
        Path::new(hermes_home).join("openviking.json")
    }

    fn default_config_path() -> PathBuf {
        config_io::default_hermes_home().join("openviking.json")
    }

    fn configured_at(path: &Path) -> bool {
        let object = config_io::read_json_object(path);
        if object
            .get("enabled")
            .and_then(Value::as_bool)
            .is_some_and(|enabled| enabled)
        {
            return true;
        }
        ["endpoint", "api_key", "root_api_key"].iter().any(|key| {
            object
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
    }

    fn load(hermes_home: &str) -> Self {
        let mut config = Self {
            endpoint: std::env::var("OPENVIKING_ENDPOINT")
                .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string()),
            api_key: std::env::var("OPENVIKING_API_KEY").unwrap_or_default(),
            api_key_type: std::env::var("OPENVIKING_API_KEY_TYPE")
                .unwrap_or_else(|_| "user".to_string()),
            account: std::env::var("OPENVIKING_ACCOUNT").unwrap_or_else(|_| "default".into()),
            user: std::env::var("OPENVIKING_USER").unwrap_or_else(|_| "default".into()),
            agent: std::env::var("OPENVIKING_AGENT").unwrap_or_else(|_| DEFAULT_AGENT.into()),
            recall: OpenVikingRecallConfig::from_env(),
        };

        let path = Self::config_path(hermes_home);
        let raw = config_io::read_json_object(&path);
        apply_openviking_config_map(&mut config, &raw);

        config.endpoint = normalize_openviking_endpoint(&config.endpoint);
        config.api_key_type = normalize_openviking_key_type(&config.api_key_type);
        config.account = nonempty_or(&config.account, "default");
        config.user = nonempty_or(&config.user, "default");
        config.agent = nonempty_or(&config.agent, DEFAULT_AGENT);
        config
    }
}

impl OpenVikingRecallConfig {
    fn from_env() -> Self {
        let mut cfg = Self::default();
        apply_recall_usize_env(&mut cfg.limit, "OPENVIKING_RECALL_LIMIT");
        apply_recall_f64_env(
            &mut cfg.score_threshold,
            "OPENVIKING_RECALL_SCORE_THRESHOLD",
        );
        apply_recall_usize_env(
            &mut cfg.max_injected_chars,
            "OPENVIKING_RECALL_MAX_INJECTED_CHARS",
        );
        apply_recall_duration_env(&mut cfg.timeout, "OPENVIKING_RECALL_TIMEOUT_SECONDS");
        apply_recall_duration_env(
            &mut cfg.request_timeout,
            "OPENVIKING_RECALL_REQUEST_TIMEOUT_SECONDS",
        );
        apply_recall_usize_env(
            &mut cfg.full_read_limit,
            "OPENVIKING_RECALL_FULL_READ_LIMIT",
        );
        apply_recall_bool_env(
            &mut cfg.prefer_abstract,
            "OPENVIKING_RECALL_PREFER_ABSTRACT",
        );
        apply_recall_bool_env(&mut cfg.include_resources, "OPENVIKING_RECALL_RESOURCES");
        cfg.normalize();
        cfg
    }

    fn normalize(&mut self) {
        self.limit = self.limit.clamp(1, 50);
        self.score_threshold = self.score_threshold.clamp(0.0, 1.0);
        self.max_injected_chars = self.max_injected_chars.clamp(256, 50_000);
        self.timeout = self.timeout.max(RECALL_MIN_TIMEOUT);
        self.request_timeout = self.request_timeout.max(RECALL_MIN_TIMEOUT);
        self.full_read_limit = self.full_read_limit.min(10);
    }
}

fn apply_openviking_config_map(
    config: &mut OpenVikingConfig,
    raw: &serde_json::Map<String, Value>,
) {
    if let Some(endpoint) = raw
        .get("endpoint")
        .or(raw.get("base_url"))
        .or(raw.get("baseUrl"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        config.endpoint = endpoint.to_string();
    }
    if let Some(api_key) = raw
        .get("api_key")
        .or(raw.get("apiKey"))
        .or(raw.get("root_api_key"))
        .or(raw.get("rootApiKey"))
        .and_then(Value::as_str)
    {
        config.api_key = api_key.to_string();
    }
    if let Some(key_type) = raw
        .get("api_key_type")
        .or(raw.get("apiKeyType"))
        .and_then(Value::as_str)
    {
        config.api_key_type = key_type.to_string();
    }
    if let Some(account) = raw.get("account").and_then(Value::as_str) {
        config.account = account.to_string();
    }
    if let Some(user) = raw.get("user").and_then(Value::as_str) {
        config.user = user.to_string();
    }
    if let Some(agent) = raw.get("agent").and_then(Value::as_str) {
        config.agent = agent.to_string();
    }
    apply_recall_usize_map(&mut config.recall.limit, raw, "recall_limit");
    apply_recall_f64_map(
        &mut config.recall.score_threshold,
        raw,
        "recall_score_threshold",
    );
    apply_recall_usize_map(
        &mut config.recall.max_injected_chars,
        raw,
        "recall_max_injected_chars",
    );
    apply_recall_duration_map(&mut config.recall.timeout, raw, "recall_timeout_seconds");
    apply_recall_duration_map(
        &mut config.recall.request_timeout,
        raw,
        "recall_request_timeout_seconds",
    );
    apply_recall_usize_map(
        &mut config.recall.full_read_limit,
        raw,
        "recall_full_read_limit",
    );
    apply_recall_bool_map(
        &mut config.recall.prefer_abstract,
        raw,
        "recall_prefer_abstract",
    );
    apply_recall_bool_map(
        &mut config.recall.include_resources,
        raw,
        "recall_resources",
    );
    config.recall.normalize();
}

fn json_number_or_string_usize(value: &Value) -> Option<usize> {
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .or_else(|| {
            value
                .as_str()
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
}

fn json_number_or_string_f64(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| {
        value
            .as_str()
            .and_then(|value| value.trim().parse::<f64>().ok())
    })
}

fn json_boolish(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| {
        value
            .as_str()
            .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Some(true),
                "0" | "false" | "no" | "off" => Some(false),
                _ => None,
            })
    })
}

fn apply_recall_usize_env(target: &mut usize, key: &str) {
    if let Ok(value) = std::env::var(key) {
        if let Ok(parsed) = value.trim().parse::<usize>() {
            *target = parsed;
        }
    }
}

fn apply_recall_f64_env(target: &mut f64, key: &str) {
    if let Ok(value) = std::env::var(key) {
        if let Ok(parsed) = value.trim().parse::<f64>() {
            *target = parsed;
        }
    }
}

fn apply_recall_duration_env(target: &mut Duration, key: &str) {
    if let Ok(value) = std::env::var(key) {
        if let Ok(parsed) = value.trim().parse::<f64>() {
            *target = duration_from_secs_f64(parsed);
        }
    }
}

fn apply_recall_bool_env(target: &mut bool, key: &str) {
    if let Ok(value) = std::env::var(key) {
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => *target = true,
            "0" | "false" | "no" | "off" => *target = false,
            _ => {}
        }
    }
}

fn apply_recall_usize_map(target: &mut usize, raw: &serde_json::Map<String, Value>, key: &str) {
    if let Some(value) = raw.get(key).and_then(json_number_or_string_usize) {
        *target = value;
    }
}

fn apply_recall_f64_map(target: &mut f64, raw: &serde_json::Map<String, Value>, key: &str) {
    if let Some(value) = raw.get(key).and_then(json_number_or_string_f64) {
        *target = value;
    }
}

fn apply_recall_duration_map(
    target: &mut Duration,
    raw: &serde_json::Map<String, Value>,
    key: &str,
) {
    if let Some(value) = raw.get(key).and_then(json_number_or_string_f64) {
        *target = duration_from_secs_f64(value);
    }
}

fn apply_recall_bool_map(target: &mut bool, raw: &serde_json::Map<String, Value>, key: &str) {
    if let Some(value) = raw.get(key).and_then(json_boolish) {
        *target = value;
    }
}

fn duration_from_secs_f64(seconds: f64) -> Duration {
    if !seconds.is_finite() || seconds <= 0.0 {
        RECALL_MIN_TIMEOUT
    } else {
        Duration::from_secs_f64(seconds)
    }
}

fn normalize_openviking_endpoint(raw: &str) -> String {
    let value = raw.trim();
    let with_scheme = if value.is_empty() {
        DEFAULT_ENDPOINT.to_string()
    } else if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

fn normalize_openviking_key_type(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "root" | "root_api_key" | "root-api-key" => "root".to_string(),
        "none" | "dev" | "local" | "no_api_key" | "no-api-key" => "none".to_string(),
        _ => "user".to_string(),
    }
}

fn nonempty_or(raw: &str, default: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}
