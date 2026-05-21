//! Skills guard — security scanner for externally-sourced skills (Python parity).
//!
//! Ported from `tools/skills_guard.py` in upstream hermes-agent.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Trust policy (mirrors Python INSTALL_POLICY / TRUSTED_REPOS)
// ---------------------------------------------------------------------------

pub const TRUSTED_REPOS: &[&str] = &[
    "openai/skills",
    "anthropics/skills",
    "huggingface/skills",
];

/// Install decision aligned with Python `(allowed, reason)` where `allowed` may be `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallDecision {
    Allowed,
    Blocked,
    NeedsConfirmation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    pub pattern_id: String,
    pub severity: String,
    pub category: String,
    pub file: String,
    pub line: u32,
    pub match_text: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanResult {
    pub skill_name: String,
    pub source: String,
    pub trust_level: String,
    pub verdict: String,
    pub findings: Vec<Finding>,
    pub scanned_at: String,
    pub summary: String,
}

#[derive(Debug, Deserialize)]
struct ThreatPatternDef {
    regex: String,
    id: String,
    severity: String,
    category: String,
    desc: String,
}

struct CompiledThreatPattern {
    regex: Regex,
    id: String,
    severity: String,
    category: String,
    description: String,
}

static COMPILED_PATTERNS: OnceLock<Vec<CompiledThreatPattern>> = OnceLock::new();

const THREAT_PATTERNS_JSON: &str = include_str!("../threat_patterns.json");

const MAX_FILE_COUNT: usize = 50;
const MAX_TOTAL_SIZE_KB: usize = 1024;
const MAX_SINGLE_FILE_KB: usize = 256;

const SCANNABLE_EXTENSIONS: &[&str] = &[
    ".md", ".txt", ".py", ".sh", ".bash", ".js", ".ts", ".rb", ".yaml", ".yml", ".json", ".toml",
    ".cfg", ".ini", ".conf", ".html", ".css", ".xml", ".tex", ".r", ".jl", ".pl", ".php",
];

const SUSPICIOUS_BINARY_EXTENSIONS: &[&str] = &[
    ".exe", ".dll", ".so", ".dylib", ".bin", ".dat", ".com", ".msi", ".dmg", ".app", ".deb",
    ".rpm",
];

const INVISIBLE_CHARS: &[char] = &[
    '\u{200b}', '\u{200c}', '\u{200d}', '\u{2060}', '\u{2062}', '\u{2063}', '\u{2064}',
    '\u{feff}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}',
    '\u{2067}', '\u{2068}', '\u{2069}',
];

fn compiled_patterns() -> &'static [CompiledThreatPattern] {
    COMPILED_PATTERNS.get_or_init(|| {
        let defs: Vec<ThreatPatternDef> =
            serde_json::from_str(THREAT_PATTERNS_JSON).expect("threat_patterns.json valid");
        defs.into_iter()
            .filter_map(|d| {
                let re = Regex::new(&format!("(?i){}", d.regex)).ok()?;
                Some(CompiledThreatPattern {
                    regex: re,
                    id: d.id,
                    severity: d.severity,
                    category: d.category,
                    description: d.desc,
                })
            })
            .collect()
    })
}

/// Map a source identifier to trust level (Python `_resolve_trust_level`).
pub fn resolve_trust_level(source: &str) -> String {
    let mut normalized = source.trim();
    for prefix in [
        "skills-sh/",
        "skills.sh/",
        "skils-sh/",
        "skils.sh/",
    ] {
        if let Some(rest) = normalized.strip_prefix(prefix) {
            normalized = rest;
            break;
        }
    }
    if normalized == "agent-created" {
        return "agent-created".into();
    }
    if normalized == "official" || normalized.starts_with("official/") {
        return "builtin".into();
    }
    for trusted in TRUSTED_REPOS {
        if normalized == *trusted || normalized.starts_with(&format!("{trusted}/")) {
            return "trusted".into();
        }
    }
    "community".into()
}

/// Determine overall verdict from findings (Python `_determine_verdict`).
pub fn determine_verdict(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "safe".into();
    }
    if findings.iter().any(|f| f.severity == "critical") {
        return "dangerous".into();
    }
    if findings.iter().any(|f| f.severity == "high") {
        return "caution".into();
    }
    "caution".into()
}

