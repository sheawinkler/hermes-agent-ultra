//! Media caching for images, audio, and documents.
//!
//! Downloads and caches media files locally to avoid repeated downloads
//! and to enable sending files through platform APIs that require local paths.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::debug;

use hermes_core::errors::GatewayError;

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

const MEDIA_DELIVERY_ALLOW_DIRS_ENV: &str = "HERMES_MEDIA_ALLOW_DIRS";
const MEDIA_DELIVERY_TRUST_RECENT_ENV: &str = "HERMES_MEDIA_TRUST_RECENT_FILES";
const MEDIA_DELIVERY_TRUST_RECENT_SECONDS_ENV: &str = "HERMES_MEDIA_TRUST_RECENT_SECONDS";
const MEDIA_DELIVERY_STRICT_ENV: &str = "HERMES_MEDIA_DELIVERY_STRICT";
const MEDIA_DELIVERY_TRUST_RECENT_DEFAULT_SECONDS: u64 = 600;

const MEDIA_DELIVERY_DENIED_PREFIXES: &[&str] = &[
    "/etc", "/proc", "/sys", "/dev", "/root", "/boot", "/var/log", "/var/lib", "/var/run",
];

const MEDIA_DELIVERY_DENIED_HOME_SUBPATHS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".docker",
    ".config",
    ".azure",
    ".gcloud",
    "Library/Keychains",
];

const MEDIA_DELIVERY_DENIED_HERMES_FILES: &[&str] =
    &[".env", "auth.json", "credentials", "config.yaml"];

#[derive(Debug, Clone)]
struct MediaDeliveryPolicy {
    home: Option<PathBuf>,
    hermes_home: PathBuf,
    hermes_root: Option<PathBuf>,
    allowed_roots: Vec<PathBuf>,
    denied_prefixes: Vec<PathBuf>,
    strict_mode: bool,
    recent_window: Duration,
}

impl MediaDeliveryPolicy {
    fn from_env() -> Self {
        let home = current_user_home();
        let hermes_home = hermes_config::hermes_home();
        let hermes_root = home.as_ref().map(|home| home.join(".hermes"));

        let mut allowed_roots = vec![PathBuf::from(default_cache_dir())];
        allowed_roots.extend(hermes_media_cache_roots(&hermes_home));
        allowed_roots.extend(extra_media_allow_roots_from_env());

        Self {
            home,
            hermes_home,
            hermes_root,
            allowed_roots,
            denied_prefixes: MEDIA_DELIVERY_DENIED_PREFIXES
                .iter()
                .map(PathBuf::from)
                .collect(),
            strict_mode: truthy_env(MEDIA_DELIVERY_STRICT_ENV, false),
            recent_window: media_delivery_recency_window_from_env(),
        }
    }
}

fn current_user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn hermes_media_cache_roots(hermes_home: &Path) -> Vec<PathBuf> {
    [
        "image_cache",
        "audio_cache",
        "video_cache",
        "document_cache",
        "browser_screenshots",
        "cache/images",
        "cache/audio",
        "cache/videos",
        "cache/documents",
        "cache/screenshots",
    ]
    .into_iter()
    .map(|subpath| hermes_home.join(subpath))
    .collect()
}

fn extra_media_allow_roots_from_env() -> Vec<PathBuf> {
    let Some(raw) = std::env::var_os(MEDIA_DELIVERY_ALLOW_DIRS_ENV) else {
        return Vec::new();
    };
    let raw = raw.to_string_lossy();
    let path_separator = if cfg!(windows) { ';' } else { ':' };
    raw.split(path_separator)
        .flat_map(|chunk| chunk.split(','))
        .map(str::trim)
        .filter(|chunk| !chunk.is_empty())
        .filter_map(|chunk| expand_home_path(chunk, current_user_home().as_deref()))
        .filter(|path| path.is_absolute())
        .collect()
}

fn truthy_env(name: &str, default: bool) -> bool {
    let Ok(raw) = std::env::var(name) else {
        return default;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" | "" => false,
        _ => default,
    }
}

