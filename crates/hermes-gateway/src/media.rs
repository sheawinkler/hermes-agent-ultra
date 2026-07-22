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
pub use hermes_config::{validate_media_delivery_path, ValidatedMediaDelivery};

/// Stage a protected delivery snapshot without blocking a gateway async worker.
pub(crate) async fn stage_media_delivery_path(path: &str) -> Option<ValidatedMediaDelivery> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || validate_media_delivery_path(&path))
        .await
        .ok()
        .flatten()
}

pub const SUPPORTED_DOCUMENT_TYPES: &[(&str, &str)] = &[
    (".pdf", "application/pdf"),
    (".md", "text/markdown"),
    (".txt", "text/plain"),
    (".csv", "text/csv"),
    (".json", "application/json"),
    (
        ".docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        ".xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        ".pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    (".zip", "application/zip"),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedMedia {
    pub kind: String,
    pub media_type: String,
    pub path: PathBuf,
    pub display_name: String,
}

impl CachedMedia {
    pub fn context_note(&self) -> String {
        format!(
            "{} cached at {} ({})",
            self.display_name,
            self.path.display(),
            self.media_type
        )
    }
}

// ---------------------------------------------------------------------------
// MediaCacheConfig
// ---------------------------------------------------------------------------

/// Default cap for a single inbound/downloaded media payload.
///
/// This bounds the in-memory and on-disk blast radius of hostile media links or
/// unusually large platform uploads while keeping ordinary images, voice notes,
/// and short clips well inside the default.
pub const DEFAULT_INBOUND_MEDIA_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// Configuration for the media cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaCacheConfig {
    /// Directory where cached files are stored.
    #[serde(default = "hermes_config::default_media_cache_dir")]
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
            cache_dir: hermes_config::default_media_cache_dir(),
            max_size: 0,
            ttl_seconds: 0,
            max_file_size: DEFAULT_INBOUND_MEDIA_MAX_BYTES,
        }
    }
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

    /// Cache document bytes under a safe, unique leaf filename.
    pub async fn cache_document_from_bytes(
        &self,
        data: &[u8],
        file_name: Option<&str>,
    ) -> Result<CachedMedia, GatewayError> {
        let display_name = safe_leaf_name(file_name.unwrap_or(""), "document");
        let media_type =
            mime_for_document_name(&display_name).unwrap_or("application/octet-stream");
        let path = self
            .write_bytes_to_cache("documents", data, &display_name)
            .await?;
        Ok(CachedMedia {
            kind: "document".to_string(),
            media_type: media_type.to_string(),
            path,
            display_name,
        })
    }

    /// Cache image/video/document bytes using MIME and extension hints.
    pub async fn cache_media_bytes(
        &self,
        data: &[u8],
        file_name: Option<&str>,
        mime_type: Option<&str>,
        default_kind: Option<&str>,
    ) -> Result<Option<CachedMedia>, GatewayError> {
        let display_name = safe_leaf_name(file_name.unwrap_or(""), "media");
        let ext = Path::new(&display_name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_ascii_lowercase()));
        let mime = mime_type
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase)
            .or_else(|| {
                ext.as_deref()
                    .and_then(mime_from_extension)
                    .map(str::to_string)
            })
            .or_else(|| {
                default_kind
                    .filter(|kind| *kind == "image")
                    .map(|_| "image/png".to_string())
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());

        let kind = if mime.starts_with("image/") {
            if !looks_like_image(data, &mime) {
                return Ok(None);
            }
            "image"
        } else if mime.starts_with("audio/") {
            "audio"
        } else if mime.starts_with("video/") {
            "video"
        } else if document_mime_supported(&mime)
            || ext
                .as_deref()
                .and_then(mime_for_extension)
                .map(document_mime_supported)
                .unwrap_or(false)
        {
            "document"
        } else {
            return Ok(None);
        };

        let path = self.write_bytes_to_cache(kind, data, &display_name).await?;
        Ok(Some(CachedMedia {
            kind: kind.to_string(),
            media_type: mime,
            path,
            display_name,
        }))
    }

    async fn write_bytes_to_cache(
        &self,
        subdir: &str,
        data: &[u8],
        display_name: &str,
    ) -> Result<PathBuf, GatewayError> {
        self.validate_max_file_size(data.len() as u64, "inbound media payload")?;
        if self.max_size > 0 {
            let current_size = self.calculate_cache_size().await.unwrap_or(0);
            if current_size.saturating_add(data.len() as u64) > self.max_size {
                return Err(GatewayError::ConnectionFailed(format!(
                    "Cache size limit exceeded while caching {} (current={}, incoming={}, max={})",
                    display_name,
                    current_size,
                    data.len(),
                    self.max_size
                )));
            }
        }

        let dest_dir = self.cache_dir.join(subdir);
        fs::create_dir_all(&dest_dir).await.map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Failed to create cache subdirectory {:?}: {}",
                dest_dir, e
            ))
        })?;
        let safe_name = safe_leaf_name(display_name, "media");
        let dest_path = dest_dir.join(format!("{}-{}", uuid::Uuid::new_v4(), safe_name));
        if !is_path_within(&dest_path, &dest_dir) {
            return Err(GatewayError::ConnectionFailed(format!(
                "Refusing to cache file outside cache directory: {}",
                safe_name
            )));
        }
        let mut file = fs::File::create(&dest_path).await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to create file {:?}: {}", dest_path, e))
        })?;
        file.write_all(data).await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to write file {:?}: {}", dest_path, e))
        })?;
        file.flush().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to flush file {:?}: {}", dest_path, e))
        })?;
        Ok(dest_path)
    }

    fn validate_max_file_size(&self, size: u64, context: &str) -> Result<(), GatewayError> {
        if self.max_file_size > 0 && size > self.max_file_size {
            return Err(GatewayError::ConnectionFailed(format!(
                "File too large for {}: {} bytes exceeds max_file_size {}",
                context, size, self.max_file_size
            )));
        }
        Ok(())
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
                self.validate_max_file_size(content_length, "Content-Length")?;
            }
        }

        let current_size = if self.max_size > 0 {
            self.calculate_cache_size().await.unwrap_or(0)
        } else {
            0
        };
        let tmp_path = dest_dir.join(format!(
            ".{}.{}.part",
            safe_name,
            uuid::Uuid::new_v4().simple()
        ));
        if !is_path_within(&tmp_path, &dest_dir) {
            return Err(GatewayError::ConnectionFailed(format!(
                "Refusing to cache temporary file outside cache directory: {}",
                safe_name
            )));
        }

        let download_result: Result<u64, GatewayError> = async {
            let mut file = fs::File::create(&tmp_path).await.map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Failed to create file {:?}: {}",
                    tmp_path, e
                ))
            })?;
            let mut response = response;
            let mut total = 0u64;
            while let Some(chunk) = response.chunk().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!("Failed to read response body: {}", e))
            })? {
                total = total.saturating_add(chunk.len() as u64);
                self.validate_max_file_size(total, "downloaded body")?;
                if self.max_size > 0 && current_size.saturating_add(total) > self.max_size {
                    return Err(GatewayError::ConnectionFailed(format!(
                        "Cache size limit exceeded while caching {} (current={}, incoming={}, max={})",
                        safe_name, current_size, total, self.max_size
                    )));
                }
                file.write_all(&chunk).await.map_err(|e| {
                    GatewayError::ConnectionFailed(format!(
                        "Failed to write file {:?}: {}",
                        tmp_path, e
                    ))
                })?;
            }
            file.flush().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!("Failed to flush file {:?}: {}", tmp_path, e))
            })?;
            Ok(total)
        }
        .await;

        if let Err(err) = download_result {
            let _ = fs::remove_file(&tmp_path).await;
            return Err(err);
        }

        if fs::try_exists(&dest_path).await.unwrap_or(false) {
            fs::remove_file(&dest_path).await.map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Failed to replace existing cached file {:?}: {}",
                    dest_path, e
                ))
            })?;
        }
        fs::rename(&tmp_path, &dest_path).await.map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Failed to move cached file {:?} to {:?}: {}",
                tmp_path, dest_path, e
            ))
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
                            if age > ttl_seconds && fs::remove_file(&path).await.is_ok() {
                                *removed += 1;
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

fn safe_leaf_name(raw: &str, fallback_stem: &str) -> String {
    let cleaned = raw.replace('\0', "");
    let leaf = Path::new(&cleaned)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .unwrap_or(fallback_stem);
    leaf.chars()
        .map(|ch| match ch {
            '/' | '\\' | '\0' => '_',
            _ => ch,
        })
        .collect()
}

fn mime_for_document_name(file_name: &str) -> Option<&'static str> {
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_ascii_lowercase()))?;
    mime_for_extension(&ext).filter(|mime| document_mime_supported(mime))
}

