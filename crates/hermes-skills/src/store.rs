//! Local skill storage: the `SkillStore` trait and `FileSkillStore` implementation.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, instrument};

use hermes_core::types::{Skill, SkillMeta};

use crate::guard::SkillGuard;
use crate::skill::SkillError;

// ---------------------------------------------------------------------------
// SkillStore trait
// ---------------------------------------------------------------------------

/// Abstraction over skill persistence backends.
#[async_trait]
pub trait SkillStore: Send + Sync {
    /// Persist a skill. Creates or overwrites.
    async fn save(&self, skill: &Skill) -> Result<(), SkillError>;

    /// Load a skill by name. Returns `None` if not found.
    async fn load(&self, name: &str) -> Result<Option<Skill>, SkillError>;

    /// List metadata for all stored skills.
    async fn list(&self) -> Result<Vec<SkillMeta>, SkillError>;

    /// Delete a skill by name. Succeeds even if the skill didn't exist.
    async fn delete(&self, name: &str) -> Result<(), SkillError>;
}

// ---------------------------------------------------------------------------
// YAML frontmatter
// ---------------------------------------------------------------------------

/// The frontmatter we write into / parse from `SKILL.md`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SkillFrontmatter {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

// ---------------------------------------------------------------------------
// FileSkillStore
// ---------------------------------------------------------------------------

/// File-system backed skill store.
///
/// Skills are stored as `<skills_dir>/<category>/<name>/SKILL.md`.
/// Each file contains a YAML frontmatter block followed by the skill
/// content in Markdown.
pub struct FileSkillStore {
    skills_dir: PathBuf,
}

impl FileSkillStore {
    /// Create a new store rooted at `skills_dir`.
    pub fn new(skills_dir: PathBuf) -> Self {
        Self { skills_dir }
    }

    /// Return the default skills directory: `~/.hermes/skills/`.
    pub fn default_dir() -> PathBuf {
        directories::ProjectDirs::from("com", "hermes", "hermes")
            .map(|dirs| dirs.data_dir().join("skills"))
            .unwrap_or_else(|| PathBuf::from(".hermes/skills"))
    }

    /// Validate a skill path segment (`name` / `category`) to prevent path traversal.
    fn validate_segment(value: &str, field: &str) -> Result<String, SkillError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: value must be non-empty"
            )));
        }
        if trimmed.len() > 128 {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: value exceeds 128 chars"
            )));
        }

        let path = Path::new(trimmed);
        let mut comps = path.components();
        let segment = match (comps.next(), comps.next()) {
            (Some(Component::Normal(seg)), None) => seg.to_string_lossy().to_string(),
            _ => {
                return Err(SkillError::GuardViolation(format!(
                    "Invalid skill {field}: path traversal or separators are not allowed"
                )))
            }
        };

        if segment.starts_with('.') {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: hidden path segments are not allowed"
            )));
        }
        if segment.chars().any(|c| c.is_control()) {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: control characters are not allowed"
            )));
        }
        Ok(segment)
    }

    fn is_hidden_name(name: &str) -> bool {
        name.starts_with('.')
    }

    /// Compute the directory path for a given skill name and optional category.
    fn skill_dir(&self, name: &str, category: Option<&str>) -> Result<PathBuf, SkillError> {
        let safe_name = Self::validate_segment(name, "name")?;
        let safe_category = category
            .map(|c| Self::validate_segment(c, "category"))
            .transpose()?;
        Ok(match safe_category {
            Some(cat) => self.skills_dir.join(cat).join(safe_name),
            None => self.skills_dir.join(safe_name),
        })
    }

    async fn canonical_root(&self) -> Result<PathBuf, SkillError> {
        fs::create_dir_all(&self.skills_dir)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))?;
        fs::canonicalize(&self.skills_dir)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))
    }

    async fn ensure_path_within_root(&self, path: &Path) -> Result<(), SkillError> {
        let root = self.canonical_root().await?;

        // For non-existing targets, resolve the parent and append the filename.
        let resolved = match fs::canonicalize(path).await {
            Ok(p) => p,
            Err(_) => {
                let parent = path.parent().ok_or_else(|| {
                    SkillError::GuardViolation("Invalid skill path: missing parent".to_string())
                })?;
                if !parent.exists() {
                    // Parent doesn't exist yet; check the nearest guaranteed safe root.
                    let rel = path.strip_prefix(&self.skills_dir).map_err(|_| {
                        SkillError::GuardViolation(
                            "Invalid skill path: outside skills root".to_string(),
                        )
                    })?;
                    root.join(rel)
                } else {
                    let parent_real = fs::canonicalize(parent)
                        .await
                        .map_err(|e| SkillError::Io(e.to_string()))?;
                    if let Some(name) = path.file_name() {
                        parent_real.join(name)
                    } else {
                        parent_real
                    }
                }
            }
        };

        if !resolved.starts_with(&root) {
            return Err(SkillError::GuardViolation(format!(
                "Skill path escapes root boundary: {}",
                path.display()
            )));
        }
        Ok(())
    }

    /// Write a `SKILL.md` file with frontmatter + content.
    fn render_skill_file(fm: &SkillFrontmatter, content: &str) -> String {
        let yaml = serde_yaml::to_string(fm).unwrap_or_default();
        // serde_yaml adds a leading "---\n" we need to strip the first line
        // and add our own delimiters.
        let yaml_body = yaml.trim_start_matches("---\n").trim_end();
        format!("---\n{}\n---\n{}", yaml_body, content)
    }

    /// Parse a `SKILL.md` file, extracting frontmatter and body.
    fn parse_skill_file(raw: &str) -> Result<(SkillFrontmatter, String), SkillError> {
        // We expect the file to start with "---\n" and end the frontmatter
        // with another "---\n".
        if !raw.starts_with("---") {
            return Err(SkillError::Parse(
                "Skill file must start with YAML frontmatter".to_string(),
            ));
        }

        let rest = &raw[3..]; // skip first "---"
                              // Find the closing "---"
        let end = rest
            .find("\n---")
            .ok_or_else(|| SkillError::Parse("Missing closing --- in frontmatter".to_string()))?;

        let yaml_str = &rest[..end];
        let body_start = end + 4; // skip "\n---"
        let body = rest[body_start..].trim_start_matches('\n').to_string();

        let fm: SkillFrontmatter =
            serde_yaml::from_str(yaml_str).map_err(|e| SkillError::Parse(e.to_string()))?;

        Ok((fm, body))
    }
}