/// Whether install is allowed (Python `should_allow_install`).
pub fn should_allow_install(result: &ScanResult, force: bool) -> (InstallDecision, String) {
    let policy = match result.trust_level.as_str() {
        "builtin" => ["allow", "allow", "allow"],
        "trusted" => ["allow", "allow", "block"],
        "agent-created" => ["allow", "allow", "ask"],
        _ => ["allow", "block", "block"],
    };
    let vi = match result.verdict.as_str() {
        "safe" => 0,
        "caution" => 1,
        _ => 2,
    };
    let decision = policy[vi];
    match decision {
        "allow" => (
            InstallDecision::Allowed,
            format!(
                "Allowed ({} source, {} verdict)",
                result.trust_level, result.verdict
            ),
        ),
        "ask" if force => (
            InstallDecision::Allowed,
            format!(
                "Force-installed despite {} verdict ({} findings)",
                result.verdict,
                result.findings.len()
            ),
        ),
        "ask" => (
            InstallDecision::NeedsConfirmation,
            format!(
                "Requires confirmation ({} source + {} verdict, {} findings)",
                result.trust_level,
                result.verdict,
                result.findings.len()
            ),
        ),
        _ if force => (
            InstallDecision::Allowed,
            format!(
                "Force-installed despite {} verdict ({} findings)",
                result.verdict,
                result.findings.len()
            ),
        ),
        _ => (
            InstallDecision::Blocked,
            format!(
                "Blocked ({} source + {} verdict, {} findings). Use --force to override.",
                result.trust_level,
                result.verdict,
                result.findings.len()
            ),
        ),
    }
}

fn is_scannable(rel_path: &str) -> bool {
    let path = Path::new(rel_path);
    if path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
        return true;
    }
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| {
            let lower = format!(".{}", ext.to_ascii_lowercase());
            SCANNABLE_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

/// Scan inline text as a file (Python `scan_file` without disk I/O).
pub fn scan_content(rel_path: &str, content: &str) -> Vec<Finding> {
    if !is_scannable(rel_path) {
        return Vec::new();
    }
    let mut findings = Vec::new();
    let mut seen = HashSet::new();
    let lines: Vec<&str> = content.lines().collect();
    for pat in compiled_patterns() {
        for (i, line) in lines.iter().enumerate() {
            let line_no = (i + 1) as u32;
            let key = (pat.id.as_str(), line_no);
            if seen.contains(&key) {
                continue;
            }
            if pat.regex.is_match(line) {
                seen.insert(key);
                let mut matched = line.trim().to_string();
                if matched.len() > 120 {
                    matched.truncate(117);
                    matched.push_str("...");
                }
                findings.push(Finding {
                    pattern_id: pat.id.clone(),
                    severity: pat.severity.clone(),
                    category: pat.category.clone(),
                    file: rel_path.to_string(),
                    line: line_no,
                    match_text: matched,
                    description: pat.description.clone(),
                });
            }
        }
    }
    for (i, line) in lines.iter().enumerate() {
        for ch in INVISIBLE_CHARS {
            if line.contains(*ch) {
                let name = unicode_char_name(*ch);
                findings.push(Finding {
                    pattern_id: "invisible_unicode".into(),
                    severity: "high".into(),
                    category: "injection".into(),
                    file: rel_path.to_string(),
                    line: (i + 1) as u32,
                    match_text: format!("U+{:04X} ({name})", *ch as u32),
                    description: format!(
                        "invisible unicode character {name} (possible text hiding/injection)"
                    ),
                });
                break;
            }
        }
    }
    findings
}

/// Scan all files under a skill directory (Python `scan_skill`).
pub fn scan_skill(skill_path: &Path, source: &str) -> ScanResult {
    let skill_name = skill_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("skill")
        .to_string();
    let trust_level = resolve_trust_level(source);
    let mut all_findings = Vec::new();

    if skill_path.is_dir() {
        all_findings.extend(check_structure(skill_path));
        for entry in WalkDir::new(skill_path).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let f = entry.path();
            let rel = f
                .strip_prefix(skill_path)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| f.display().to_string());
            if let Ok(content) = std::fs::read_to_string(f) {
                all_findings.extend(scan_content(&rel, &content));
            }
        }
    } else if skill_path.is_file()
        && let Ok(content) = std::fs::read_to_string(skill_path)
    {
        all_findings.extend(scan_content(
            skill_path.file_name().unwrap().to_str().unwrap(),
            &content,
        ));
    }

    let verdict = determine_verdict(&all_findings);
    let summary = build_summary(&skill_name, source, &trust_level, &verdict, &all_findings);
    ScanResult {
        skill_name,
        source: source.to_string(),
        trust_level,
        verdict,
        findings: all_findings,
        scanned_at: Utc::now().to_rfc3339(),
        summary,
    }
}

