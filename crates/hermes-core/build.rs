use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=HERMES_GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=HERMES_BUILD_GIT_SHA");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let repo_root = manifest_dir.join("../..");
    emit_git_rerun_triggers(&repo_root);

    if let Some(sha) = env_sha("HERMES_GIT_COMMIT")
        .or_else(|| env_sha("HERMES_BUILD_GIT_SHA"))
        .or_else(|| env_sha("GITHUB_SHA"))
        .or_else(|| git_sha(&repo_root))
    {
        println!("cargo:rustc-env=HERMES_BUILD_GIT_SHA={sha}");
        println!("cargo:rustc-env=HERMES_GIT_COMMIT={sha}");
    }
}

fn env_sha(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(|v| short_sha(&v))
}

fn git_sha(repo_root: &std::path::Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

fn short_sha(raw: &str) -> String {
    raw.chars().take(12).collect()
}

fn emit_git_rerun_triggers(repo_root: &std::path::Path) {
    let git_dir = repo_root.join(".git");
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );

    let head = std::fs::read_to_string(git_dir.join("HEAD")).unwrap_or_default();
    let Some(ref_path) = head.trim().strip_prefix("ref: ") else {
        return;
    };
    if !ref_path.contains("..") {
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join(ref_path).display()
        );
    }
}
