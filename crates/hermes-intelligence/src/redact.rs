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
        let mut result = text.to_string();
        for pattern in &self.patterns {
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
        assert!(result.contains("[REDACTED_API_KEY]"));
        assert!(!result.contains("sk-abc123"));
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
        assert!(result.contains("[REDACTED_TELEGRAM_TOKEN]"));
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
        assert!(content.contains("[REDACTED_API_KEY]"));
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
