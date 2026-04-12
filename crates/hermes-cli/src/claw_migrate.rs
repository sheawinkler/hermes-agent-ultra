//! `hermes claw migrate` — OpenClaw migration command.
//!
//! Ported from Python `hermes_cli/claw.py`.
//! Detects `~/.openclaw` directory and imports settings to `~/.hermes/`.

use std::path::{Path, PathBuf};

/// Known OpenClaw directory names (current + legacy).
const OPENCLAW_DIR_NAMES: &[&str] = &[".openclaw", ".clawdbot", ".moldbot"];

/// Files to migrate from OpenClaw.
const PERSONALITY_FILES: &[&str] = &["SOUL.md", "MEMORY.md", "USER.md"];

/// Result of a migration operation.
#[derive(Debug, Clone)]
pub struct MigrationResult {
    pub migrated: Vec<MigrationItem>,
    pub skipped: Vec<MigrationItem>,
    pub errors: Vec<MigrationItem>,
}

/// A single migration item.
#[derive(Debug, Clone)]
pub struct MigrationItem {
    pub kind: String,
    pub source: Option<PathBuf>,
    pub destination: Option<PathBuf>,
    pub status: MigrationStatus,
    pub reason: Option<String>,
}

/// Status of a migration item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationStatus {
    Migrated,
    Skipped,
    Conflict,
    Error,
}

/// Options for the migration command.
#[derive(Debug, Clone)]
pub struct MigrateOptions {
    /// Source directory (defaults to `~/.openclaw`).
    pub source: Option<PathBuf>,
    /// Whether to only preview changes without executing.
    pub dry_run: bool,
    /// Migration preset: "full", "minimal", "custom".
    pub preset: String,
    /// Whether to overwrite existing files.
    pub overwrite: bool,
}

impl Default for MigrateOptions {
    fn default() -> Self {
        Self {
            source: None,
            dry_run: false,
            preset: "full".to_string(),
            overwrite: false,
        }
    }
}

/// Find the OpenClaw source directory.
pub fn find_openclaw_dir(explicit_source: Option<&Path>) -> Option<PathBuf> {
    if let Some(source) = explicit_source {
        if source.is_dir() {
            return Some(source.to_path_buf());
        }
        return None;
    }

    let home = dirs::home_dir()?;
    for name in OPENCLAW_DIR_NAMES {
        let candidate = home.join(name);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// Get the hermes home directory (`~/.hermes`).
pub fn get_hermes_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".hermes"))
}