fn mime_from_extension(ext: &str) -> Option<&'static str> {
    let normalized = if ext.starts_with('.') {
        ext.to_ascii_lowercase()
    } else {
        format!(".{}", ext.to_ascii_lowercase())
    };
    match normalized.as_str() {
        ".png" => Some("image/png"),
        ".jpg" | ".jpeg" => Some("image/jpeg"),
        ".gif" => Some("image/gif"),
        ".webp" => Some("image/webp"),
        ".mp3" | ".mpeg" | ".mpga" => Some("audio/mpeg"),
        ".m4a" => Some("audio/mp4"),
        ".wav" => Some("audio/wav"),
        ".ogg" | ".oga" => Some("audio/ogg"),
        ".aac" => Some("audio/aac"),
        ".flac" => Some("audio/flac"),
        ".mp4" | ".m4v" => Some("video/mp4"),
        ".mov" => Some("video/quicktime"),
        _ => mime_for_extension(&normalized),
    }
}

fn mime_for_extension(ext: &str) -> Option<&'static str> {
    SUPPORTED_DOCUMENT_TYPES
        .iter()
        .find_map(|(known, mime)| (*known == ext).then_some(*mime))
}

fn document_mime_supported(mime: &str) -> bool {
    SUPPORTED_DOCUMENT_TYPES
        .iter()
        .any(|(_, supported)| *supported == mime)
}

