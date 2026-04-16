//! Real memory backend: read/write MEMORY.md and USER.md in ~/.hermes/

use async_trait::async_trait;
use serde_json::json;
use std::collections::HashSet;

use crate::tools::memory::MemoryBackend;
use hermes_core::ToolError;

const ENTRY_DELIMITER: &str = "\n§\n";

/// Real memory backend that stores entries in ~/.hermes/memories/MEMORY.md and USER.md.
pub struct FileMemoryBackend {
    hermes_dir: std::path::PathBuf,
}

impl FileMemoryBackend {
    pub fn new() -> Self {
        let home = dirs_home().unwrap_or_else(|| std::path::PathBuf::from("."));
        Self {
            hermes_dir: home.join(".hermes").join("memories"),
        }
    }

    pub fn with_dir(dir: std::path::PathBuf) -> Self {
        Self { hermes_dir: dir }
    }

    fn path_for(&self, target: &str) -> Result<std::path::PathBuf, ToolError> {
        match target {
            "memory" => Ok(self.hermes_dir.join("MEMORY.md")),
            "user" => Ok(self.hermes_dir.join("USER.md")),
            _ => Err(ToolError::InvalidParams(format!(
                "Invalid target '{}'. Use 'memory' or 'user'.",
                target
            ))),
        }
    }

    async fn ensure_dir(&self) -> Result<(), ToolError> {
        tokio::fs::create_dir_all(&self.hermes_dir)
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to create ~/.hermes/memories: {}", e))
            })
    }

    async fn read_file(&self, path: &std::path::Path) -> Result<String, ToolError> {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => Ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(ToolError::ExecutionFailed(format!(
                "Failed to read '{}': {}",
                path.display(),
                e
            ))),
        }
    }

    async fn write_file(&self, path: &std::path::Path, content: &str) -> Result<(), ToolError> {
        self.ensure_dir().await?;
        tokio::fs::write(path, content).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path.display(), e))
        })
    }

    fn parse_entries(content: &str) -> Vec<String> {
        if content.trim().is_empty() {
            return Vec::new();
        }
        content
            .split(ENTRY_DELIMITER)
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    fn format_entries(entries: &[String]) -> String {
        entries.join(ENTRY_DELIMITER)
    }

    fn preview(text: &str) -> String {
        if text.chars().count() <= 80 {
            text.to_string()
        } else {
            let head: String = text.chars().take(80).collect();
            format!("{head}...")
        }
    }

    fn success_response(target: &str, entries: &[String], message: &str) -> String {
        json!({
            "success": true,
            "target": target,
            "message": message,
            "entries": entries,
            "entry_count": entries.len()
        })
        .to_string()
    }
}

impl Default for FileMemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var("HOME").ok().map(std::path::PathBuf::from)
}

#[async_trait]
impl MemoryBackend for FileMemoryBackend {
    async fn add(&self, target: &str, content: &str) -> Result<String, ToolError> {
        let path = self.path_for(target)?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(ToolError::InvalidParams(
                "Content cannot be empty for action='add'.".to_string(),
            ));
        }

        let content = self.read_file(&path).await?;
        let mut entries = Self::parse_entries(&content);

        if entries.iter().any(|e| e == trimmed) {
            return Ok(Self::success_response(
                target,
                &entries,
                "Entry already exists (no duplicate added).",
            ));
        }

        entries.push(trimmed.to_string());
        self.write_file(&path, &Self::format_entries(&entries))
            .await?;
        Ok(Self::success_response(target, &entries, "Entry added."))
    }

    async fn replace(
        &self,
        target: &str,
        old_text: &str,
        new_content: &str,
    ) -> Result<String, ToolError> {
        let path = self.path_for(target)?;
        let old_text = old_text.trim();
        let new_content = new_content.trim();
        if old_text.is_empty() {
            return Err(ToolError::InvalidParams(
                "old_text cannot be empty for action='replace'.".to_string(),
            ));
        }
        if new_content.is_empty() {
            return Err(ToolError::InvalidParams(
                "content cannot be empty for action='replace'.".to_string(),
            ));
        }

        let content = self.read_file(&path).await?;
        let mut entries = Self::parse_entries(&content);

        let matches: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter_map(|(idx, e)| {
                if e.contains(old_text) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();

        if matches.is_empty() {
            return Err(ToolError::ExecutionFailed(format!(
                "No entry matched '{}'.",
                old_text
            )));
        }

        if matches.len() > 1 {
            let distinct: HashSet<String> = matches.iter().map(|i| entries[*i].clone()).collect();
            if distinct.len() > 1 {
                let previews: Vec<String> = matches
                    .iter()
                    .map(|i| Self::preview(&entries[*i]))
                    .collect();
                return Err(ToolError::ExecutionFailed(format!(
                    "Multiple entries matched '{}'. Be more specific. Matches: {}",
                    old_text,
                    previews.join(" | ")
                )));
            }
        }

        let idx = matches[0];
        entries[idx] = new_content.to_string();
        self.write_file(&path, &Self::format_entries(&entries))
            .await?;
        Ok(Self::success_response(target, &entries, "Entry replaced."))
    }

    async fn remove(&self, target: &str, old_text: &str) -> Result<String, ToolError> {
        let path = self.path_for(target)?;
        let old_text = old_text.trim();
        if old_text.is_empty() {
            return Err(ToolError::InvalidParams(
                "old_text cannot be empty for action='remove'.".to_string(),
            ));
        }

        let content = self.read_file(&path).await?;
        let mut entries = Self::parse_entries(&content);
        let matches: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter_map(|(idx, e)| {
                if e.contains(old_text) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();

        if matches.is_empty() {
            return Err(ToolError::ExecutionFailed(format!(
                "No entry matched '{}'.",
                old_text
            )));
        }

        if matches.len() > 1 {
            let distinct: HashSet<String> = matches.iter().map(|i| entries[*i].clone()).collect();
            if distinct.len() > 1 {
                let previews: Vec<String> = matches
                    .iter()
                    .map(|i| Self::preview(&entries[*i]))
                    .collect();
                return Err(ToolError::ExecutionFailed(format!(
                    "Multiple entries matched '{}'. Be more specific. Matches: {}",
                    old_text,
                    previews.join(" | ")
                )));
            }
        }

        let idx = matches[0];
        entries.remove(idx);
        self.write_file(&path, &Self::format_entries(&entries))
            .await?;

        Ok(Self::success_response(target, &entries, "Entry removed."))
    }
}
