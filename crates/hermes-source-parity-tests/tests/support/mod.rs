use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn canonical_rust_source_path(file: &str) -> &str {
    match file {
        "crates/hermes-cli/src/commands.rs" => "crates/hermes-cli/src/commands/mod.rs",
        _ => file,
    }
}

fn include_target(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("include!(\"")
        .and_then(|rest| rest.strip_suffix("\");"))
}

fn declares_sidecar_tests(line: &str) -> bool {
    line.trim() == "mod tests;"
}

fn sidecar_test_candidates(path: &Path) -> Vec<PathBuf> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    if path.file_name().and_then(|name| name.to_str()) == Some("mod.rs") {
        return vec![parent.join("tests.rs"), parent.join("tests/mod.rs")];
    }

    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return Vec::new();
    };
    vec![
        parent.join(stem).join("tests.rs"),
        parent.join(stem).join("tests/mod.rs"),
    ]
}

fn append_sidecar_tests(path: &Path, source: &mut String, seen: &mut BTreeSet<PathBuf>) {
    let candidates = sidecar_test_candidates(path);
    if let Some(candidate) = candidates.iter().find(|candidate| candidate.exists()) {
        append_source_with_includes(candidate, source, seen);
        return;
    }
    let rendered = candidates
        .iter()
        .map(|candidate| candidate.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    panic!(
        "{} declares `mod tests;` but no sidecar test module exists at: {}",
        path.display(),
        rendered
    );
}

fn append_source_with_includes(path: &Path, source: &mut String, seen: &mut BTreeSet<PathBuf>) {
    let path = path.to_path_buf();
    if !seen.insert(path.clone()) {
        return;
    }

    let raw = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "failed reading referenced Rust file {}: {}",
            path.display(),
            err
        )
    });
    source.push_str(&raw);
    source.push('\n');

    let parent = path
        .parent()
        .unwrap_or_else(|| panic!("referenced Rust file has no parent: {}", path.display()));
    for line in raw.lines() {
        if let Some(target) = include_target(line) {
            append_source_with_includes(&parent.join(target), source, seen);
        } else if declares_sidecar_tests(line) {
            append_sidecar_tests(&path, source, seen);
        }
    }
}

pub fn read_rust_source_with_includes(file: &str) -> String {
    let full = repo_root().join(canonical_rust_source_path(file));
    let mut source = String::new();
    let mut seen = BTreeSet::new();
    append_source_with_includes(&full, &mut source, &mut seen);
    source
}

fn has_test_attribute_immediately_before(source: &str, index: usize) -> bool {
    let line_start = source[..index].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    for line in source[..line_start].lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with("#[") {
            return false;
        }
        if trimmed == "#[test]" || trimmed.starts_with("#[tokio::test") {
            return true;
        }
    }
    false
}

pub fn rust_test_exists(file: &str, name: &str) -> bool {
    let source = read_rust_source_with_includes(file);
    let needle = format!("fn {name}");
    let Some(index) = source.find(&needle) else {
        return false;
    };
    has_test_attribute_immediately_before(&source, index)
}