#[async_trait]
impl SkillStore for FileSkillStore {
    #[instrument(skip(self, skill), fields(name = %skill.name))]
    async fn save(&self, skill: &Skill) -> Result<(), SkillError> {
        let dir = self.skill_dir(&skill.name, skill.category.as_deref())?;
        self.ensure_path_within_root(&dir).await?;
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))?;
        self.ensure_path_within_root(&dir).await?;

        let fm = SkillFrontmatter {
            name: skill.name.clone(),
            category: skill.category.clone(),
            description: skill.description.clone(),
            version: Some(crate::version::compute_version(&skill.content)),
        };

        let content = Self::render_skill_file(&fm, &skill.content);
        let path = dir.join("SKILL.md");

        debug!("Writing skill file: {}", path.display());
        fs::write(&path, content)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))?;

        Ok(())
    }

    #[instrument(skip(self), fields(name = %name))]
    async fn load(&self, name: &str) -> Result<Option<Skill>, SkillError> {
        // Search in all category subdirectories and the root.
        let candidates = self.candidate_dirs(name).await?;
        let root = self.canonical_root().await?;

        for dir in candidates {
            let path = dir.join("SKILL.md");
            if fs::try_exists(&path).await.unwrap_or(false) {
                let real = fs::canonicalize(&path)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                if !real.starts_with(&root) {
                    tracing::warn!("Skipping skill outside root boundary: {}", path.display());
                    continue;
                }
                let raw = fs::read_to_string(&path)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;

                let (fm, content) = Self::parse_skill_file(&raw)?;
                let skill = Skill {
                    name: fm.name,
                    content,
                    category: fm.category,
                    description: fm.description,
                };
                // Mandatory pre-use security gate.
                SkillGuard::default().scan_security_only(&skill)?;

                return Ok(Some(skill));
            }
        }

        Ok(None)
    }

    #[instrument(skip(self))]
    async fn list(&self) -> Result<Vec<SkillMeta>, SkillError> {
        let mut metas = Vec::new();

        // Ensure root exists.
        if !self.skills_dir.exists() {
            return Ok(metas);
        }

        self.collect_metas(&self.skills_dir, &mut metas).await?;
        Ok(metas)
    }

    #[instrument(skip(self), fields(name = %name))]
    async fn delete(&self, name: &str) -> Result<(), SkillError> {
        let candidates = self.candidate_dirs(name).await?;
        let root = self.canonical_root().await?;

        for dir in candidates {
            let path = dir.join("SKILL.md");
            if fs::try_exists(&path).await.unwrap_or(false) {
                let real = fs::canonicalize(&dir)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                if !real.starts_with(&root) {
                    tracing::warn!("Refusing to delete skill outside root: {}", dir.display());
                    continue;
                }
                // Remove the whole skill directory.
                fs::remove_dir_all(&dir)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                return Ok(());
            }
        }

        // Idempotent: deleting a non-existent skill is fine.
        Ok(())
    }
}

