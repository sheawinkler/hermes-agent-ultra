use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=HERMES_BUILD_GIT_SHA");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let repo_root = manifest_dir.join("../..");
    emit_git_rerun_triggers(&repo_root);

    if let Ok(build_sha) = std::env::var("HERMES_BUILD_GIT_SHA") {
        let trimmed = build_sha.trim();
        if !trimmed.is_empty() {
            println!(
                "cargo:rustc-env=HERMES_BUILD_GIT_SHA={}",
                short_sha(trimmed)
            );
        }
        return;
    }

    if let Ok(github_sha) = std::env::var("GITHUB_SHA") {
        let trimmed = github_sha.trim();
        if !trimmed.is_empty() {
            println!(
                "cargo:rustc-env=HERMES_BUILD_GIT_SHA={}",
                short_sha(trimmed)
            );
            return;
        }
    }

    let output = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(repo_root)
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !sha.is_empty() {
                println!("cargo:rustc-env=HERMES_BUILD_GIT_SHA={sha}");
            }
        }
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