fn media_delivery_recency_window_from_env() -> Duration {
    if !truthy_env(MEDIA_DELIVERY_TRUST_RECENT_ENV, true) {
        return Duration::ZERO;
    }
    let seconds = std::env::var(MEDIA_DELIVERY_TRUST_RECENT_SECONDS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(MEDIA_DELIVERY_TRUST_RECENT_DEFAULT_SECONDS as f64);
    Duration::from_secs_f64(seconds)
}

fn expand_home_path(raw: &str, home: Option<&Path>) -> Option<PathBuf> {
    if raw.contains('\0') {
        return None;
    }
    if raw == "~" {
        return home.map(Path::to_path_buf);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home.map(|home| home.join(rest));
    }
    Some(PathBuf::from(raw))
}

fn strip_model_path_wrapping(raw: &str) -> Option<String> {
    let mut candidate = raw.trim();
    if candidate.is_empty() || candidate.contains('\0') {
        return None;
    }

    let mut chars = candidate.chars();
    if let (Some(first), Some(last)) = (chars.next(), candidate.chars().last()) {
        if candidate.len() >= 2 && first == last && matches!(first, '`' | '"' | '\'') {
            let start = first.len_utf8();
            let end = candidate.len() - last.len_utf8();
            candidate = candidate[start..end].trim();
        }
    }

    let candidate = candidate
        .trim_start_matches(['`', '"', '\''])
        .trim_end_matches(['`', '"', '\'', ',', '.', ';', ':', ')', '}', ']'])
        .trim();

    if candidate.is_empty() || candidate.contains('\0') {
        None
    } else {
        Some(candidate.to_string())
    }
}

fn canonicalize_or_absolute(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Some(canonical);
    }
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    std::env::current_dir().ok().map(|cwd| cwd.join(path))
}

fn media_delivery_denied_paths(policy: &MediaDeliveryPolicy) -> Vec<PathBuf> {
    let mut denied = policy.denied_prefixes.clone();
    if let Some(home) = policy.home.as_ref() {
        denied.extend(
            MEDIA_DELIVERY_DENIED_HOME_SUBPATHS
                .iter()
                .map(|subpath| home.join(subpath)),
        );
    }
    for hermes_root in std::iter::once(&policy.hermes_home).chain(policy.hermes_root.iter()) {
        denied.extend(
            MEDIA_DELIVERY_DENIED_HERMES_FILES
                .iter()
                .map(|file| hermes_root.join(file)),
        );
    }
    denied
}

fn path_under_denied_prefix(resolved: &Path, policy: &MediaDeliveryPolicy) -> bool {
    let home = policy
        .home
        .as_ref()
        .and_then(|home| canonicalize_or_absolute(home));

    for denied in media_delivery_denied_paths(policy) {
        let Some(resolved_denied) = canonicalize_or_absolute(&denied) else {
            continue;
        };
        if !(resolved == resolved_denied || resolved.starts_with(&resolved_denied)) {
            continue;
        }
        // Root-run gateways have $HOME=/root; the home tree itself can contain
        // legitimate deliverables, while credential subpaths remain separate
        // more-specific denylist entries.
        if home.as_deref() == Some(resolved_denied.as_path()) {
            continue;
        }
        return true;
    }

    false
}

fn file_is_recently_produced(path: &Path, window: Duration) -> bool {
    if window.is_zero() {
        return false;
    }
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age <= window)
        .unwrap_or(false)
}

fn validate_media_delivery_path_with_policy(
    path: &str,
    policy: &MediaDeliveryPolicy,
) -> Option<PathBuf> {
    let candidate = strip_model_path_wrapping(path)?;
    let expanded = expand_home_path(&candidate, policy.home.as_deref())?;
    if !expanded.is_absolute() {
        return None;
    }

    let resolved = std::fs::canonicalize(&expanded).ok()?;
    if !resolved.is_file() {
        return None;
    }

    for root in &policy.allowed_roots {
        let Some(resolved_root) = canonicalize_or_absolute(root) else {
            continue;
        };
        if resolved == resolved_root || resolved.starts_with(&resolved_root) {
            return Some(resolved);
        }
    }

    if !policy.strict_mode {
        return (!path_under_denied_prefix(&resolved, policy)).then_some(resolved);
    }

    if !path_under_denied_prefix(&resolved, policy)
        && file_is_recently_produced(&resolved, policy.recent_window)
    {
        return Some(resolved);
    }

    None
}