impl FileSkillStore {
    /// Build a list of candidate directories where a skill named `name`
    /// might live (root + any category subdirectory).
    async fn candidate_dirs(&self, name: &str) -> Result<Vec<PathBuf>, SkillError> {
        let safe_name = Self::validate_segment(name, "name")?;
        let root = self.canonical_root().await?;
        let mut dirs = vec![self.skills_dir.join(&safe_name)];

        if self.skills_dir.exists() {
            let mut entries = fs::read_dir(&self.skills_dir)
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?;

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?
            {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                if Self::is_hidden_name(&name) {
                    continue;
                }
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                if !file_type.is_dir() && !file_type.is_symlink() {
                    continue;
                }
                let canonical = fs::canonicalize(&path)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                if !fs::metadata(&canonical)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?
                    .is_dir()
                {
                    continue;
                }
                if !canonical.starts_with(&root) {
                    tracing::warn!("Skipping category outside root: {}", path.display());
                    continue;
                }
                let candidate = path.join(&safe_name);
                dirs.push(candidate);
            }
        }

        Ok(dirs)
    }

    /// Recursively collect [`SkillMeta`] from all `SKILL.md` files under
    /// `dir`. The `relative` prefix is used to reconstruct categories.
    async fn collect_metas(
        &self,
        dir: &Path,
        metas: &mut Vec<SkillMeta>,
    ) -> Result<(), SkillError> {
        if !dir.exists() {
            return Ok(());
        }

        let root = self.canonical_root().await?;
        let mut stack = vec![dir.to_path_buf()];
        let mut visited = HashSet::new();

        while let Some(current_dir) = stack.pop() {
            let canonical_current = fs::canonicalize(&current_dir)
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?;
            if !canonical_current.starts_with(&root) {
                tracing::warn!("Skipping directory outside root: {}", current_dir.display());
                continue;
            }
            if !visited.insert(canonical_current) {
                continue;
            }

            let mut entries = fs::read_dir(&current_dir)
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?;

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| SkillError::Io(e.to_string()))?
            {
                let file_name = entry.file_name();
                let file_name = file_name.to_string_lossy();
                if Self::is_hidden_name(&file_name) {
                    continue;
                }

                let file_type = entry
                    .file_type()
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                if !file_type.is_dir() && !file_type.is_symlink() {
                    continue;
                }

                let path = entry.path();
                let canonical = fs::canonicalize(&path)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?;
                if !fs::metadata(&canonical)
                    .await
                    .map_err(|e| SkillError::Io(e.to_string()))?
                    .is_dir()
                {
                    continue;
                }
                if !canonical.starts_with(&root) {
                    tracing::warn!("Skipping directory outside root: {}", path.display());
                    continue;
                }

                let skill_file = path.join("SKILL.md");
                if fs::try_exists(&skill_file).await.unwrap_or(false) {
                    let raw = fs::read_to_string(&skill_file)
                        .await
                        .map_err(|e| SkillError::Io(e.to_string()))?;

                    match Self::parse_skill_file(&raw) {
                        Ok((fm, _)) => {
                            // Derive category from the parent path relative to skills root.
                            let category = fm.category.or_else(|| {
                                canonical
                                    .parent()
                                    .and_then(|p| p.strip_prefix(&root).ok())
                                    .and_then(|rel| {
                                        let s = rel.to_string_lossy().to_string();
                                        if s.is_empty() {
                                            None
                                        } else {
                                            Some(s)
                                        }
                                    })
                            });

                            metas.push(SkillMeta {
                                name: fm.name,
                                category,
                                description: fm.description,
                            });
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse {}: {}", skill_file.display(), e);
                        }
                    }
                } else {
                    stack.push(path);
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_render_and_parse_frontmatter() {
        let fm = SkillFrontmatter {
            name: "my-skill".to_string(),
            category: Some("general".to_string()),
            description: Some("A test skill".to_string()),
            version: Some("0.1.0".to_string()),
        };
        let content = "# My Skill\nDo the thing.";
        let rendered = FileSkillStore::render_skill_file(&fm, content);

        let (parsed_fm, parsed_body) = FileSkillStore::parse_skill_file(&rendered).unwrap();
        assert_eq!(parsed_fm.name, "my-skill");
        assert_eq!(parsed_fm.category, Some("general".to_string()));
        assert_eq!(parsed_fm.description, Some("A test skill".to_string()));
        assert!(parsed_body.contains("Do the thing"));
    }

    #[tokio::test]
    async fn test_save_and_load_skill() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill = Skill {
            name: "greet".to_string(),
            content: "# Greet\nSay hello.".to_string(),
            category: Some("social".to_string()),
            description: Some("Greets people".to_string()),
        };

        store.save(&skill).await.unwrap();
        let loaded = store.load("greet").await.unwrap().unwrap();

        assert_eq!(loaded.name, "greet");
        assert_eq!(loaded.category, Some("social".to_string()));
        assert!(loaded.content.contains("Say hello."));
    }

    #[tokio::test]
    async fn test_list_skills() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill_a = Skill {
            name: "skill-a".to_string(),
            content: "Content A".to_string(),
            category: Some("cat1".to_string()),
            description: None,
        };
        let skill_b = Skill {
            name: "skill-b".to_string(),
            content: "Content B".to_string(),
            category: None,
            description: None,
        };

        store.save(&skill_a).await.unwrap();
        store.save(&skill_b).await.unwrap();

        let metas = store.list().await.unwrap();
        assert_eq!(metas.len(), 2);
    }

    #[tokio::test]
    async fn test_delete_skill() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill = Skill {
            name: "temp".to_string(),
            content: "Temporary".to_string(),
            category: None,
            description: None,
        };

        store.save(&skill).await.unwrap();
        store.delete("temp").await.unwrap();

        let result = store.load("temp").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_load_nonexistent() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());
        let result = store.load("nope").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_reject_path_traversal_name_on_save_and_load() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill = Skill {
            name: "../escape".to_string(),
            content: "Bad".to_string(),
            category: None,
            description: None,
        };
        assert!(store.save(&skill).await.is_err());
        assert!(store.load("../escape").await.is_err());
    }

    #[tokio::test]
    async fn test_reject_path_traversal_category_on_save() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let skill = Skill {
            name: "safe-skill".to_string(),
            content: "Body".to_string(),
            category: Some("../badcat".to_string()),
            description: None,
        };
        assert!(store.save(&skill).await.is_err());
    }

    #[tokio::test]
    async fn test_list_ignores_hidden_directories() {
        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let visible = Skill {
            name: "visible".to_string(),
            content: "# Visible".to_string(),
            category: None,
            description: None,
        };
        store.save(&visible).await.unwrap();

        let hidden_skill_dir = dir.path().join(".hidden").join("secret-skill");
        fs::create_dir_all(&hidden_skill_dir).await.unwrap();
        let fm = SkillFrontmatter {
            name: "secret-skill".to_string(),
            category: None,
            description: None,
            version: Some("0.1.0".to_string()),
        };
        let hidden_content = FileSkillStore::render_skill_file(&fm, "# Secret");
        fs::write(hidden_skill_dir.join("SKILL.md"), hidden_content)
            .await
            .unwrap();

        let metas = store.list().await.unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].name, "visible");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_list_follows_symlinked_skill_directories_once() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir().unwrap();
        let store = FileSkillStore::new(dir.path().to_path_buf());

        let real_skill_dir = dir.path().join("real-cat").join("linked-skill");
        fs::create_dir_all(&real_skill_dir).await.unwrap();
        let fm = SkillFrontmatter {
            name: "linked-skill".to_string(),
            category: None,
            description: Some("reachable through symlink".to_string()),
            version: Some("0.1.0".to_string()),
        };
        fs::write(
            real_skill_dir.join("SKILL.md"),
            FileSkillStore::render_skill_file(&fm, "# Linked Skill\nBody"),
        )
        .await
        .unwrap();

        let alias_dir = dir.path().join("alias-cat");
        unix_fs::symlink(dir.path().join("real-cat"), &alias_dir).unwrap();

        let metas = store.list().await.unwrap();
        let hits = metas.iter().filter(|m| m.name == "linked-skill").count();
        assert_eq!(hits, 1);
    }
}
