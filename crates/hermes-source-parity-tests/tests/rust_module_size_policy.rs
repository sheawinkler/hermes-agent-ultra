use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const IDEAL_LIMIT: usize = 800;
const HEALTHY_LIMIT: usize = 1_500;
const REVIEW_PRESSURE_LIMIT: usize = 2_500;
const EXCEPTIONAL_LIMIT: usize = 4_000;
const HARD_LIMIT: usize = 5_000;

const ALLOWLIST_PATH: &str = "docs/architecture/rust-module-size-allowlist.txt";

#[derive(Debug, Clone)]
struct RustFile {
    path: String,
    lines: usize,
}

#[derive(Debug, Clone)]
struct AllowlistEntry {
    max_lines: usize,
    reason: String,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn should_skip_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(
            ".git"
                | ".github"
                | ".pytest_cache"
                | ".sync-reports"
                | "node_modules"
                | "target"
                | "third_party"
        )
    )
}

fn repo_relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn count_lines(path: &Path) -> usize {
    let bytes =
        fs::read(path).unwrap_or_else(|err| panic!("failed reading {}: {err}", path.display()));
    if bytes.is_empty() {
        return 0;
    }
    let newline_count = bytes.iter().filter(|byte| **byte == b'\n').count();
    if bytes.ends_with(b"\n") {
        newline_count
    } else {
        newline_count + 1
    }
}

fn collect_rust_files(root: &Path, dir: &Path, files: &mut Vec<RustFile>) {
    if should_skip_dir(dir) {
        return;
    }

    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("failed reading {}: {err}", dir.display()));
    for entry in entries {
        let entry = entry
            .unwrap_or_else(|err| panic!("failed reading dir entry in {}: {err}", dir.display()));
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|err| panic!("failed reading file type for {}: {err}", path.display()));

        if file_type.is_dir() {
            collect_rust_files(root, &path, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(RustFile {
                path: repo_relative_path(root, &path),
                lines: count_lines(&path),
            });
        }
    }
}

fn parse_allowlist(root: &Path) -> BTreeMap<String, AllowlistEntry> {
    let path = root.join(ALLOWLIST_PATH);
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed reading {}: {err}", path.display()));
    let mut entries = BTreeMap::new();

    for (line_idx, raw_line) in raw.lines().enumerate() {
        let line_number = line_idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts = line.split('|').map(str::trim).collect::<Vec<_>>();
        assert_eq!(
            parts.len(),
            3,
            "{ALLOWLIST_PATH}:{line_number} must use `path|max_lines|reason`"
        );

        let repo_path = parts[0];
        assert!(
            repo_path.ends_with(".rs"),
            "{ALLOWLIST_PATH}:{line_number} allowlist path must be a Rust file"
        );
        assert!(
            !repo_path.starts_with('/')
                && !repo_path.contains("..")
                && !repo_path.starts_with("third_party/"),
            "{ALLOWLIST_PATH}:{line_number} allowlist path must be first-party and repo-relative"
        );

        let max_lines = parts[1].parse::<usize>().unwrap_or_else(|err| {
            panic!("{ALLOWLIST_PATH}:{line_number} invalid max_lines: {err}")
        });
        assert!(
            max_lines > HARD_LIMIT,
            "{ALLOWLIST_PATH}:{line_number} max_lines must be greater than the {HARD_LIMIT}-line hard limit"
        );

        let reason = parts[2].to_string();
        assert!(
            reason.len() >= 24,
            "{ALLOWLIST_PATH}:{line_number} reason must be specific enough to be useful"
        );

        assert!(
            entries
                .insert(repo_path.to_string(), AllowlistEntry { max_lines, reason })
                .is_none(),
            "{ALLOWLIST_PATH}:{line_number} duplicate allowlist entry for {repo_path}"
        );
    }

    entries
}

fn tier_name(lines: usize) -> &'static str {
    match lines {
        0..=IDEAL_LIMIT => "ideal",
        _ if lines <= HEALTHY_LIMIT => "healthy",
        _ if lines <= REVIEW_PRESSURE_LIMIT => "reported-pressure",
        _ if lines <= EXCEPTIONAL_LIMIT => "review-pressure",
        _ if lines <= HARD_LIMIT => "exceptional",
        _ => "hard-limit",
    }
}

fn print_tier_report(files: &[RustFile]) {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for file in files {
        *counts.entry(tier_name(file.lines)).or_default() += 1;
    }

    eprintln!("Rust module size policy report:");
    eprintln!("  scanned: {} first-party Rust files", files.len());
    for tier in [
        "ideal",
        "healthy",
        "reported-pressure",
        "review-pressure",
        "exceptional",
        "hard-limit",
    ] {
        eprintln!(
            "  {tier}: {}",
            counts.get(tier).copied().unwrap_or_default()
        );
    }

    eprintln!("  largest first-party Rust files:");
    for file in files.iter().take(25) {
        eprintln!("    {:>5} {}", file.lines, file.path);
    }
}

#[test]
fn first_party_rust_modules_respect_size_redline() {
    let root = repo_root();
    let mut files = Vec::new();
    collect_rust_files(&root, &root, &mut files);
    files.sort_by(|left, right| {
        right
            .lines
            .cmp(&left.lines)
            .then(left.path.cmp(&right.path))
    });
    assert!(
        !files.is_empty(),
        "module-size policy scan found no Rust files"
    );

    let allowlist = parse_allowlist(&root);
    let files_by_path = files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<BTreeMap<_, _>>();

    let mut stale_allowlist_entries = Vec::new();
    for (path, entry) in &allowlist {
        let Some(file) = files_by_path.get(path.as_str()) else {
            stale_allowlist_entries.push(format!("{path}: file not found"));
            continue;
        };
        if file.lines <= HARD_LIMIT {
            stale_allowlist_entries.push(format!(
                "{path}: {} lines no longer needs a hard-limit exception",
                file.lines
            ));
        }
        if file.lines > entry.max_lines {
            stale_allowlist_entries.push(format!(
                "{path}: {} lines exceeds allowlisted max {} ({})",
                file.lines, entry.max_lines, entry.reason
            ));
        }
    }

    let allowlisted_paths = allowlist.keys().cloned().collect::<BTreeSet<_>>();
    let hard_limit_violations = files
        .iter()
        .filter(|file| file.lines > HARD_LIMIT && !allowlisted_paths.contains(&file.path))
        .map(|file| format!("{} lines: {}", file.lines, file.path))
        .collect::<Vec<_>>();

    print_tier_report(&files);

    assert!(
        stale_allowlist_entries.is_empty(),
        "stale or invalid Rust module-size allowlist entries:\n{}",
        stale_allowlist_entries.join("\n")
    );
    assert!(
        hard_limit_violations.is_empty(),
        "first-party Rust files over {HARD_LIMIT} lines need refactoring or a justified allowlist entry in {ALLOWLIST_PATH}:\n{}",
        hard_limit_violations.join("\n")
    );
}
