//! Discord inbound attachment download and caching.

use tracing::warn;

use hermes_core::errors::GatewayError;

use super::gateway_loop::DiscordInner;
use super::parse::DiscordAttachment;
use crate::media::{MediaCache, MediaCacheConfig};

/// Default max attachment size (25 MiB, Discord bot limit).
pub const DEFAULT_MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentMediaKind {
    Image,
    Audio,
    Document,
}

pub fn classify_attachment(content_type: Option<&str>, filename: &str) -> AttachmentMediaKind {
    let ct = content_type.unwrap_or("").to_ascii_lowercase();
    if ct.starts_with("image/") {
        return AttachmentMediaKind::Image;
    }
    if ct.starts_with("audio/") || ct == "audio/ogg" {
        return AttachmentMediaKind::Audio;
    }
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
    {
        return AttachmentMediaKind::Image;
    }
    if lower.ends_with(".ogg") || lower.ends_with(".opus") || lower.ends_with(".mp3") || lower.ends_with(".wav")
    {
        return AttachmentMediaKind::Audio;
    }
    AttachmentMediaKind::Document
}

pub fn is_voice_attachment(att: &DiscordAttachment) -> bool {
    att.waveform.is_some()
        || att
            .content_type
            .as_deref()
            .is_some_and(|ct| ct == "audio/ogg" || ct.starts_with("audio/"))
}

fn media_cache_for(inner: &DiscordInner) -> Result<MediaCache, GatewayError> {
    let mut config = MediaCacheConfig::default();
    config.max_file_size = inner.config.max_attachment_bytes;
    MediaCache::new(&config)
}

async fn download_bytes(inner: &DiscordInner, url: &str, max_bytes: u64) -> Result<Vec<u8>, GatewayError> {
    crate::ssrf::validate_url(url)?;
    let resp = inner
        .client
        .get(url)
        .header("Authorization", inner.auth_header())
        .send()
        .await
        .map_err(|e| GatewayError::ConnectionFailed(format!("Discord attachment download: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(GatewayError::ConnectionFailed(format!(
            "Discord attachment HTTP {status}: {text}"
        )));
    }
    let mut buf = Vec::new();
    let mut stream = resp.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Discord attachment read: {e}"))
        })?;
        if max_bytes > 0 && buf.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err(GatewayError::ConnectionFailed(format!(
                "Discord attachment exceeds max size ({max_bytes} bytes)"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Download and cache attachments; returns parallel `media_urls` and `media_types`.
pub async fn cache_message_attachments(
    inner: &DiscordInner,
    attachments: &[DiscordAttachment],
) -> (Vec<String>, Vec<String>) {
    if attachments.is_empty() {
        return (vec![], vec![]);
    }
    let cache = match media_cache_for(inner) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "Discord media cache init failed");
            return (vec![], vec![]);
        }
    };
    let max_bytes = inner.config.max_attachment_bytes;
    let mut urls = Vec::new();
    let mut types = Vec::new();
    for att in attachments {
        if att.size > 0 && max_bytes > 0 && att.size > max_bytes {
            warn!(
                attachment_id = %att.id,
                size = att.size,
                max = max_bytes,
                "Discord attachment skipped: too large"
            );
            continue;
        }
        let filename = if att.filename.is_empty() {
            format!("discord-{}.bin", att.id)
        } else {
            att.filename.clone()
        };
        let content_type = att.content_type.as_deref().unwrap_or("");
        let kind = classify_attachment(att.content_type.as_deref(), &filename);
        let path_result = match download_bytes(inner, &att.url, max_bytes).await {
            Ok(bytes) if !bytes.is_empty() => cache_bytes(&cache, &bytes, kind, &filename).await,
            Ok(_) => {
                warn!(attachment_id = %att.id, "Discord attachment empty body");
                continue;
            }
            Err(e) => {
                warn!(attachment_id = %att.id, error = %e, "Discord attachment download failed");
                continue;
            }
        };
        let Ok(path) = path_result else {
            continue;
        };
        let media_type = if content_type.is_empty() {
            match kind {
                AttachmentMediaKind::Image => "image",
                AttachmentMediaKind::Audio => "audio",
                AttachmentMediaKind::Document => "application/octet-stream",
            }
        } else {
            content_type
        };
        urls.push(path.display().to_string());
        types.push(media_type.to_string());
    }
    (urls, types)
}

async fn cache_bytes(
    cache: &MediaCache,
    bytes: &[u8],
    kind: AttachmentMediaKind,
    filename: &str,
) -> Result<std::path::PathBuf, GatewayError> {
    let subdir = match kind {
        AttachmentMediaKind::Image => "images",
        AttachmentMediaKind::Audio => "audio",
        AttachmentMediaKind::Document => "documents",
    };
    cache.store_bytes(subdir, filename, bytes).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_image_by_mime() {
        assert_eq!(
            classify_attachment(Some("image/png"), "file.bin"),
            AttachmentMediaKind::Image
        );
    }

    #[test]
    fn classify_audio_ogg() {
        assert_eq!(
            classify_attachment(Some("audio/ogg"), "voice.ogg"),
            AttachmentMediaKind::Audio
        );
    }

    #[test]
    fn classify_document_pdf() {
        assert_eq!(
            classify_attachment(Some("application/pdf"), "report.pdf"),
            AttachmentMediaKind::Document
        );
    }

    #[test]
    fn voice_attachment_detects_waveform() {
        let att = DiscordAttachment {
            id: "1".into(),
            filename: "voice.ogg".into(),
            content_type: Some("audio/ogg".into()),
            url: "https://cdn.discordapp.com/a.ogg".into(),
            size: 100,
            waveform: Some(vec![1, 2]),
        };
        assert!(is_voice_attachment(&att));
    }
}
