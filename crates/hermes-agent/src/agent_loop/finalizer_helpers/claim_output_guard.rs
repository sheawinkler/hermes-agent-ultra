fn objective_eval_score(state: &str) -> f64 {
    match state.trim().to_ascii_lowercase().as_str() {
        "advancing" => 1.0,
        "flat" => 0.5,
        "regressing" => 0.0,
        "unproven" => 0.25,
        _ => 0.4,
    }
}

fn claim_verifier_enabled_runtime() -> bool {
    if let Ok(raw) = std::env::var("HERMES_CLAIM_VERIFIER_ENABLED") {
        return !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        );
    }
    let hermes_home = std::env::var("HERMES_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
        .unwrap_or_else(|| PathBuf::from(".hermes"));
    let path = hermes_home.join("alpha").join("claim_verifier_policy.json");
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return true,
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return true,
    };
    parsed
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

fn detect_research_evidence_intent(messages: &[Message]) -> bool {
    if !detect_repo_review_intent(messages) {
        return false;
    }
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let combined = format!("{} {}", user, objective);
    [
        "research",
        "analyze",
        "analysis",
        "assess",
        "report",
        "read-only",
        "readonly",
        "evidence",
        "recommendation",
        "recommendations",
        "improve",
        "profitability",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
}

fn normalize_evidence_path_token(raw: &str) -> Option<String> {
    let mut token = raw
        .trim()
        .trim_start_matches(['`', '"', '\''])
        .trim_end_matches(['`', '"', '\'', ',', ';', ')', ']', '.'])
        .to_string();
    if token.is_empty()
        || token.starts_with('<')
        || token.starts_with('$')
        || token.starts_with("http://")
        || token.starts_with("https://")
        || token.contains("...")
    {
        return None;
    }
    if let Some((path, suffix)) = token.rsplit_once(':') {
        if !path.is_empty() && suffix.chars().all(|c| c.is_ascii_digit() || c == '-') {
            token = path.to_string();
        }
    }
    Some(token)
}

fn extract_explicit_evidence_paths(assistant_text: &str) -> Vec<String> {
    let markers = ["file=", "path=", "file:", "path:"];
    let mut paths = Vec::new();
    for line in assistant_text.lines() {
        let lower = line.to_ascii_lowercase();
        for marker in markers {
            let mut search_start = 0usize;
            while let Some(relative_idx) = lower[search_start..].find(marker) {
                let value_start = search_start + relative_idx + marker.len();
                let raw = line[value_start..].trim_start();
                let raw = raw
                    .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';' | ')' | ']'))
                    .next()
                    .unwrap_or_default();
                if let Some(token) = normalize_evidence_path_token(raw) {
                    paths.push(token);
                }
                search_start = value_start;
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn assistant_references_missing_evidence_paths_from_base(
    assistant_text: &str,
    base: &Path,
) -> bool {
    extract_explicit_evidence_paths(assistant_text)
        .iter()
        .any(|token| {
            let path = Path::new(token);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                base.join(path)
            };
            !resolved.exists()
        })
}

fn assistant_references_missing_evidence_paths(assistant_text: &str) -> bool {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    assistant_references_missing_evidence_paths_from_base(assistant_text, &base)
}

fn finalizer_claim_requires_evidence_retry(
    messages: &[Message],
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if !claim_verifier_enabled_runtime() {
        return false;
    }
    if retry_count >= FINALIZER_EVIDENCE_MAX_RETRIES || !detect_repo_review_intent(messages) {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    let claims_completion = [
        "completed",
        "implemented",
        "fixed",
        "done",
        "resolved",
        "ready",
        "finished",
        "shipped",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let research_evidence_required = detect_research_evidence_intent(messages);
    if !(claims_completion || research_evidence_required) {
        return false;
    }
    let has_evidence = lower.contains("file=")
        || lower.contains("path=")
        || lower.contains("cmd=")
        || lower.contains("exists_now=")
        || lower.contains("`/users/")
        || lower.contains("cargo test");
    let has_confidence = lower.contains("confidence=high")
        || lower.contains("confidence=medium")
        || lower.contains("confidence=low")
        || lower.contains("confidence:");
    if research_evidence_required {
        let has_explicit_path_evidence = lower.contains("file=") || lower.contains("path=");
        let has_explicit_command_evidence = lower.contains("cmd=") || lower.contains("command=");
        if !(has_explicit_path_evidence && has_explicit_command_evidence && has_confidence) {
            return true;
        }
        if assistant_references_missing_evidence_paths(assistant_text) {
            return true;
        }
        return false;
    }
    !(has_evidence && has_confidence)
}

fn strip_list_prefix(line: &str) -> &str {
    let trimmed = line.trim();
    let without_bullet = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .unwrap_or(trimmed);
    let mut chars = without_bullet.char_indices();
    let mut end_idx = 0usize;
    while let Some((idx, ch)) = chars.next() {
        if ch.is_ascii_digit() {
            end_idx = idx + ch.len_utf8();
            continue;
        }
        if (ch == '.' || ch == ')') && end_idx > 0 {
            let tail = &without_bullet[idx + ch.len_utf8()..];
            return tail.trim_start();
        }
        break;
    }
    without_bullet
}

fn finalizer_output_quality_requires_retry(assistant_text: &str, retry_count: u32) -> bool {
    if retry_count >= FINALIZER_OUTPUT_QUALITY_MAX_RETRIES {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    let placeholder_markers = [
        "[url](url)",
        "(url)",
        "[paper details](url)",
        "pack of authors",
        "attached separately",
        "attached reference output",
        "full evidence available",
        "full remand data",
        "see telemetry evidence answer",
        "proposed calibration: redacted",
        "working summary",
        "<tool_call",
        "</tool_call>",
        "lorem ipsum",
        "<insert",
        "<todo",
    ];
    if placeholder_markers
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return true;
    }

    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut in_code_block = false;
    for raw_line in assistant_text.lines() {
        let line = raw_line.trim();
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        let normalized = strip_list_prefix(line).trim().to_ascii_lowercase();
        if normalized.len() < 24 {
            continue;
        }
        let entry = counts.entry(normalized).or_insert(0);
        *entry += 1;
        if *entry >= 3 {
            return true;
        }
    }
    false
}

fn assistant_response_has_execution_evidence(lower: &str) -> bool {
    [
        "file=",
        "path=",
        "cmd=",
        "exists_now=",
        "objective_state=",
        "error:",
        "blocked:",
        "blocker:",
        "run finished",
        "tested",
        "verified",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn detect_execution_required_intent(messages: &[Message]) -> bool {
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if user.trim().is_empty() {
        return false;
    }
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let combined = format!("{} {}", user, objective);
    let action_terms = [
        "proceed",
        "implement",
        "fix",
        "debug",
        "diagnose",
        "run",
        "test",
        "patch",
        "sync",
        "rebuild",
        "verify",
        "connect",
        "integrat",
        "investigate",
        "analyze",
        "review",
    ];
    let has_action = action_terms.iter().any(|needle| combined.contains(needle));
    let has_surface = combined.contains("repo")
        || combined.contains("repository")
        || combined.contains("codebase")
        || combined.contains("contextlattice")
        || combined.contains('/')
        || combined.contains(".rs")
        || combined.contains(".py")
        || combined.contains("session");
    has_action && has_surface
}

fn finalizer_action_execution_requires_retry(
    messages: &[Message],
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if retry_count >= FINALIZER_ACTION_EXECUTION_MAX_RETRIES {
        return false;
    }
    if !detect_execution_required_intent(messages) {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    if assistant_response_has_execution_evidence(&lower) {
        return false;
    }
    let deferral_markers = [
        "i will",
        "i'll",
        "let me",
        "i can",
        "i'm going to",
        "proceeding",
        "next i",
        "i should",
        "i would",
    ];
    deferral_markers.iter().any(|needle| lower.contains(needle))
}

fn detect_contextlattice_connect_intent(messages: &[Message]) -> bool {
    let Some(last_user) = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.as_deref())
    else {
        return false;
    };
    let lower = last_user.to_ascii_lowercase();
    if !lower.contains("contextlattice") {
        return false;
    }
    [
        "connect",
        "connection",
        "configure",
        "setup",
        "set up",
        "verify",
        "harden",
        "probe",
        "integrat",
        "health",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