/// Return a safe absolute file path for native platform media delivery.
///
/// Default mode accepts existing regular files outside credential and system
/// deny paths. Strict mode (`HERMES_MEDIA_DELIVERY_STRICT=1`) requires either a
/// Hermes/operator allow root or a freshly-produced file inside the recency
/// window. Symlinks are resolved before allow/deny checks.
pub fn validate_media_delivery_path(path: &str) -> Option<PathBuf> {
    validate_media_delivery_path_with_policy(path, &MediaDeliveryPolicy::from_env())
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

        let invalid = cache
            .cache_media_bytes(b"<html>", Some("bad.png"), Some("image/png"), None)
            .await
            .unwrap();
        assert!(invalid.is_none());
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

    fn test_media_policy(
        home: &Path,
        hermes_home: &Path,
        denied_prefixes: Vec<PathBuf>,
    ) -> MediaDeliveryPolicy {
        MediaDeliveryPolicy {
            home: Some(home.to_path_buf()),
            hermes_home: hermes_home.to_path_buf(),
            hermes_root: Some(home.join(".hermes")),
            allowed_roots: Vec::new(),
            denied_prefixes,
            strict_mode: false,
            recent_window: Duration::from_secs(MEDIA_DELIVERY_TRUST_RECENT_DEFAULT_SECONDS),
        }
    }

    #[test]
    fn media_delivery_accepts_running_users_home_deliverable_when_home_prefix_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("root");
        let hermes_home = fake_home.join(".hermes-agent-ultra");
        let workdir = fake_home.join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        std::fs::create_dir_all(&hermes_home).unwrap();
        let doc = workdir.join("proposal.docx");
        std::fs::write(&doc, b"PK\x03\x04").unwrap();
        let policy = test_media_policy(&fake_home, &hermes_home, vec![fake_home.clone()]);

        assert_eq!(
            validate_media_delivery_path_with_policy(&doc.to_string_lossy(), &policy),
            Some(std::fs::canonicalize(&doc).unwrap())
        );
    }

    #[test]
    fn media_delivery_blocks_home_credential_subdir_under_home_exception() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("root");
        let hermes_home = fake_home.join(".hermes-agent-ultra");
        let ssh_dir = fake_home.join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        std::fs::create_dir_all(&hermes_home).unwrap();
        let key = ssh_dir.join("id_rsa");
        std::fs::write(&key, b"-----BEGIN OPENSSH PRIVATE KEY-----").unwrap();
        let policy = test_media_policy(&fake_home, &hermes_home, vec![fake_home.clone()]);

        assert!(
            validate_media_delivery_path_with_policy(&key.to_string_lossy(), &policy).is_none()
        );
    }

    #[test]
    fn media_delivery_blocks_hermes_env_under_home_exception() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("root");
        let hermes_home = fake_home.join(".hermes");
        std::fs::create_dir_all(&hermes_home).unwrap();
        let env_file = hermes_home.join(".env");
        std::fs::write(&env_file, b"OPENROUTER_API_KEY=sk-test").unwrap();
        let policy = test_media_policy(&fake_home, &hermes_home, vec![fake_home.clone()]);

        assert!(
            validate_media_delivery_path_with_policy(&env_file.to_string_lossy(), &policy)
                .is_none()
        );
    }

    #[test]
    fn media_delivery_blocks_other_users_home_when_not_running_home() {
        let tmp = tempfile::tempdir().unwrap();
        let my_home = tmp.path().join("home/me");
        let other_home = tmp.path().join("root");
        let hermes_home = my_home.join(".hermes-agent-ultra");
        std::fs::create_dir_all(&my_home).unwrap();
        std::fs::create_dir_all(&other_home).unwrap();
        std::fs::create_dir_all(&hermes_home).unwrap();
        let other_doc = other_home.join("secret.docx");
        std::fs::write(&other_doc, b"PK\x03\x04").unwrap();
        let policy = test_media_policy(
            &my_home,
            &hermes_home,
            vec![my_home.clone(), other_home.clone()],
        );

        assert!(
            validate_media_delivery_path_with_policy(&other_doc.to_string_lossy(), &policy)
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn media_delivery_blocks_workdir_symlink_to_home_credential() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("root");
        let hermes_home = fake_home.join(".hermes-agent-ultra");
        let ssh_dir = fake_home.join(".ssh");
        let workdir = fake_home.join("work");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        std::fs::create_dir_all(&workdir).unwrap();
        std::fs::create_dir_all(&hermes_home).unwrap();
        let key = ssh_dir.join("id_rsa");
        std::fs::write(&key, b"-----BEGIN OPENSSH PRIVATE KEY-----").unwrap();
        let link = workdir.join("innocent.pdf");
        std::os::unix::fs::symlink(&key, &link).unwrap();
        let policy = test_media_policy(&fake_home, &hermes_home, vec![fake_home.clone()]);

        assert!(
            validate_media_delivery_path_with_policy(&link.to_string_lossy(), &policy).is_none()
        );
    }

    #[test]
    fn media_delivery_normalizes_wrapped_absolute_safe_path() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&hermes_home).unwrap();
        let report = tmp.path().join("deliverable.pdf");
        std::fs::write(&report, b"%PDF-1.4").unwrap();
        let policy = test_media_policy(&home, &hermes_home, vec![tmp.path().join("other-home")]);
        let wrapped = format!("`{}.`", report.display());

        assert_eq!(
            validate_media_delivery_path_with_policy(&wrapped, &policy),
            Some(std::fs::canonicalize(&report).unwrap())
        );
    }
}