/// Scan a bundle of relative paths + bytes (hub install pre-check).
pub fn scan_bundle(skill_name: &str, source: &str, files: &[(String, Vec<u8>)]) -> ScanResult {
    let trust_level = resolve_trust_level(source);
    let mut all_findings = Vec::new();
    for (rel, bytes) in files {
        if let Ok(text) = std::str::from_utf8(bytes) {
            all_findings.extend(scan_content(rel, text));
        }
    }
    let verdict = determine_verdict(&all_findings);
    let summary = build_summary(skill_name, source, &trust_level, &verdict, &all_findings);
    ScanResult {
        skill_name: skill_name.to_string(),
        source: source.to_string(),
        trust_level,
        verdict,
        findings: all_findings,
        scanned_at: Utc::now().to_rfc3339(),
        summary,
    }
}

pub fn content_hash(skill_path: &Path) -> String {
    let mut h = Sha256::new();
    if skill_path.is_dir() {
        let mut paths: Vec<PathBuf> = WalkDir::new(skill_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .collect();
        paths.sort();
        for f in paths {
            if let Ok(bytes) = std::fs::read(&f) {
                h.update(bytes);
            }
        }
    } else if let Ok(bytes) = std::fs::read(skill_path) {
        h.update(bytes);
    }
    format!("sha256:{:x}", h.finalize()).chars().take(23).collect()
}

fn build_summary(name: &str, source: &str, trust: &str, verdict: &str, findings: &[Finding]) -> String {
    if findings.is_empty() {
        return format!("{name}: clean scan, no threats detected");
    }
    format!(
        "{name}: {source}/{trust} — {} finding(s), verdict={verdict}",
        findings.len()
    )
}

fn unicode_char_name(ch: char) -> &'static str {
    match ch {
        '\u{200b}' => "zero-width space",
        '\u{200c}' => "zero-width non-joiner",
        '\u{200d}' => "zero-width joiner",
        '\u{2060}' => "word joiner",
        '\u{2062}' => "invisible times",
        '\u{2063}' => "invisible separator",
        '\u{2064}' => "invisible plus",
        '\u{feff}' => "BOM/zero-width no-break space",
        '\u{202a}' => "LTR embedding",
        '\u{202b}' => "RTL embedding",
        '\u{202c}' => "pop directional",
        '\u{202d}' => "LTR override",
        '\u{202e}' => "RTL override",
        '\u{2066}' => "LTR isolate",
        '\u{2067}' => "RTL isolate",
        '\u{2068}' => "first strong isolate",
        '\u{2069}' => "pop directional isolate",
        _ => "unknown",
    }
}