fn looks_like_image(data: &[u8], mime: &str) -> bool {
    match mime {
        "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => data.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a"),
        "image/webp" => data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP"),
        _ => !data.is_empty(),
    }
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
        assert_eq!(config.max_file_size, DEFAULT_INBOUND_MEDIA_MAX_BYTES);
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

    #[tokio::test]
    async fn cache_document_from_bytes_preserves_safe_leaf_and_uniqueness() {
        let dir = tempfile::tempdir().unwrap();
        let cache = MediaCache::new(&MediaCacheConfig {
            cache_dir: dir.path().to_string_lossy().to_string(),
            max_size: 0,
            ttl_seconds: 0,
            max_file_size: 0,
        })
        .unwrap();

        let first = cache
            .cache_document_from_bytes(b"a", Some("../../report.pdf"))
            .await
            .unwrap();
        let second = cache
            .cache_document_from_bytes(b"b", Some("../../report.pdf"))
            .await
            .unwrap();

        assert_eq!(first.kind, "document");
        assert_eq!(first.media_type, "application/pdf");
        assert_eq!(first.display_name, "report.pdf");
        assert_ne!(first.path, second.path);
        assert!(first.path.starts_with(dir.path().join("documents")));
        assert_eq!(tokio::fs::read(first.path).await.unwrap(), b"a");
    }

    #[tokio::test]
    async fn cache_media_bytes_routes_supported_kinds_and_rejects_invalid_image() {
        let dir = tempfile::tempdir().unwrap();
        let cache = MediaCache::new(&MediaCacheConfig {
            cache_dir: dir.path().to_string_lossy().to_string(),
            max_size: 0,
            ttl_seconds: 0,
            max_file_size: 0,
        })
        .unwrap();
        let png_1px = [
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 0,
        ];

        let image = cache
            .cache_media_bytes(&png_1px, Some("photo.png"), Some("image/png"), None)
            .await
            .unwrap()
            .expect("valid png");
        assert_eq!(image.kind, "image");
        assert_eq!(image.media_type, "image/png");

        let doc = cache
            .cache_media_bytes(b"%PDF-1.4", Some("report.pdf"), None, None)
            .await
            .unwrap()
            .expect("pdf");
        assert_eq!(doc.kind, "document");
        assert!(doc.context_note().contains("report.pdf"));

        let audio = cache
            .cache_media_bytes(
                b"mp4-aac-bytes",
                Some("audio_message.mp4"),
                Some("audio/mp4"),
                None,
            )
            .await
            .unwrap()
            .expect("audio/mp4");
        assert_eq!(audio.kind, "audio");
        assert_eq!(audio.media_type, "audio/mp4");
        assert!(audio.path.starts_with(dir.path().join("audio")));

        let m4a = cache
            .cache_media_bytes(b"m4a-bytes", Some("clip.m4a"), None, None)
            .await
            .unwrap()
            .expect("m4a extension");
        assert_eq!(m4a.kind, "audio");
        assert_eq!(m4a.media_type, "audio/mp4");

        let invalid = cache
            .cache_media_bytes(b"<html>", Some("bad.png"), Some("image/png"), None)
            .await
            .unwrap();
        assert!(invalid.is_none());
    }

    #[tokio::test]
    async fn cache_media_bytes_rejects_oversized_payload_before_write() {
        let dir = tempfile::tempdir().unwrap();
        let cache = MediaCache::new(&MediaCacheConfig {
            cache_dir: dir.path().to_string_lossy().to_string(),
            max_size: 0,
            ttl_seconds: 0,
            max_file_size: 4,
        })
        .unwrap();

        let err = cache
            .cache_media_bytes(b"%PDF-1.4", Some("report.pdf"), None, None)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("inbound media payload"));
        assert!(!dir.path().join("documents").exists());
    }

    #[tokio::test]
    async fn cache_file_rejects_oversized_content_length_before_body() {
        crate::ssrf::reset_allow_private_cache_for_tests();
        std::env::set_var("HERMES_ALLOW_PRIVATE_URLS", "true");
        crate::ssrf::reset_allow_private_cache_for_tests();

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let served = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\noversized")
                .await
                .unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let cache = MediaCache::new(&MediaCacheConfig {
            cache_dir: dir.path().to_string_lossy().to_string(),
            max_size: 0,
            ttl_seconds: 0,
            max_file_size: 4,
        })
        .unwrap();

        let err = cache
            .cache_document(&format!("http://{addr}/big.pdf"), "big.pdf")
            .await
            .unwrap_err();
        served.await.unwrap();

        assert!(err.to_string().contains("Content-Length"));
        assert!(!dir.path().join("documents").join("big.pdf").exists());

        std::env::remove_var("HERMES_ALLOW_PRIVATE_URLS");
        crate::ssrf::reset_allow_private_cache_for_tests();
    }

    #[test]
    fn supported_document_types_have_expected_extensions_and_mimes() {
        for (ext, mime) in SUPPORTED_DOCUMENT_TYPES {
            assert!(ext.starts_with('.'));
            assert!(mime.contains('/'));
        }
        for ext in [".pdf", ".md", ".txt", ".zip", ".docx", ".xlsx", ".pptx"] {
            assert!(SUPPORTED_DOCUMENT_TYPES
                .iter()
                .any(|(known, _)| *known == ext));
        }
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
