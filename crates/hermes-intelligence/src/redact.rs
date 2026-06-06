//! Sensitive data redaction — removes API keys, passwords, emails, phones,
//! credit card numbers, and IP addresses from text and messages.
//!
//! Requirement 16.8

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

use hermes_core::Message;

// ---------------------------------------------------------------------------
// RedactionPattern
// ---------------------------------------------------------------------------

/// A named pattern for redacting sensitive data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionPattern {
    pub name: String,
    #[serde(
        serialize_with = "serialize_regex",
        deserialize_with = "deserialize_regex"
    )]
    pub regex: Regex,
    pub replacement: String,
}

fn serialize_regex<S: serde::Serializer>(regex: &Regex, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(regex.as_str())
}

fn deserialize_regex<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Regex, D::Error> {
    let s = String::deserialize(d)?;
    Regex::new(&s).map_err(serde::de::Error::custom)
}

// ---------------------------------------------------------------------------
// Built-in patterns
// ---------------------------------------------------------------------------

/// Built-in redaction patterns for common sensitive data.
static BUILTIN_PATTERNS: LazyLock<Vec<RedactionPattern>> = LazyLock::new(|| {
    vec![
        // API keys: sk-..., pk-..., key-..., etc.
        RedactionPattern {
            name: "api_key".into(),
            regex: Regex::new(r#"(?i)(sk-|pk-|key-|api[_-]?key[_\s]*[:=]\s*)[a-zA-Z0-9\-_]{20,}"#)
                .unwrap(),
            replacement: "[REDACTED_API_KEY]".into(),
        },
        // Generic secret tokens (Bearer, token, etc.)
        RedactionPattern {
            name: "auth_token".into(),
            regex: Regex::new(r#"(?i)(bearer\s+|token[_\s]*[:=]\s*)[a-zA-Z0-9\-_.]{20,}"#).unwrap(),
            replacement: "[REDACTED_TOKEN]".into(),
        },
        // Telegram bot tokens: "<digits>:<base64-ish token>"
        RedactionPattern {
            name: "telegram_bot_token".into(),
            regex: Regex::new(r#"\b\d{8,12}:[A-Za-z0-9_-]{30,}\b"#).unwrap(),
            replacement: "[REDACTED_TELEGRAM_TOKEN]".into(),
        },
        // Passwords in URLs or assignments
        RedactionPattern {
            name: "password".into(),
            regex: Regex::new(r#"(?i)(password|passwd|pwd)[_\s]*[:=]\s*["']?[^\s"']{4,}"#).unwrap(),
            replacement: "[REDACTED_PASSWORD]".into(),
        },
        // Passwords in URLs: ://user:pass@host
        RedactionPattern {
            name: "url_password".into(),
            regex: Regex::new(r#"://[^:]+:([^@]+)@"#).unwrap(),
            replacement: "://[REDACTED_USER]:[REDACTED]@".into(),
        },
        // Email addresses
        RedactionPattern {
            name: "email".into(),
            regex: Regex::new(r#"[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}"#).unwrap(),
            replacement: "[REDACTED_EMAIL]".into(),
        },
        // Phone numbers (US-style and international)
        RedactionPattern {
            name: "phone".into(),
            regex: Regex::new(r#"(?:\+?\d{1,3}[-.\s]?)?\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}"#)
                .unwrap(),
            replacement: "[REDACTED_PHONE]".into(),
        },
        // Credit card numbers (basic pattern for 13-19 digit numbers)
        RedactionPattern {
            name: "credit_card".into(),
            regex: Regex::new(r#"\b\d{4}[-\s]?\d{4}[-\s]?\d{4}[-\s]?\d{1,4}\b"#).unwrap(),
            replacement: "[REDACTED_CC]".into(),
        },
        // IP addresses (IPv4)
        RedactionPattern {
            name: "ipv4".into(),
            regex: Regex::new(r#"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b"#).unwrap(),
            replacement: "[REDACTED_IP]".into(),
        },
    ]
});

static SENSITIVE_QUERY_PARAMS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "token",
    "api_key",
    "apikey",
    "client_secret",
    "password",
    "auth",
    "jwt",
    "session",
    "secret",
    "key",
    "code",
    "signature",
    "x-amz-signature",
];

static SENSITIVE_BODY_KEYS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "token",
    "api_key",
    "apikey",
    "client_secret",
    "password",
    "auth",
    "jwt",
    "secret",
    "private_key",
    "authorization",
    "key",
];

static PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(^|[^A-Za-z0-9_-])(",
        r"sk-[A-Za-z0-9_-]{10,}",
        r"|ghp_[A-Za-z0-9]{10,}",
        r"|github_pat_[A-Za-z0-9_]{10,}",
        r"|gho_[A-Za-z0-9]{10,}",
        r"|ghu_[A-Za-z0-9]{10,}",
        r"|ghs_[A-Za-z0-9]{10,}",
        r"|ghr_[A-Za-z0-9]{10,}",
        r"|xox[baprs]-[A-Za-z0-9-]{10,}",
        r"|AIza[A-Za-z0-9_-]{30,}",
        r"|pplx-[A-Za-z0-9]{10,}",
        r"|fal_[A-Za-z0-9_-]{10,}",
        r"|fc-[A-Za-z0-9]{10,}",
        r"|bb_live_[A-Za-z0-9_-]{10,}",
        r"|gAAAA[A-Za-z0-9_=-]{20,}",
        r"|AKIA[A-Z0-9]{16}",
        r"|sk_live_[A-Za-z0-9]{10,}",
        r"|sk_test_[A-Za-z0-9]{10,}",
        r"|rk_live_[A-Za-z0-9]{10,}",
        r"|SG\.[A-Za-z0-9_-]{10,}",
        r"|hf_[A-Za-z0-9]{10,}",
        r"|r8_[A-Za-z0-9]{10,}",
        r"|npm_[A-Za-z0-9]{10,}",
        r"|pypi-[A-Za-z0-9_-]{10,}",
        r"|dop_v1_[A-Za-z0-9]{10,}",
        r"|doo_v1_[A-Za-z0-9]{10,}",
        r"|am_[A-Za-z0-9_-]{10,}",
        r"|sk_[A-Za-z0-9_]{10,}",
        r"|tvly-[A-Za-z0-9]{10,}",
        r"|exa_[A-Za-z0-9]{10,}",
        r"|gsk_[A-Za-z0-9]{10,}",
        r"|syt_[A-Za-z0-9]{10,}",
        r"|retaindb_[A-Za-z0-9]{10,}",
        r"|hsk-[A-Za-z0-9]{10,}",
        r"|mem0_[A-Za-z0-9]{10,}",
        r"|brv_[A-Za-z0-9]{10,}",
        r"|xai-[A-Za-z0-9]{30,}",
        r")"
    ))
    .unwrap()
});

static ENV_ASSIGN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?m)\b([A-Z0-9_]{0,50}(?:API_?KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|AUTH)[A-Z0-9_]{0,50})(\s*=\s*)(['"]?)([^\s'"]+)(['"]?)"#,
    )
    .unwrap()
});