fn check_structure(skill_dir: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut file_count = 0usize;
    let mut total_size = 0u64;
    let skill_canon = skill_dir.canonicalize().unwrap_or_else(|_| skill_dir.to_path_buf());

    for entry in WalkDir::new(skill_dir).into_iter().filter_map(|e| e.ok()) {
        let f = entry.path();
        if entry.file_type().is_symlink() {
            let rel = f
                .strip_prefix(skill_dir)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if let Ok(resolved) = f.canonicalize() {
                if !resolved.starts_with(&skill_canon) {
                    findings.push(Finding {
                        pattern_id: "symlink_escape".into(),
                        severity: "critical".into(),
                        category: "traversal".into(),
                        file: rel,
                        line: 0,
                        match_text: format!("symlink -> {}", resolved.display()),
                        description: "symlink points outside the skill directory".into(),
                    });
                }
            } else {
                findings.push(Finding {
                    pattern_id: "broken_symlink".into(),
                    severity: "medium".into(),
                    category: "traversal".into(),
                    file: rel,
                    line: 0,
                    match_text: "broken symlink".into(),
                    description: "broken or circular symlink".into(),
                });
            }
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        file_count += 1;
        let rel = f
            .strip_prefix(skill_dir)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let Ok(meta) = f.metadata() else {
            continue;
        };
        let size = meta.len();
        total_size += size;
        if size > (MAX_SINGLE_FILE_KB as u64) * 1024 {
            findings.push(Finding {
                pattern_id: "oversized_file".into(),
                severity: "medium".into(),
                category: "structural".into(),
                file: rel.clone(),
                line: 0,
                match_text: format!("{}KB", size / 1024),
                description: format!(
                    "file is {}KB (limit: {MAX_SINGLE_FILE_KB}KB)",
                    size / 1024
                ),
            });
        }
        if let Some(ext) = f.extension().and_then(|e| e.to_str()) {
            let lower = format!(".{}", ext.to_ascii_lowercase());
            if SUSPICIOUS_BINARY_EXTENSIONS.contains(&lower.as_str()) {
                findings.push(Finding {
                    pattern_id: "binary_file".into(),
                    severity: "critical".into(),
                    category: "structural".into(),
                    file: rel.clone(),
                    line: 0,
                    match_text: format!("binary: {lower}"),
                    description: format!("binary/executable file ({lower}) should not be in a skill"),
                });
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode();
            let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
            let script_exts = ["sh", "bash", "py", "rb", "pl"];
            if !script_exts.contains(&ext) && mode & 0o111 != 0 {
                findings.push(Finding {
                    pattern_id: "unexpected_executable".into(),
                    severity: "medium".into(),
                    category: "structural".into(),
                    file: rel,
                    line: 0,
                    match_text: "executable bit set".into(),
                    description:
                        "file has executable permission but is not a recognized script type"
                            .into(),
                });
            }
        }
    }
    if file_count > MAX_FILE_COUNT {
        findings.push(Finding {
            pattern_id: "too_many_files".into(),
            severity: "medium".into(),
            category: "structural".into(),
            file: "(directory)".into(),
            line: 0,
            match_text: format!("{file_count} files"),
            description: format!("skill has {file_count} files (limit: {MAX_FILE_COUNT})"),
        });
    }
    if total_size > (MAX_TOTAL_SIZE_KB as u64) * 1024 {
        findings.push(Finding {
            pattern_id: "oversized_skill".into(),
            severity: "high".into(),
            category: "structural".into(),
            file: "(directory)".into(),
            line: 0,
            match_text: format!("{}KB total", total_size / 1024),
            description: format!(
                "skill is {}KB total (limit: {MAX_TOTAL_SIZE_KB}KB)",
                total_size / 1024
            ),
        });
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_trust_levels() {
        assert_eq!(resolve_trust_level("official"), "builtin");
        assert_eq!(resolve_trust_level("openai/skills/x"), "trusted");
        assert_eq!(
            resolve_trust_level("skills-sh/anthropics/skills/foo"),
            "trusted"
        );
        assert_eq!(resolve_trust_level("random/x"), "community");
    }

    #[test]
    fn determine_verdict_levels() {
        assert_eq!(determine_verdict(&[]), "safe");
        let critical = Finding {
            pattern_id: "x".into(),
            severity: "critical".into(),
            category: "c".into(),
            file: "f".into(),
            line: 1,
            match_text: "m".into(),
            description: "d".into(),
        };
        assert_eq!(determine_verdict(&[critical.clone()]), "dangerous");
        let high = Finding {
            severity: "high".into(),
            ..critical
        };
        assert_eq!(determine_verdict(&[high]), "caution");
    }

    #[test]
    fn scan_content_detects_rm_rf() {
        let f = scan_content("bad.sh", "rm -rf /\n");
        assert!(f.iter().any(|x| x.pattern_id == "destructive_root_rm"));
    }

    #[test]
    fn community_caution_blocked_without_force() {
        let result = ScanResult {
            skill_name: "t".into(),
            source: "x".into(),
            trust_level: "community".into(),
            verdict: "caution".into(),
            findings: vec![Finding {
                pattern_id: "x".into(),
                severity: "high".into(),
                category: "c".into(),
                file: "f".into(),
                line: 1,
                match_text: "m".into(),
                description: "d".into(),
            }],
            scanned_at: String::new(),
            summary: String::new(),
        };
        let (d, _) = should_allow_install(&result, false);
        assert_eq!(d, InstallDecision::Blocked);
    }
}
