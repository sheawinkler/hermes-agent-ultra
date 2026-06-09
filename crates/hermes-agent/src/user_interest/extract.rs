//! Rule-based interest signal extraction from conversation text.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::Value;

use super::catalog::{scan_lang_signals, scan_tech_signals};
use super::contextual::extract_contextual_interests;
use super::declared::extract_declared_interests;
use super::store::InterestSignal;
use super::topic_id::stable_topic_id;
use super::types::{ExtractOptions, SignalSource};

lazy_static::lazy_static! {
    static ref PATH_RE: Regex = Regex::new(
        r#"(?i)(?:^|[\s"'`])([~/][\w./-]+|(?:crates|src|tests|docs)/[\w./-]+)"#,
    )
    .unwrap();
}

const STOPWORDS: &[&str] = &[
    "about", "after", "again", "also", "been", "before", "being", "could", "does", "doing",
    "done", "from", "have", "help", "here", "into", "just", "like", "make", "more", "need",
    "only", "please", "should", "some", "that", "their", "them", "then", "there", "these",
    "they", "this", "those", "through", "very", "want", "what", "when", "where", "which",
    "while", "will", "with", "would", "your", "你好", "请问", "怎么", "什么", "可以", "一下",
];

/// Tokens that must never become keyword/topic POI rows (roles, product surface, CLI meta).
const POI_TOKEN_BLOCKLIST: &[&str] = &[
    // Conversation / LLM roles
    "user", "assistant", "system", "tool", "tools", "human", "model", "models", "role", "roles",
    // Hermes product & memory surface
    "memory", "memories", "interest", "interests", "profile", "soul", "prompt", "prompts",
    "session", "sessions", "context", "provider", "providers", "config", "yaml", "gateway",
    "ultra", "agent", "agents", "message", "messages",
    "content", "token", "tokens", "response", "responses", "reply", "replies", "answer",
    // CLI / ops meta (often from `hermes interest list` etc.)
    "list", "status", "clear", "setup", "command", "commands", "show", "path", "database",
    "keyword", "topic", "language", "tags", "weight", "mode", "hybrid", "rules",
    // Generic engineering filler
    "file", "files", "code", "data", "error", "errors", "test", "tests", "true", "false",
    "null", "none", "string", "value", "values", "type", "types", "name", "names", "work",
    "working", "using", "used", "use", "run", "running", "please", "thanks", "thank",
    "hello", "okay", "ok", "yes", "yeah",
];

/// Minimum length for keyword POI (avoids short role/meta tokens).
const MIN_KEYWORD_LEN: usize = 5;

fn normalized_token(token: &str) -> String {
    token.trim().to_ascii_lowercase()
}

fn is_stopword(token: &str) -> bool {
    let lower = normalized_token(token);
    STOPWORDS.iter().any(|w| *w == lower.as_str()) || is_poi_blocklisted(&lower)
}

fn is_poi_blocklisted(token: &str) -> bool {
    let lower = normalized_token(token);
    if lower.is_empty() {
        return true;
    }
    POI_TOKEN_BLOCKLIST.iter().any(|w| *w == lower.as_str())
}

fn is_acceptable_keyword(token: &str) -> bool {
    let lower = normalized_token(token);
    if lower.len() < MIN_KEYWORD_LEN || lower.len() > 32 {
        return false;
    }
    if lower.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    !is_stopword(&lower)
}

fn is_acceptable_tech_topic(term: &str) -> bool {
    let lower = normalized_token(term);
    !lower.is_empty() && !is_poi_blocklisted(&lower)
}

fn is_acceptable_path(path: &str) -> bool {
    let p = path.trim();
    if p.len() < 8 {
        return false;
    }
    if !(p.contains('/') || p.starts_with('~') || p.contains('\\')) {
        return false;
    }
    let lower = p.trim_matches(|c| c == '/' || c == '\\').to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "src" | "docs" | "tests" | "crates" | "lib" | "bin" | "tmp" | "target"
    )
}

/// Append one POI signal; `canonical` drives stable `id`, `label` is for display.
pub(crate) fn push_signal(
    out: &mut Vec<InterestSignal>,
    namespace: &str,
    canonical: &str,
    label: &str,
    summary: &str,
    weight_delta: f64,
    tags: &[&str],
    source: SignalSource,
) {
    let label = label.trim();
    let canonical = canonical.trim();
    if label.len() < 2 || canonical.len() < 2 {
        return;
    }
    let id = stable_topic_id(namespace, canonical);
    if id.is_empty() || should_reject_signal_id(&id, label) {
        return;
    }
    out.push(InterestSignal::new(
        id,
        label.to_string(),
        summary.to_string(),
        weight_delta,
        tags.iter().map(|t| t.to_string()).collect(),
        source,
    ));
}

