//! Command approval system
//!
//! Checks whether a terminal command requires explicit user approval
//! before execution, based on dangerous command patterns.

use regex::Regex;
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// ApprovalDecision
// ---------------------------------------------------------------------------

/// Decision from the approval check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Command is safe to execute without confirmation.
    Approved,
    /// Command is denied outright.
    Denied,
    /// Command requires user confirmation before execution.
    RequiresConfirmation,
}

// ---------------------------------------------------------------------------
// Dangerous patterns
// ---------------------------------------------------------------------------

/// Patterns that are always denied.
static DENIED_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)\brm\s+(-\w*r\w*f\w*|-\w*f\w*r\w*)\s").unwrap(),
        Regex::new(r"(?i)\brm\s+(-\w*r\w*f\w*|-\w*f\w*r\w*)\s*/").unwrap(),
        Regex::new(r"(?i)\brm\s+--no-preserve-root\s").unwrap(),
        Regex::new(r"(?i)\bmkfs\b").unwrap(),
        Regex::new(r"(?i)\bdd\s+.*of=/dev/").unwrap(),
        Regex::new(r"(?i):()\s*>\s*/dev/").unwrap(),
        Regex::new(r"(?i)>\s*/dev/sd[a-z]").unwrap(),
        Regex::new(r"(?i)chmod\s+777\s").unwrap(),
        Regex::new(r"(?i)chmod\s+-R\s+777\s").unwrap(),
    ]
});

/// Patterns that require confirmation.
static CONFIRM_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // sudo commands
        Regex::new(r"(?i)\bsudo\b").unwrap(),
        // rm -r (but not rm -rf which is denied)
        Regex::new(r"(?i)\brm\s+-[^f]*r").unwrap(),
        // System service manipulation
        Regex::new(r"(?i)\bsystemctl\s+(start|stop|restart|enable|disable)\s").unwrap(),
        // Package management
        Regex::new(r"(?i)\b(apt|apt-get|yum|dnf|pacman|brew)\s+(install|remove|purge)\b").unwrap(),
        // Network configuration
        Regex::new(r"(?i)\biptables\b").unwrap(),
        Regex::new(r"(?i)\bifconfig\s").unwrap(),
        // Process killing
        Regex::new(r"(?i)\bkill\s+-9\b").unwrap(),
        Regex::new(r"(?i)\bkillall\b").unwrap(),
        // Disk operations
        Regex::new(r"(?i)\bformat\b").unwrap(),
        // Cron modifications
        Regex::new(r"(?i)\bcrontab\s+-r\b").unwrap(),
        // Shell pipe to sh
        Regex::new(r"\|\s*(ba)?sh\b").unwrap(),
        // Curl pipe to shell
        Regex::new(r"(?i)curl\s+.*\|\s*(ba)?sh").unwrap(),
        // Writing to system directories
        Regex::new(r"(?i)>\s*/etc/").unwrap(),
        Regex::new(r"(?i)>\s*/usr/").unwrap(),
        // Docker operations that affect system
        Regex::new(r"(?i)\bdocker\s+(rm|rmi|system\s+prune)\b").unwrap(),
        // Git force push
        Regex::new(r"(?i)\bgit\s+push\s+.*--force").unwrap(),
        Regex::new(r"(?i)\bgit\s+push\s+-f\b").unwrap(),
    ]
});

// ---------------------------------------------------------------------------
// ApprovalManager
// ---------------------------------------------------------------------------

/// Manages command approval checks.
pub struct ApprovalManager {
    /// Custom denied patterns (compiled regexes).
    denied_patterns: Vec<Regex>,
    /// Custom confirm patterns (compiled regexes).
    confirm_patterns: Vec<Regex>,
}

impl ApprovalManager {
    /// Create a new ApprovalManager with built-in patterns.
    pub fn new() -> Self {
        Self {
            denied_patterns: Vec::new(),
            confirm_patterns: Vec::new(),
        }
    }

    /// Add a custom denied pattern.
    pub fn add_denied_pattern(&mut self, pattern: &str) -> Result<(), regex::Error> {
        let re = Regex::new(pattern)?;
        self.denied_patterns.push(re);
        Ok(())
    }

    /// Add a custom confirm-required pattern.
    pub fn add_confirm_pattern(&mut self, pattern: &str) -> Result<(), regex::Error> {
        let re = Regex::new(pattern)?;
        self.confirm_patterns.push(re);
        Ok(())
    }

    /// Check whether a command requires approval.
    ///
    /// Returns:
    /// - `Denied` if the command matches a denied pattern
    /// - `RequiresConfirmation` if the command matches a confirm pattern
    /// - `Approved` if no patterns match
    pub fn check_approval(&self, command: &str) -> ApprovalDecision {
        // Check denied patterns first (built-in then custom)
        for re in DENIED_PATTERNS.iter() {
            if re.is_match(command) {
                return ApprovalDecision::Denied;
            }
        }
        for re in &self.denied_patterns {
            if re.is_match(command) {
                return ApprovalDecision::Denied;
            }
        }

        // Check confirm patterns (built-in then custom)
        for re in CONFIRM_PATTERNS.iter() {
            if re.is_match(command) {
                return ApprovalDecision::RequiresConfirmation;
            }
        }
        for re in &self.confirm_patterns {
            if re.is_match(command) {
                return ApprovalDecision::RequiresConfirmation;
            }
        }

        ApprovalDecision::Approved
    }

    /// Async version of check_approval (same logic, for trait compatibility).
    pub async fn check_approval_async(&self, command: &str) -> ApprovalDecision {
        self.check_approval(command)
    }
}

impl Default for ApprovalManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience function: check if a command requires approval.
pub fn check_approval(command: &str) -> ApprovalDecision {
    let manager = ApprovalManager::new();
    manager.check_approval(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_approved_commands() {
        assert_eq!(check_approval("ls -la"), ApprovalDecision::Approved);
        assert_eq!(check_approval("echo hello"), ApprovalDecision::Approved);
        assert_eq!(check_approval("cat file.txt"), ApprovalDecision::Approved);
        assert_eq!(check_approval("git status"), ApprovalDecision::Approved);
    }

    #[test]
    fn test_denied_commands() {
        assert_eq!(check_approval("rm -rf /"), ApprovalDecision::Denied);
        assert_eq!(check_approval("rm -fr /home"), ApprovalDecision::Denied);
        assert_eq!(check_approval("mkfs.ext4 /dev/sda1"), ApprovalDecision::Denied);
        assert_eq!(check_approval("chmod 777 /etc/passwd"), ApprovalDecision::Denied);
    }

    #[test]
    fn test_requires_confirmation() {
        assert_eq!(check_approval("sudo apt install something"), ApprovalDecision::RequiresConfirmation);
        assert_eq!(check_approval("systemctl restart nginx"), ApprovalDecision::RequiresConfirmation);
        assert_eq!(check_approval("kill -9 1234"), ApprovalDecision::RequiresConfirmation);
    }

    #[test]
    fn test_custom_patterns() {
        let mut manager = ApprovalManager::new();
        manager.add_denied_pattern(r"(?i)\bdangerous_cmd\b").unwrap();
        manager.add_confirm_pattern(r"(?i)\bcautious_cmd\b").unwrap();

        assert_eq!(manager.check_approval("dangerous_cmd"), ApprovalDecision::Denied);
        assert_eq!(manager.check_approval("cautious_cmd"), ApprovalDecision::RequiresConfirmation);
        assert_eq!(manager.check_approval("safe_cmd"), ApprovalDecision::Approved);
    }
}