static JSON_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)("(?:api_?key|apikey|token|secret|password|access_token|refresh_token|auth_token|bearer|secret_value|raw_secret|secret_input|key_material)")(\s*:\s*)"([^"]+)""#,
    )
    .unwrap()
});

static AUTH_HEADER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)(authorization:\s*bearer\s+)(\S+)"#).unwrap());

static TELEGRAM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(bot)?(\d{8,}):([-A-Za-z0-9_]{30,})"#).unwrap());

static PRIVATE_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)-----BEGIN[A-Z ]*PRIVATE KEY-----.*?-----END[A-Z ]*PRIVATE KEY-----"#)
        .unwrap()
});

static DB_CONNSTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)((?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis|amqp)://[^:]+:)([^@]+)(@)"#)
        .unwrap()
});

static JWT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"eyJ[A-Za-z0-9_-]{10,}(?:\.[A-Za-z0-9_=-]{4,}){0,2}"#).unwrap());

static DISCORD_MENTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<@!?(\d{17,20})>"#).unwrap());

static SIGNAL_PHONE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\+[1-9]\d{6,14}\b"#).unwrap());

static URL_WITH_QUERY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(https?|wss?|ftp)://([^\s/?#]+)([^\s?#]*)\?([^\s#]+)(#\S*)?"#).unwrap()
});

static URL_USERINFO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b(https?|wss?|ftp)://([^/\s:@]+):([^/\s@]+)@"#).unwrap());

static HTTP_REQUEST_TARGET_QUERY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b((?:GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS|TRACE|CONNECT)\s+[^ \t\r\n"']*?)\?([^ \t\r\n"']+)"#,
    )
    .unwrap()
});

static FORM_BODY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^[A-Za-z_][A-Za-z0-9_.-]*=[^&\s]*(?:&[A-Za-z_][A-Za-z0-9_.-]*=[^&\s]*)+$"#)
        .unwrap()
});

