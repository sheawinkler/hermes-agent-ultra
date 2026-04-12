//! Real memory backend: read/write MEMORY.md and USER.md in ~/.hermes/

use async_trait::async_trait;
use serde_json::json;

use crate::tools::memory::MemoryBackend;
use hermes_core::ToolError;

/// Real memory backend that stores entries in ~/.hermes/MEMORY.md and USER.md.
pub struct FileMemoryBackend {
    hermes_dir: std::path::PathBuf,
}

impl FileMemoryBackend {
    pub fn new() -> Self {
        let home = dirs_home().unwrap_or_else(|| std::path::PathBuf::from("."));
        Self {
            hermes_dir: home.join(".hermes"),
        }
    }

    pub fn with_dir(dir: std::path::PathBuf) -> Self {
        Self { hermes_dir: dir }
    }

    fn memory_path(&self) -> std::path::PathBuf {
        self.hermes_dir.join("MEMORY.md")
    }

    fn user_path(&self) -> std::path::PathBuf {
        self.hermes_dir.join("USER.md")
    }

    async fn ensure_dir(&self) -> Result<(), ToolError> {
        tokio::fs::create_dir_all(&self.hermes_dir)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to create ~/.hermes: {}", e)))
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

    fn parse_entries(content: &str) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        let mut current_key = String::new();
        let mut current_value = String::new();

        for line in content.lines() {
            if line.starts_with("## ") {
                if !current_key.is_empty() {
                    entries.push((current_key.clone(), current_value.trim().to_string()));
                }
                current_key = line[3..].trim().to_string();
                current_value.clear();
            } else if !current_key.is_empty() {
                if !current_value.is_empty() {
                    current_value.push('\n');
                }
                current_value.push_str(line);
            }
        }
        if !current_key.is_empty() {
            entries.push((current_key, current_value.trim().to_string()));
        }
        entries
    }

    fn format_entries(entries: &[(String, String)]) -> String {
        let mut out = String::from("# Memory\n\n");
        for (key, value) in entries {
            out.push_str(&format!("## {}\n{}\n\n", key, value));
        }
        out
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
    async fn add(&self, key: &str, value: &str) -> Result<String, ToolError> {
        let path = self.memory_path();
        let content = self.read_file(&path).await?;
        let mut entries = Self::parse_entries(&content);
        entries.push((key.to_string(), value.to_string()));
        self.write_file(&path, &Self::format_entries(&entries))
            .await?;
        Ok(json!({"status": "ok", "action": "added", "key": key}).to_string())
    }

    async fn replace(&self, key: &str, value: &str) -> Result<String, ToolError> {
        let path = self.memory_path();
        let content = self.read_file(&path).await?;
        let mut entries = Self::parse_entries(&content);
        let mut found = false;
        for entry in entries.iter_mut() {
            if entry.0 == key {
                entry.1 = value.to_string();
                found = true;
                break;
            }
        }
        if !found {
            entries.push((key.to_string(), value.to_string()));
        }
        self.write_file(&path, &Self::format_entries(&entries))
            .await?;
        Ok(json!({"status": "ok", "action": "replaced", "key": key}).to_string())
    }

    async fn remove(&self, key: &str) -> Result<String, ToolError> {
        let path = self.memory_path();
        let content = self.read_file(&path).await?;
        let entries: Vec<(String, String)> = Self::parse_entries(&content)
            .into_iter()
            .filter(|(k, _)| k != key)
            .collect();
        self.write_file(&path, &Self::format_entries(&entries))
            .await?;
        Ok(json!({"status": "ok", "action": "removed", "key": key}).to_string())
    }

    async fn list(&self) -> Result<String, ToolError> {
        let memory_content = self.read_file(&self.memory_path()).await?;
        let user_content = self.read_file(&self.user_path()).await?;

        let memory_entries = Self::parse_entries(&memory_content);
        let user_entries = Self::parse_entries(&user_content);

        let result = json!({
            "memory": memory_entries.iter().map(|(k, v)| json!({"key": k, "value": v})).collect::<Vec<_>>(),
            "user": user_entries.iter().map(|(k, v)| json!({"key": k, "value": v})).collect::<Vec<_>>(),
        });

        Ok(result.to_string())
    }
}