/// Run the OpenClaw → Hermes migration.
pub fn run_migration(options: &MigrateOptions) -> MigrationResult {
    let mut result = MigrationResult {
        migrated: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
    };

    let source_dir = match find_openclaw_dir(options.source.as_deref()) {
        Some(dir) => dir,
        None => {
            result.errors.push(MigrationItem {
                kind: "source".to_string(),
                source: options.source.clone(),
                destination: None,
                status: MigrationStatus::Error,
                reason: Some("OpenClaw directory not found".to_string()),
            });
            return result;
        }
    };

    let hermes_home = match get_hermes_home() {
        Some(h) => h,
        None => {
            result.errors.push(MigrationItem {
                kind: "target".to_string(),
                source: None,
                destination: None,
                status: MigrationStatus::Error,
                reason: Some("Could not determine home directory".to_string()),
            });
            return result;
        }
    };

    // Ensure hermes home exists
    if !options.dry_run {
        let _ = std::fs::create_dir_all(&hermes_home);
    }

    // Migrate personality files (SOUL.md, MEMORY.md, USER.md)
    for &filename in PERSONALITY_FILES {
        let src = source_dir.join(filename);
        let dst = hermes_home.join(filename);

        if !src.exists() {
            result.skipped.push(MigrationItem {
                kind: format!("personality:{}", filename),
                source: Some(src),
                destination: Some(dst),
                status: MigrationStatus::Skipped,
                reason: Some("Source file not found".to_string()),
            });
            continue;
        }

        if dst.exists() && !options.overwrite {
            result.skipped.push(MigrationItem {
                kind: format!("personality:{}", filename),
                source: Some(src),
                destination: Some(dst),
                status: MigrationStatus::Conflict,
                reason: Some("Already exists (use --overwrite)".to_string()),
            });
            continue;
        }

        if !options.dry_run {
            match std::fs::copy(&src, &dst) {
                Ok(_) => {
                    result.migrated.push(MigrationItem {
                        kind: format!("personality:{}", filename),
                        source: Some(src),
                        destination: Some(dst),
                        status: MigrationStatus::Migrated,
                        reason: None,
                    });
                }
                Err(e) => {
                    result.errors.push(MigrationItem {
                        kind: format!("personality:{}", filename),
                        source: Some(src),
                        destination: Some(dst),
                        status: MigrationStatus::Error,
                        reason: Some(e.to_string()),
                    });
                }
            }
        } else {
            result.migrated.push(MigrationItem {
                kind: format!("personality:{}", filename),
                source: Some(src),
                destination: Some(dst),
                status: MigrationStatus::Migrated,
                reason: Some("(dry run)".to_string()),
            });
        }
    }

    // Migrate skills directory
    let src_skills = source_dir.join("skills");
    let dst_skills = hermes_home.join("skills").join("openclaw-imports");

    if src_skills.is_dir() {
        if !options.dry_run {
            let _ = std::fs::create_dir_all(&dst_skills);
        }
        migrate_skills_dir(&src_skills, &dst_skills, options, &mut result);
    }

    // Migrate .env file (API keys)
    let src_env = source_dir.join(".env");
    let dst_env = hermes_home.join(".env");

    if src_env.exists() {
        if dst_env.exists() && !options.overwrite {
            result.skipped.push(MigrationItem {
                kind: "api-keys".to_string(),
                source: Some(src_env),
                destination: Some(dst_env),
                status: MigrationStatus::Conflict,
                reason: Some("Already exists (use --overwrite)".to_string()),
            });
        } else if !options.dry_run {
            match merge_env_files(&src_env, &dst_env) {
                Ok(count) => {
                    result.migrated.push(MigrationItem {
                        kind: "api-keys".to_string(),
                        source: Some(src_env),
                        destination: Some(dst_env),
                        status: MigrationStatus::Migrated,
                        reason: Some(format!("{} keys imported", count)),
                    });
                }
                Err(e) => {
                    result.errors.push(MigrationItem {
                        kind: "api-keys".to_string(),
                        source: Some(src_env),
                        destination: Some(dst_env),
                        status: MigrationStatus::Error,
                        reason: Some(e),
                    });
                }
            }
        } else {
            result.migrated.push(MigrationItem {
                kind: "api-keys".to_string(),
                source: Some(src_env),
                destination: Some(dst_env),
                status: MigrationStatus::Migrated,
                reason: Some("(dry run)".to_string()),
            });
        }
    }

    // Migrate platform configs (config.yaml / gateway.json)
    for config_name in &["config.yaml", "gateway.json"] {
        let src_cfg = source_dir.join(config_name);
        let dst_cfg = hermes_home.join(config_name);

        if src_cfg.exists() {
            if dst_cfg.exists() && !options.overwrite {
                result.skipped.push(MigrationItem {
                    kind: format!("config:{}", config_name),
                    source: Some(src_cfg),
                    destination: Some(dst_cfg),
                    status: MigrationStatus::Conflict,
                    reason: Some("Already exists".to_string()),
                });
            } else if !options.dry_run {
                match std::fs::copy(&src_cfg, &dst_cfg) {
                    Ok(_) => {
                        result.migrated.push(MigrationItem {
                            kind: format!("config:{}", config_name),
                            source: Some(src_cfg),
                            destination: Some(dst_cfg),
                            status: MigrationStatus::Migrated,
                            reason: None,
                        });
                    }
                    Err(e) => {
                        result.errors.push(MigrationItem {
                            kind: format!("config:{}", config_name),
                            source: Some(src_cfg),
                            destination: Some(dst_cfg),
                            status: MigrationStatus::Error,
                            reason: Some(e.to_string()),
                        });
                    }
                }
            }
        }
    }

    result
}