static PASSWORD_ASSIGN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(password|passwd|pwd)(\s*[:=]\s*)(['"]?)([^\s'"]{4,})(['"]?)"#).unwrap()
});

static EMAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}"#).unwrap());

static PHONE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:\+?\d{1,3}[-.\s]?)?\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}"#).unwrap()
});

static CREDIT_CARD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\b\d{4}[-\s]?\d{4}[-\s]?\d{4}[-\s]?\d{1,4}\b"#).unwrap());

static IPV4_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b"#).unwrap());

fn mask_token(value: &str) -> String {
    let char_count = value.chars().count();
    if value.is_empty() || char_count < 18 {
        "***".to_string()
    } else {
        let head = value.chars().take(6).collect::<String>();
        let tail = value
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        format!("{head}...{tail}")
    }
}

fn is_sensitive_key(key: &str, set: &[&str]) -> bool {
    set.iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(key))
}

fn redact_query_string(query: &str, sensitive_keys: &[&str]) -> String {
    query
        .split('&')
        .map(|pair| {
            let Some((key, _value)) = pair.split_once('=') else {
                return pair.to_string();
            };
            if is_sensitive_key(key, sensitive_keys) {
                format!("{key}=***")
            } else {
                pair.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn redact_form_body(text: &str) -> String {
    if text.is_empty() || text.contains('\n') || !text.contains('&') {
        return text.to_string();
    }
    let trimmed = text.trim();
    if !FORM_BODY_RE.is_match(trimmed) {
        return text.to_string();
    }
    redact_query_string(trimmed, SENSITIVE_BODY_KEYS)
}

fn same_pattern(left: &RedactionPattern, right: &RedactionPattern) -> bool {
    left.name == right.name
        && left.regex.as_str() == right.regex.as_str()
        && left.replacement == right.replacement
}

fn is_builtin_pattern(pattern: &RedactionPattern) -> bool {
    BUILTIN_PATTERNS
        .iter()
        .any(|builtin| same_pattern(pattern, builtin))
}

fn has_builtin_patterns(patterns: &[RedactionPattern]) -> bool {
    BUILTIN_PATTERNS.iter().all(|builtin| {
        patterns
            .iter()
            .any(|pattern| same_pattern(pattern, builtin))
    })
}

fn redact_builtin_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let mut result = text.to_string();

    result = PREFIX_RE
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            format!("{}{}", &caps[1], mask_token(&caps[2]))
        })
        .to_string();

    if result.contains('=') {
        result = ENV_ASSIGN_RE
            .replace_all(&result, |caps: &regex::Captures<'_>| {
                format!(
                    "{}{}{}{}{}",
                    &caps[1],
                    &caps[2],
                    &caps[3],
                    mask_token(&caps[4]),
                    &caps[5]
                )
            })
            .to_string();
    }

    if result.contains(':') && result.contains('"') {
        result = JSON_FIELD_RE
            .replace_all(&result, |caps: &regex::Captures<'_>| {
                format!("{}{}\"{}\"", &caps[1], &caps[2], mask_token(&caps[3]))
            })
            .to_string();
    }

    result = AUTH_HEADER_RE
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            format!("{}{}", &caps[1], mask_token(&caps[2]))
        })
        .to_string();

    result = TELEGRAM_RE
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            let bot = caps.get(1).map_or("", |m| m.as_str());
            format!("{}{}:***", bot, &caps[2])
        })
        .to_string();

    result = PRIVATE_KEY_RE
        .replace_all(&result, "[REDACTED PRIVATE KEY]")
        .to_string();

    result = DB_CONNSTR_RE
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            format!("{}***{}", &caps[1], &caps[3])
        })
        .to_string();

    result = JWT_RE
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            mask_token(caps.get(0).map_or("", |m| m.as_str()))
        })
        .to_string();

    result = URL_WITH_QUERY_RE
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            let fragment = caps.get(5).map_or("", |m| m.as_str());
            format!(
                "{}://{}{}?{}{}",
                &caps[1],
                &caps[2],
                &caps[3],
                redact_query_string(&caps[4], SENSITIVE_QUERY_PARAMS),
                fragment
            )
        })
        .to_string();

    result = URL_USERINFO_RE
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            format!("{}://{}:***@", &caps[1], &caps[2])
        })
        .to_string();

    result = HTTP_REQUEST_TARGET_QUERY_RE
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            format!(
                "{}?{}",
                &caps[1],
                redact_query_string(&caps[2], SENSITIVE_QUERY_PARAMS)
            )
        })
        .to_string();

    result = redact_form_body(&result);

    if !result.contains('&') {
        result = PASSWORD_ASSIGN_RE
            .replace_all(&result, "[REDACTED_PASSWORD]")
            .to_string();
    }

    result = DISCORD_MENTION_RE
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            if caps.get(0).map_or("", |m| m.as_str()).contains('!') {
                "<@!***>".to_string()
            } else {
                "<@***>".to_string()
            }
        })
        .to_string();

    result = SIGNAL_PHONE_RE
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            let phone = caps.get(0).map_or("", |m| m.as_str());
            if phone.len() <= 8 {
                format!("{}****{}", &phone[..2], &phone[phone.len() - 2..])
            } else {
                format!("{}****{}", &phone[..4], &phone[phone.len() - 4..])
            }
        })
        .to_string();

    result = CREDIT_CARD_RE
        .replace_all(&result, "[REDACTED_CC]")
        .to_string();
    result = EMAIL_RE
        .replace_all(&result, "[REDACTED_EMAIL]")
        .to_string();
    result = PHONE_RE
        .replace_all(&result, "[REDACTED_PHONE]")
        .to_string();
    result = IPV4_RE.replace_all(&result, "[REDACTED_IP]").to_string();

    result
}