/// Reject ids/labels that are clearly not user interests (e.g. `keyword-user`).
fn should_reject_signal_id(id: &str, label: &str) -> bool {
    let id_lower = id.to_ascii_lowercase();
    let label_lower = label.to_ascii_lowercase();
    for prefix in ["keyword-", "topic-", "language-", "path-"] {
        if let Some(rest) = id_lower.strip_prefix(prefix) {
            if is_poi_blocklisted(rest) {
                return true;
            }
        }
    }
    if label_lower.starts_with("keyword: ") {
        let word = label_lower.trim_start_matches("keyword: ").trim();
        if !is_acceptable_keyword(word) {
            return true;
        }
    }
    if label_lower.starts_with("topic: ") {
        let term = label_lower.trim_start_matches("topic: ").trim();
        if !is_acceptable_tech_topic(term) {
            return true;
        }
    }
    false
}

/// Extract interest signals from free text (user message or transcript chunk).
pub fn extract_signals_from_text(
    text: &str,
    weight_scale: f64,
    options: ExtractOptions,
) -> Vec<InterestSignal> {
    let mut out = Vec::new();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return out;
    }

    let declared = extract_declared_interests(trimmed, weight_scale);
    let skip_keyword_pass = !declared.is_empty();
    out.extend(declared);
    out.extend(extract_contextual_interests(trimmed, weight_scale));
    // Task/domain POI is inferred semantically at session-end via LLM — not keyword rules.

    for cap in PATH_RE.captures_iter(trimmed) {
        if let Some(path) = cap.get(1) {
            let p = path.as_str().trim_matches(|c| c == '"' || c == '\'' || c == '`');
            if is_acceptable_path(p) {
                let short = p.chars().take(80).collect::<String>();
                push_signal(
                    &mut out,
                    "path",
                    &short,
                    &format!("path: {short}"),
                    &format!("User work touched path {p}"),
                    0.15 * weight_scale,
                    &["path"],
                    SignalSource::Path,
                );
            }
        }
    }

    out.extend(scan_lang_signals(trimmed, weight_scale));
    out.extend(scan_tech_signals(trimmed, weight_scale));

    if skip_keyword_pass || !options.include_keywords {
        return dedupe_signals(out);
    }

    let mut counts: HashMap<String, u32> = HashMap::new();
    for token in trimmed.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let token = token.trim();
        if !is_acceptable_keyword(token) {
            continue;
        }
        *counts.entry(normalized_token(token)).or_insert(0) += 1;
    }
    let mut ranked: Vec<_> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (word, count) in ranked.into_iter().take(8) {
        let min_count = if word.len() >= 6 { 1 } else { 2 };
        if count < min_count {
            continue;
        }
        push_signal(
            &mut out,
            "keyword",
            &word,
            &format!("keyword: {word}"),
            &format!("Frequent term \"{word}\" in user messages"),
            (0.08 * count as f64).min(0.35) * weight_scale,
            &["keyword"],
            SignalSource::Keyword,
        );
    }

    dedupe_signals(out)
}

fn dedupe_signals(signals: Vec<InterestSignal>) -> Vec<InterestSignal> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for sig in signals {
        if seen.insert(sig.id.clone()) {
            out.push(sig);
        }
    }
    out
}

/// Extract signals from serialized conversation messages (session end).
///
/// Only **user** messages are scanned so assistant/system narration (roles, memory
/// tool guidance, etc.) does not pollute POI rows.
pub fn extract_signals_from_messages(messages: &[Value]) -> Vec<InterestSignal> {
    let mut combined_user = String::new();
    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if !role.eq_ignore_ascii_case("user") {
            continue;
        }
        let content = message_text_from_value(msg);
        if content.is_empty() {
            continue;
        }
        combined_user.push_str(&content);
        combined_user.push('\n');
    }
    dedupe_signals(extract_signals_from_text(
        &combined_user,
        1.0,
        ExtractOptions {
            include_keywords: false,
        },
    ))
}

/// Extract plain text from a serialized chat message `content` field.
pub fn message_text_from_value(msg: &Value) -> String {
    if let Some(s) = msg.get("content").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(parts) = msg.get("content").and_then(|v| v.as_array()) {
        return parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
    }
    String::new()
}

/// Parse LLM JSON output into interest signals.
pub fn parse_llm_topics_json(raw: &str) -> Vec<InterestSignal> {
    let trimmed = raw.trim();
    let json_start = trimmed.find('[').or_else(|| trimmed.find('{'));
    let Some(start) = json_start else {
        return Vec::new();
    };
    let slice = &trimmed[start..];
    let value: Value = match serde_json::from_str(slice) {
        Ok(v) => v,
        Err(_) => {
            if let Some(end) = slice.rfind(']') {
                serde_json::from_str(&slice[..=end]).unwrap_or(Value::Null)
            } else {
                Value::Null
            }
        }
    };
    let items = if let Some(arr) = value.as_array() {
        arr.clone()
    } else if let Some(arr) = value.get("topics").and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let label = item
            .get("label")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(label) = label else {
            continue;
        };
        let summary = item
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or(label);
        let confidence = item
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.7)
            .clamp(0.1, 1.0);
        let mut tags: Vec<String> = item
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(domain_key) = item
            .get("domain_key")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !tags.iter().any(|t| t == domain_key) {
                tags.push(domain_key.to_string());
            }
            if !tags.iter().any(|t| t == "domain") {
                tags.push("domain".to_string());
            }
        }
        let id = stable_topic_id("llm", label);
        if id.is_empty() || should_reject_signal_id(&id, label) {
            continue;
        }
        let mut signal = InterestSignal::new(
            id,
            label.to_string(),
            summary.to_string(),
            0.25 * confidence,
            tags,
            SignalSource::Llm,
        );
        signal.confidence = confidence;
        out.push(signal);
    }
    dedupe_signals(out)
}