/// Recursively migrate skills from source to destination directory.
fn migrate_skills_dir(
    src: &Path,
    dst: &Path,
    options: &MigrateOptions,
    result: &mut MigrationResult,
) {
    let entries = match std::fs::read_dir(src) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let src_path = entry.path();
        let name = entry.file_name();
        let dst_path = dst.join(&name);

        if src_path.is_dir() {
            if !options.dry_run {
                let _ = std::fs::create_dir_all(&dst_path);
            }
            migrate_skills_dir(&src_path, &dst_path, options, result);
        } else if src_path.is_file() {
            if dst_path.exists() && !options.overwrite {
                result.skipped.push(MigrationItem {
                    kind: format!("skill:{}", name.to_string_lossy()),
                    source: Some(src_path),
                    destination: Some(dst_path),
                    status: MigrationStatus::Conflict,
                    reason: Some("Already exists".to_string()),
                });
            } else if !options.dry_run {
                match std::fs::copy(&src_path, &dst_path) {
                    Ok(_) => {
                        result.migrated.push(MigrationItem {
                            kind: format!("skill:{}", name.to_string_lossy()),
                            source: Some(src_path),
                            destination: Some(dst_path),
                            status: MigrationStatus::Migrated,
                            reason: None,
                        });
                    }
                    Err(e) => {
                        result.errors.push(MigrationItem {
                            kind: format!("skill:{}", name.to_string_lossy()),
                            source: Some(src_path),
                            destination: Some(dst_path),
                            status: MigrationStatus::Error,
                            reason: Some(e.to_string()),
                        });
                    }
                }
            }
        }
    }
}

/// Merge environment variables from source .env into destination .env.
/// Returns the number of keys imported.
fn merge_env_files(src: &Path, dst: &Path) -> Result<usize, String> {
    let src_content =
        std::fs::read_to_string(src).map_err(|e| format!("Failed to read source .env: {}", e))?;

    let existing = if dst.exists() {
        std::fs::read_to_string(dst).unwrap_or_default()
    } else {
        String::new()
    };

    let existing_keys: std::collections::HashSet<String> = existing
        .lines()
        .filter(|l| !l.starts_with('#') && l.contains('='))
        .filter_map(|l| l.split('=').next().map(|k| k.trim().to_string()))
        .collect();

    let mut new_lines = Vec::new();
    let mut count = 0;

    for line in src_content.lines() {
        if line.starts_with('#') || !line.contains('=') {
            continue;
        }
        if let Some(key) = line.split('=').next() {
            let key = key.trim();
            if !existing_keys.contains(key) {
                new_lines.push(line.to_string());
                count += 1;
            }
        }
    }

    if !new_lines.is_empty() {
        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("# Imported from OpenClaw\n"));
        content.push_str(&new_lines.join("\n"));
        content.push('\n');

        std::fs::write(dst, content).map_err(|e| format!("Failed to write .env: {}", e))?;
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_openclaw_dir_none() {
        let result = find_openclaw_dir(Some(Path::new("/nonexistent")));
        assert!(result.is_none());
    }

    #[test]
    fn test_find_openclaw_dir_explicit() {
        let tmp = tempfile::tempdir().unwrap();
        let result = find_openclaw_dir(Some(tmp.path()));
        assert!(result.is_some());
    }

    #[test]
    fn test_migration_dry_run() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("openclaw");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("SOUL.md"), "Test personality").unwrap();

        let options = MigrateOptions {
            source: Some(source.clone()),
            dry_run: true,
            preset: "full".to_string(),
            overwrite: false,
        };

        let result = run_migration(&options);
        // In dry run, personality files from source should be listed as migrated
        // (or skipped if hermes home can't be determined)
        let total = result.migrated.len() + result.skipped.len() + result.errors.len();
        assert!(total > 0, "Expected at least one migration item");
    }

    #[test]
    fn test_migration_no_source() {
        let options = MigrateOptions {
            source: Some(PathBuf::from("/nonexistent/openclaw")),
            dry_run: true,
            ..Default::default()
        };

        let result = run_migration(&options);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_merge_env_files() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.env");
        let dst = tmp.path().join("dst.env");

        std::fs::write(&src, "KEY1=value1\nKEY2=value2\n").unwrap();
        std::fs::write(&dst, "KEY1=existing\n").unwrap();

        let count = merge_env_files(&src, &dst).unwrap();
        assert_eq!(count, 1); // Only KEY2 should be imported

        let content = std::fs::read_to_string(&dst).unwrap();
        assert!(content.contains("KEY2=value2"));
        assert!(content.contains("KEY1=existing"));
    }

    #[test]
    fn test_migration_status_eq() {
        assert_eq!(MigrationStatus::Migrated, MigrationStatus::Migrated);
        assert_ne!(MigrationStatus::Migrated, MigrationStatus::Skipped);
    }
}