/// Apply the default Hermes secret redaction policy to text.
pub fn redact_sensitive_text<T: ToString>(text: T) -> String {
    redact_builtin_text(&text.to_string())
}

// ---------------------------------------------------------------------------
// Sensitive file path patterns
// ---------------------------------------------------------------------------

/// Patterns that indicate a file path contains sensitive data.
static SENSITIVE_FILE_PATTERNS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    vec![
        ".env",
        ".env.local",
        ".env.production",
        ".env.staging",
        "credentials",
        "secrets",
        "secret_key",
        "private_key",
        "id_rsa",
        "id_ed25519",
        ".pem",
        ".key",
        ".p12",
        ".pfx",
        "password",
        "passwd",
        "shadow",
        "htpasswd",
        ".htpasswd",
        "aws_credentials",
        ".aws/credentials",
        "service_account",
        "gcloud",
        "kubeconfig",
    ]
});

// ---------------------------------------------------------------------------
// Redactor
// ---------------------------------------------------------------------------

/// Redacts sensitive data from text and messages.
#[derive(Debug, Clone)]
pub struct Redactor {
    pub patterns: Vec<RedactionPattern>,
}

impl Redactor {
    /// Create a new redactor with built-in patterns.
    pub fn new() -> Self {
        Self {
            patterns: BUILTIN_PATTERNS.clone(),
        }
    }

    /// Create a redactor with no patterns (pass-through).
    pub fn empty() -> Self {
        Self {
            patterns: Vec::new(),
        }
    }

    /// Add a custom redaction pattern.
    pub fn add_pattern(&mut self, pattern: RedactionPattern) {
        self.patterns.push(pattern);
    }

    /// Redact sensitive data from a text string.
    pub fn redact(&self, text: &str) -> String {
        if self.patterns.is_empty() {
            return text.to_string();
        }

        let uses_builtins = has_builtin_patterns(&self.patterns);
        let mut result = if uses_builtins {
            redact_builtin_text(text)
        } else {
            text.to_string()
        };

        for pattern in &self.patterns {
            if uses_builtins && is_builtin_pattern(pattern) {
                continue;
            }
            result = pattern
                .regex
                .replace_all(&result, pattern.replacement.as_str())
                .to_string();
        }
        result
    }

    /// Redact sensitive data from a message, returning a new message.
    ///
    /// Only redacts the content field; preserves role, tool_calls, etc.
    pub fn redact_message(&self, message: &Message) -> Message {
        Message {
            role: message.role,
            content: message.content.as_ref().map(|c| self.redact(c)),
            tool_calls: message.tool_calls.clone(),
            tool_call_id: message.tool_call_id.clone(),
            name: message.name.clone(),
            reasoning_content: message.reasoning_content.as_ref().map(|c| self.redact(c)),
            cache_control: message.cache_control.clone(),
        }
    }

    /// Redact an entire conversation (list of messages).
    pub fn redact_messages(&self, messages: &[Message]) -> Vec<Message> {
        messages.iter().map(|m| self.redact_message(m)).collect()
    }

