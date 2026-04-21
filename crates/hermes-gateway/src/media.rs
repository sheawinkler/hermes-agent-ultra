//! Media caching for images, audio, and documents.
//!
//! Downloads and caches media files locally to avoid repeated downloads
//! and to enable sending files through platform APIs that require local paths.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::debug;

use hermes_core::errors::GatewayError;

// ---------------------------------------------------------------------------
// MediaCacheConfig
// ---------------------------------------------------------------------------

/// Configuration for the media cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaCacheConfig {
    /// Directory where cached files are stored.
    #[serde(default = "default_cache_dir")]
    pub cache_dir: String,

    /// Maximum total cache size in bytes (0 = unlimited).
    #[serde(default)]
    pub max_size: u64,

    /// Time-to-live for cached files in seconds (0 = no expiry).
    #[serde(default)]
    pub ttl_seconds: u64,

    /// Maximum size of a single downloaded file in bytes (0 = unlimited).
    #[serde(default)]
    pub max_file_size: u64,
}

impl Default for MediaCacheConfig {
    fn default() -> Self {
        Self {
            cache_dir: default_cache_dir(),
            max_size: 0,
            ttl_seconds: 0,
            max_file_size: 0,
        }
    }
}

fn default_cache_dir() -> String {
    std::env::var("HERMES_MEDIA_CACHE_DIR")
        .unwrap_or_else(|_| "/tmp/hermes-media-cache".to_string())
}

// ---------------------------------------------------------------------------
// MediaCache
// ---------------------------------------------------------------------------

/// Manages local caching of media files downloaded from URLs.
pub struct MediaCache {
    /// Root directory for cached files.
    cache_dir: PathBuf,

    /// Maximum total cache size in bytes (0 = unlimited).
    max_size: u64,

    /// Time-to-live in seconds (0 = no expiry).
    ttl_seconds: u64,

    /// Maximum size of a single downloaded file in bytes (0 = unlimited).
    max_file_size: u64,

    /// HTTP client for downloading files.
    client: reqwest::Client,
}