/// Whether an existing DB row should be treated as non-POI noise.
pub fn is_rejected_poi_topic(id: &str, label: &str) -> bool {
    should_reject_signal_id(id, label)
}

/// Drop signals that should not be persisted (shared by ingest path).
pub fn filter_poi_signals(signals: Vec<InterestSignal>) -> Vec<InterestSignal> {
    signals
        .into_iter()
        .filter(|s| !should_reject_signal_id(&s.id, &s.label))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_rust_and_hermes_keywords() {
        let signals = extract_signals_from_text(
            "Help me port hermes parity tests to Rust in crates/hermes-agent",
            1.0,
            ExtractOptions::default(),
        );
        let ids: HashSet<_> = signals.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains("tech:hermes") || ids.contains("tech:parity"));
        assert!(ids.iter().any(|id| id.contains("rust") || id.contains("path")));
    }

    #[test]
    fn rejects_role_and_product_meta_keywords() {
        let signals = extract_signals_from_text(
            "The user asked the assistant to update memory and interest list in config.yaml",
            1.0,
            ExtractOptions::default(),
        );
        let ids: HashSet<_> = signals.iter().map(|s| s.id.as_str()).collect();
        assert!(!ids.contains("keyword-user"));
        assert!(!ids.contains("keyword-assistant"));
        assert!(!ids.contains("keyword-memory"));
        assert!(!ids.contains("keyword-interest"));
        assert!(!ids.contains("keyword-list"));
        assert!(!ids.contains("tech:agent"));
    }

    #[test]
    fn session_extract_ignores_assistant_role_noise() {
        let messages = vec![
            serde_json::json!({"role": "assistant", "content": "I will use the memory tool for the user profile."}),
            serde_json::json!({"role": "user", "content": "Continue the Rust parity port in crates/hermes-parity-tests"}),
        ];
        let signals = extract_signals_from_messages(&messages);
        let ids: HashSet<_> = signals.iter().map(|s| s.id.as_str()).collect();
        assert!(!ids.contains("keyword-memory"));
        assert!(!ids.contains("keyword-user"));
        assert!(ids.contains("tech:parity") || ids.contains("lang:rust"));
    }

    #[test]
    fn chinese_declared_interests_do_not_collide() {
        let a = extract_signals_from_text("我的兴趣点是打篮球", 1.0, ExtractOptions::default());
        let b = extract_signals_from_text("我的兴趣点还有吃鱼", 1.0, ExtractOptions::default());
        let interest_a: Vec<_> = a.iter().filter(|s| s.id.starts_with("interest:")).collect();
        let interest_b: Vec<_> = b.iter().filter(|s| s.id.starts_with("interest:")).collect();
        assert_eq!(interest_a.len(), 1);
        assert_eq!(interest_b.len(), 1);
        assert_ne!(interest_a[0].id, interest_b[0].id);
    }

    #[test]
    fn chinese_contextual_and_tech_extraction() {
        let signals = extract_signals_from_text(
            "我最近在研究大模型应用和产品设计",
            1.0,
            ExtractOptions::default(),
        );
        let ids: HashSet<_> = signals.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.iter().any(|id| id.starts_with("interest:")));
        assert!(ids.contains("tech:llm"));
    }

    #[test]
    fn real_world_ashare_query_passes_turn_gate() {
        use super::super::quality::should_extract_user_turn;
        assert!(should_extract_user_turn(
            "帮我看看当前时间上A股行情怎么样了？",
            12,
        ));
    }

    #[test]
    fn real_world_finance_constraints_passes_turn_gate() {
        use super::super::quality::should_extract_user_turn;
        let text = "这30万是我的全部积蓄；投资期5年；要稳健点；没有稳定的收入来源；短期无大额支出计划；对股票有一定的了解";
        assert!(should_extract_user_turn(text, 12));
    }

    #[test]
    fn parse_llm_topics_json_array() {
        let raw = r#"Here are topics: [{"label":"Rust CLI","summary":"Building hermes-cli","confidence":0.9,"tags":["rust"]}]"#;
        let topics = parse_llm_topics_json(raw);
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].label, "Rust CLI");
    }
}