    /// Check if a file path is likely to contain sensitive data.
    pub fn is_sensitive_file(path: &str) -> bool {
        let lower = path.to_lowercase();
        SENSITIVE_FILE_PATTERNS.iter().any(|p| lower.contains(p))
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::MessageRole;

    #[test]
    fn test_redact_api_key() {
        let redactor = Redactor::new();
        let text = "My key is sk-abc123def456ghi789jkl012mno345";
        let result = redactor.redact(text);
        assert!(result.contains("sk-abc"));
        assert!(result.contains("..."));
        assert!(!result.contains("def456ghi789"));
    }

    #[test]
    fn test_redact_email() {
        let redactor = Redactor::new();
        let text = "Contact me at user@example.com for details";
        let result = redactor.redact(text);
        assert!(result.contains("[REDACTED_EMAIL]"));
        assert!(!result.contains("user@example.com"));
    }

    #[test]
    fn test_redact_phone() {
        let redactor = Redactor::new();
        let text = "Call me at 555-123-4567";
        let result = redactor.redact(text);
        assert!(result.contains("[REDACTED_PHONE]"));
    }

    #[test]
    fn test_redact_credit_card() {
        let redactor = Redactor::new();
        let text = "Card: 4111 1111 1111 1111";
        let result = redactor.redact(text);
        assert!(result.contains("[REDACTED_CC]"));
    }

    #[test]
    fn test_redact_ipv4() {
        let redactor = Redactor::new();
        let text = "Server at 192.168.1.100 is down";
        let result = redactor.redact(text);
        assert!(result.contains("[REDACTED_IP]"));
        assert!(!result.contains("192.168.1.100"));
    }

    #[test]
    fn test_redact_password() {
        let redactor = Redactor::new();
        let text = "password = mysecretpassword123";
        let result = redactor.redact(text);
        assert!(result.contains("[REDACTED_PASSWORD]"));
        assert!(!result.contains("mysecretpassword123"));
    }

    #[test]
    fn test_redact_telegram_bot_token() {
        let redactor = Redactor::new();
        let text = "TELEGRAM_BOT_TOKEN=123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcd1234";
        let result = redactor.redact(text);
        assert!(result.contains("TELEGRAM_BOT_TOKEN="));
        assert!(!result.contains("123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcd1234"));
    }

    #[test]
    fn test_redact_message() {
        let redactor = Redactor::new();
        let msg = Message::user(
            "My email is test@example.com and my key is sk-abc123def456ghi789jkl012mno345",
        );
        let redacted = redactor.redact_message(&msg);
        assert_eq!(redacted.role, MessageRole::User);
        let content = redacted.content.unwrap();
        assert!(content.contains("[REDACTED_EMAIL]"));
        assert!(!content.contains("def456ghi789"));
    }

    #[test]
    fn test_redact_known_vendor_prefixes() {
        let redactor = Redactor::new();
        let slack_token = format!("{}-{}-{}", "xoxb", "0".repeat(12), "a".repeat(14));
        let text = format!(
            "{} {}",
            concat!(
                "ghp_abc123def456ghi789jkl ",
                "github_pat_abc123def456ghi789jklmno ",
                "AIzaSyB-abc123def456ghi789jklmno012345 ",
                "pplx-abcdef123456789012345 ",
                "fal_abc123def456ghi789jkl ",
                "sk_abc123def456ghi789jklmnopqrstu ",
                "tvly-ABCdef123456789GHIJKL0000 ",
                "exa_XYZ789abcdef000000000000000"
            ),
            slack_token
        );
        let result = redactor.redact(&text);
        assert!(!result.contains("abc123def456"));
        assert!(!result.contains("aaaaaaaaaaaaaa"));
        assert!(!result.contains("ABCdef123456789"));
        assert!(!result.contains("XYZ789abcdef"));
    }

    #[test]
    fn test_short_secret_is_fully_masked() {
        let result = redact_sensitive_text("key=sk-short1234567");
        assert!(result.contains("***"));
        assert!(!result.contains("short1234567"));
    }

    #[test]
    fn test_env_assignments_preserve_non_secrets_and_code() {
        assert_eq!(redact_sensitive_text("HOME=/home/user"), "HOME=/home/user");
        assert_eq!(
            redact_sensitive_text("PATH=/usr/local/bin:/usr/bin"),
            "PATH=/usr/local/bin:/usr/bin"
        );
        assert_eq!(
            redact_sensitive_text("before_tokens = response.usage.prompt_tokens"),
            "before_tokens = response.usage.prompt_tokens"
        );
        assert_eq!(
            redact_sensitive_text("api_key = config.get('api_key')"),
            "api_key = config.get('api_key')"
        );
        assert_eq!(
            redact_sensitive_text("const token = await getToken();"),
            "const token = await getToken();"
        );

        let result = redact_sensitive_text("export SECRET_TOKEN=mypassword");
        assert!(result.starts_with("export "));
        assert!(result.contains("SECRET_TOKEN="));
        assert!(!result.contains("mypassword"));
    }

    #[test]
    fn test_json_auth_jwt_and_discord_redaction() {
        let json = redact_sensitive_text(r#"{"apiKey": "sk-proj-abc123def456ghi789jkl012"}"#);
        assert!(!json.contains("abc123def456"));

        let auth = redact_sensitive_text("authorization: bearer mytoken123456789012345678");
        assert!(auth.contains("authorization: bearer"));
        assert!(!auth.contains("mytoken12345"));

        let jwt = redact_sensitive_text(
            "Token: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc123def456ghij",
        );
        assert!(jwt.contains("Token:"));
        assert!(!jwt.contains("eyJzdWIi"));
        assert!(!jwt.contains("abc123def456"));

        assert_eq!(
            redact_sensitive_text("Hello <@222589316709220353>"),
            "Hello <@***>"
        );
        assert_eq!(
            redact_sensitive_text("Ping <@!1331549159177846844>"),
            "Ping <@!***>"
        );
        assert_eq!(redact_sensitive_text("<@12345>"), "<@12345>");
    }

    #[test]
    fn test_url_query_userinfo_and_form_redaction() {
        let oauth = redact_sensitive_text(
            "GET https://api.example.com/oauth/cb?code=abc123xyz789&state=csrf_ok",
        );
        assert!(oauth.contains("code=***"));
        assert!(oauth.contains("state=csrf_ok"));
        assert!(!oauth.contains("abc123xyz789"));

        let query =
            redact_sensitive_text("https://example.com/cb?token_count=42&session_id=xyz&foo=bar");
        assert!(query.contains("token_count=42"));
        assert!(query.contains("session_id=xyz"));

        let userinfo = redact_sensitive_text("https://user:supersecretpw@host.example.com/path");
        assert!(userinfo.contains("https://user:***@host.example.com"));
        assert!(!userinfo.contains("supersecretpw"));

        let db = redact_sensitive_text("postgres://admin:dbpass@db.internal:5432/app");
        assert!(db.contains("postgres://admin:***@db.internal"));
        assert!(!db.contains("dbpass"));

        let form = redact_sensitive_text("password=mysecret&username=bob&token=opaqueValue");
        assert!(form.contains("password=***"));
        assert!(form.contains("token=***"));
        assert!(form.contains("username=bob"));
        assert!(!form.contains("mysecret"));
        assert!(!form.contains("opaqueValue"));

        let sentence = "I have password=foo and other things";
        assert_eq!(redact_sensitive_text(sentence), sentence);
    }

    #[test]
    fn test_sensitive_file_detection() {
        assert!(Redactor::is_sensitive_file(".env"));
        assert!(Redactor::is_sensitive_file("config/.env.local"));
        assert!(Redactor::is_sensitive_file("/home/user/.ssh/id_rsa"));
        assert!(Redactor::is_sensitive_file("secrets/production.yaml"));
        assert!(Redactor::is_sensitive_file("credentials.json"));
        assert!(Redactor::is_sensitive_file("server.key"));
        assert!(!Redactor::is_sensitive_file("src/main.rs"));
        assert!(!Redactor::is_sensitive_file("README.md"));
        assert!(!Redactor::is_sensitive_file("Cargo.toml"));
    }

    #[test]
    fn test_custom_pattern() {
        let mut redactor = Redactor::empty();
        redactor.add_pattern(RedactionPattern {
            name: "custom_id".into(),
            regex: Regex::new(r#"CUSTOM-\d{5}"#).unwrap(),
            replacement: "[REDACTED_ID]".into(),
        });
        let text = "My ID is CUSTOM-12345";
        let result = redactor.redact(text);
        assert!(result.contains("[REDACTED_ID]"));
        assert!(!result.contains("CUSTOM-12345"));
    }

    #[test]
    fn test_empty_redactor() {
        let redactor = Redactor::empty();
        let text = "Nothing to redact here";
        assert_eq!(redactor.redact(text), text);
    }
}