impl MediaCache {
    /// Create a new `MediaCache` with the given configuration.
    pub fn new(config: &MediaCacheConfig) -> Result<Self, GatewayError> {
        let cache_dir = PathBuf::from(&config.cache_dir);

        // Create the cache directory if it doesn't exist
        std::fs::create_dir_all(&cache_dir).map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Failed to create cache directory {:?}: {}",
                cache_dir, e
            ))
        })?;

        let client = reqwest::Client::builder().build().map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to build HTTP client: {}", e))
        })?;

        Ok(Self {
            cache_dir,
            max_size: config.max_size,
            ttl_seconds: config.ttl_seconds,
            max_file_size: config.max_file_size,
            client,
        })
    }

    /// Create a `MediaCache` with default configuration.
    pub fn with_defaults() -> Result<Self, GatewayError> {
        Self::new(&MediaCacheConfig::default())
    }

    /// Cache an image from a URL.
    pub async fn cache_image(&self, url: &str, file_name: &str) -> Result<PathBuf, GatewayError> {
        self.cache_file(url, "images", file_name).await
    }

    /// Cache an audio file from a URL.
    pub async fn cache_audio(&self, url: &str, file_name: &str) -> Result<PathBuf, GatewayError> {
        self.cache_file(url, "audio", file_name).await
    }

    /// Cache a document from a URL.
    pub async fn cache_document(
        &self,
        url: &str,
        file_name: &str,
    ) -> Result<PathBuf, GatewayError> {
        self.cache_file(url, "documents", file_name).await
    }

    /// Generic file caching implementation.
    async fn cache_file(
        &self,
        url: &str,
        subdir: &str,
        file_name: &str,
    ) -> Result<PathBuf, GatewayError> {
        // Validate URL for SSRF protection
        crate::ssrf::validate_url(url)?;

        let dest_dir = self.cache_dir.join(subdir);
        fs::create_dir_all(&dest_dir).await.map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Failed to create cache subdirectory {:?}: {}",
                dest_dir, e
            ))
        })?;

        let safe_name = sanitize_file_name(file_name)?;
        let dest_path = dest_dir.join(&safe_name);

        if !is_path_within(&dest_path, &dest_dir) {
            return Err(GatewayError::ConnectionFailed(format!(
                "Refusing to cache file outside cache directory: {}",
                safe_name
            )));
        }

        // If the file already exists and is not expired, return it
        if dest_path.exists() {
            if self.ttl_seconds > 0 {
                if let Ok(metadata) = fs::metadata(&dest_path).await {
                    if let Ok(modified) = metadata.modified() {
                        let age = SystemTime::now()
                            .duration_since(modified)
                            .unwrap_or_default()
                            .as_secs();
                        if age < self.ttl_seconds {
                            debug!("Cache hit for {}", file_name);
                            return Ok(dest_path);
                        }
                    }
                }
            } else {
                // No TTL: file is always valid
                debug!("Cache hit for {}", file_name);
                return Ok(dest_path);
            }
        }

        // Download the file
        debug!("Downloading {} -> {:?}", url, dest_path);
        let response = self.client.get(url).send().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to download {}: {}", url, e))
        })?;

        if !response.status().is_success() {
            return Err(GatewayError::ConnectionFailed(format!(
                "HTTP {} when downloading {}",
                response.status(),
                url
            )));
        }

        if self.max_file_size > 0 {
            if let Some(content_length) = response.content_length() {
                if content_length > self.max_file_size {
                    return Err(GatewayError::ConnectionFailed(format!(
                        "File too large: {} bytes exceeds max_file_size {}",
                        content_length, self.max_file_size
                    )));
                }
            }
        }

        let bytes = response.bytes().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to read response body: {}", e))
        })?;

        if self.max_file_size > 0 && bytes.len() as u64 > self.max_file_size {
            return Err(GatewayError::ConnectionFailed(format!(
                "File too large after download: {} bytes exceeds max_file_size {}",
                bytes.len(),
                self.max_file_size
            )));
        }

        // Check cache size limits
        if self.max_size > 0 {
            let current_size = self.calculate_cache_size().await.unwrap_or(0);
            if current_size + bytes.len() as u64 > self.max_size {
                return Err(GatewayError::ConnectionFailed(format!(
                    "Cache size limit exceeded while caching {} (current={}, incoming={}, max={})",
                    safe_name,
                    current_size,
                    bytes.len(),
                    self.max_size
                )));
            }
        }

        // Write to file
        let mut file = fs::File::create(&dest_path).await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to create file {:?}: {}", dest_path, e))
        })?;

        file.write_all(&bytes).await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to write file {:?}: {}", dest_path, e))
        })?;

        file.flush().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to flush file {:?}: {}", dest_path, e))
        })?;

        debug!("Cached {} -> {:?}", url, dest_path);
        Ok(dest_path)
    }

    /// Remove expired cached files based on TTL.
    pub async fn cleanup_expired(&self, ttl_seconds: u64) -> Result<u64, GatewayError> {
        if ttl_seconds == 0 {
            return Ok(0);
        }

        let mut removed = 0u64;
        let now = SystemTime::now();

        self.cleanup_dir(&self.cache_dir, ttl_seconds, &now, &mut removed)
            .await?;

        Ok(removed)
    }

    /// Recursively clean up expired files in a directory.
    fn cleanup_dir<'a>(
        &'a self,
        dir: &'a Path,
        ttl_seconds: u64,
        now: &'a SystemTime,
        removed: &'a mut u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), GatewayError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut entries = fs::read_dir(dir).await.map_err(|e| {
                GatewayError::ConnectionFailed(format!("Failed to read directory {:?}: {}", dir, e))
            })?;

            while let Some(entry) = entries.next_entry().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!("Failed to read directory entry: {}", e))
            })? {
                let path = entry.path();
                if path.is_dir() {
                    self.cleanup_dir(&path, ttl_seconds, now, removed).await?;
                } else {
                    if let Ok(metadata) = entry.metadata().await {
                        if let Ok(modified) = metadata.modified() {
                            let age = now.duration_since(modified).unwrap_or_default().as_secs();
                            if age > ttl_seconds {
                                if fs::remove_file(&path).await.is_ok() {
                                    *removed += 1;
                                }
                            }
                        }
                    }
                }
            }

            Ok(())
        })
    }

    /// Calculate the total size of all cached files.
    async fn calculate_cache_size(&self) -> Result<u64, GatewayError> {
        let mut total_size: u64 = 0;
        self.calculate_dir_size(&self.cache_dir, &mut total_size)
            .await?;
        Ok(total_size)
    }

    /// Recursively calculate directory size.
    fn calculate_dir_size<'a>(
        &'a self,
        dir: &'a Path,
        total_size: &'a mut u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), GatewayError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut entries = fs::read_dir(dir).await.map_err(|e| {
                GatewayError::ConnectionFailed(format!("Failed to read directory {:?}: {}", dir, e))
            })?;

            while let Some(entry) = entries.next_entry().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!("Failed to read directory entry: {}", e))
            })? {
                let path = entry.path();
                if path.is_dir() {
                    self.calculate_dir_size(&path, total_size).await?;
                } else {
                    if let Ok(metadata) = entry.metadata().await {
                        *total_size += metadata.len();
                    }
                }
            }

            Ok(())
        })
    }

    /// Get the cache directory path.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}

fn sanitize_file_name(raw: &str) -> Result<String, GatewayError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(GatewayError::ConnectionFailed(
            "File name must not be empty".to_string(),
        ));
    }
    if trimmed.contains('\0')
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
    {
        return Err(GatewayError::ConnectionFailed(format!(
            "Unsafe file name rejected: {}",
            trimmed
        )));
    }
    Ok(trimmed.to_string())
}

fn is_path_within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_cache_config_default() {
        let config = MediaCacheConfig::default();
        assert!(!config.cache_dir.is_empty());
        assert_eq!(config.max_size, 0);
        assert_eq!(config.ttl_seconds, 0);
        assert_eq!(config.max_file_size, 0);
    }

    #[tokio::test]
    async fn media_cache_creates_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = MediaCacheConfig {
            cache_dir: dir.path().to_string_lossy().to_string(),
            max_size: 0,
            ttl_seconds: 0,
            max_file_size: 0,
        };
        let cache = MediaCache::new(&config).unwrap();
        assert!(cache.cache_dir().exists());
    }

    #[test]
    fn sanitize_file_name_rejects_path_traversal_patterns() {
        assert!(sanitize_file_name("../evil.txt").is_err());
        assert!(sanitize_file_name("..\\evil.txt").is_err());
        assert!(sanitize_file_name("/tmp/evil.txt").is_err());
        assert!(sanitize_file_name("subdir/file.txt").is_err());
        assert!(sanitize_file_name("safe.txt").is_ok());
    }

    #[test]
    fn is_path_within_blocks_escape_attempts() {
        let root = PathBuf::from("/tmp/hermes-cache-test");
        let ok = root.join("documents").join("file.txt");
        let bad = PathBuf::from("/tmp/hermes-cache-test-2/file.txt");
        assert!(is_path_within(&ok, &root));
        assert!(!is_path_within(&bad, &root));
    }
}
