//! Passive coding verification evidence.
//!
//! Terminal commands record bounded results here after execution. Consumers can
//! query the best known status for a workspace/session without running checks.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

const MAX_OUTPUT_PREVIEW_BYTES: usize = 4_000;
const LEDGER_RELATIVE_PATH: &[&str] = &["verification", "terminal_evidence.jsonl"];

const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "Makefile",
    "AGENTS.md",
    "CLAUDE.md",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationEvidence {
    pub command: String,
    pub canonical_command: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub exit_code: i32,
    pub output_preview: String,
    pub recorded_at: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationStatus {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<VerificationEvidence>,
}

pub fn verification_ledger_path() -> PathBuf {
    LEDGER_RELATIVE_PATH
        .iter()
        .fold(hermes_config::hermes_home(), |path, part| path.join(part))
}

pub fn canonical_command(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn output_preview(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_PREVIEW_BYTES {
        return output.to_string();
    }
    let mut end = MAX_OUTPUT_PREVIEW_BYTES.min(output.len());
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n...(truncated)", output[..end].trim_end())
}

fn command_scope(command: &str) -> &'static str {
    let lower = command.to_ascii_lowercase();
    let full_markers = [
        "cargo test",
        "cargo check",
        "cargo build",
        "cargo fmt",
        "pytest",
        "npm run test",
        "npm test",
        "pnpm run test",
        "pnpm test",
        "yarn test",
        "bun test",
        "make test",
        "make check",
        "go test",
        "mvn test",
        "gradle test",
        "lint",
        "typecheck",
    ];
    if full_markers.iter().any(|marker| lower.contains(marker)) {
        "full"
    } else {
        "targeted"
    }
}

fn canonical_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn workspace_root_from(start: &Path) -> Option<PathBuf> {
    let start = canonical_path(start);
    start.ancestors().find_map(|candidate| {
        PROJECT_MARKERS
            .iter()
            .any(|marker| candidate.join(marker).exists())
            .then(|| candidate.to_path_buf())
    })
}

fn cwd_or_current(cwd: Option<&Path>) -> PathBuf {
    cwd.map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn record_terminal_result(
    command: &str,
    cwd: Option<&Path>,
    session_id: Option<&str>,
    exit_code: i32,
    output: &str,
) -> std::io::Result<VerificationEvidence> {
    let cwd = canonical_path(&cwd_or_current(cwd));
    let canonical = canonical_command(command);
    let evidence = VerificationEvidence {
        command: command.to_string(),
        canonical_command: canonical.clone(),
        cwd: cwd.display().to_string(),
        session_id: session_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        exit_code,
        output_preview: output_preview(output),
        recorded_at: Utc::now().to_rfc3339(),
        scope: command_scope(&canonical).to_string(),
    };

    let path = verification_ledger_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, &evidence).map_err(std::io::Error::other)?;
    file.write_all(b"\n")?;
    Ok(evidence)
}

pub fn verification_status(session_id: Option<&str>, cwd: Option<&Path>) -> VerificationStatus {
    let cwd = cwd_or_current(cwd);
    let Some(root) = workspace_root_from(&cwd) else {
        return VerificationStatus {
            status: "not_applicable".to_string(),
            evidence: None,
        };
    };
    let query_session = session_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let path = verification_ledger_path();
    let Ok(file) = std::fs::File::open(path) else {
        return VerificationStatus {
            status: "unknown".to_string(),
            evidence: None,
        };
    };

    let mut best = None;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(evidence) = serde_json::from_str::<VerificationEvidence>(&line) else {
            continue;
        };
        let evidence_cwd = PathBuf::from(&evidence.cwd);
        if workspace_root_from(&evidence_cwd).as_deref() != Some(root.as_path()) {
            continue;
        }
        if let Some(query_session) = query_session.as_deref() {
            if evidence.session_id.as_deref() != Some(query_session) {
                continue;
            }
        }
        best = Some(evidence);
    }

    match best {
        Some(evidence) => VerificationStatus {
            status: if evidence.exit_code == 0 {
                "passed".to_string()
            } else {
                "failed".to_string()
            },
            evidence: Some(evidence),
        },
        None => VerificationStatus {
            status: "unknown".to_string(),
            evidence: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex};

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn records_and_reads_passive_verification_status() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        let _home = EnvGuard::set("HERMES_HOME", home.to_str().unwrap());

        let evidence =
            record_terminal_result(" cargo   test ", Some(&repo), Some("sid"), 0, "green").unwrap();

        assert_eq!(evidence.canonical_command, "cargo test");
        assert_eq!(evidence.scope, "full");

        let status = verification_status(Some("sid"), Some(&repo));
        assert_eq!(status.status, "passed");
        assert_eq!(
            status
                .evidence
                .as_ref()
                .map(|ev| ev.output_preview.as_str()),
            Some("green")
        );
    }

    #[test]
    fn reports_not_applicable_outside_workspace() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let _home = EnvGuard::set("HERMES_HOME", home.to_str().unwrap());

        let status = verification_status(Some("sid"), Some(&plain));

        assert_eq!(status.status, "not_applicable");
        assert!(status.evidence.is_none());
    }
}
