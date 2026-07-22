use futures::StreamExt;
use hermes_core::ToolError;
use reqwest::Response;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) const MAX_TTS_AUDIO_BYTES: usize = 96 * 1024 * 1024;
pub(crate) const MAX_TTS_JSON_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_TTS_ERROR_BYTES: usize = 64 * 1024;
const MAX_AUDIO_PROBE_BYTES: usize = 128 * 1024;

const DEFAULT_TTS_AUDIO_CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_TTS_AUDIO_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const TTS_AUDIO_CACHE_MAX_BYTES_ENV: &str = "HERMES_TTS_AUDIO_CACHE_MAX_BYTES";
const TTS_AUDIO_CACHE_TTL_SECONDS_ENV: &str = "HERMES_TTS_AUDIO_CACHE_TTL_SECONDS";
static TTS_AUDIO_CACHE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
const TTS_AUDIO_CACHE_QUOTA_LOCK_FILE: &str = ".hermes_tts_cache.quota.lock";
const TTS_AUDIO_CACHE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn read_bounded_response(
    response: Response,
    limit: usize,
    label: &str,
) -> Result<Vec<u8>, ToolError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ToolError::ExecutionFailed(format!(
            "{label} exceeded the {} byte limit",
            limit
        )));
    }

    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(limit);
    let mut bytes = Vec::with_capacity(initial_capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            ToolError::ExecutionFailed(format!("Failed reading {label}: {error}"))
        })?;
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(ToolError::ExecutionFailed(format!(
                "{label} exceeded the {} byte limit",
                limit
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) async fn read_bounded_error(response: Response) -> String {
    let mut bytes = Vec::new();
    let mut truncated = response
        .content_length()
        .is_some_and(|length| length > MAX_TTS_ERROR_BYTES as u64);
    let mut stream = response.bytes_stream();
    while let Some(item) = stream.next().await {
        let Ok(chunk) = item else {
            if bytes.is_empty() {
                return "<failed to read response body>".to_string();
            }
            truncated = true;
            break;
        };
        let remaining = MAX_TTS_ERROR_BYTES.saturating_sub(bytes.len());
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
        if bytes.len() == MAX_TTS_ERROR_BYTES {
            truncated = true;
            break;
        }
    }
    let mut detail = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        detail.push_str("\n[response body truncated]");
    }
    detail
}

fn valid_wav(bytes: &[u8], total_len: usize) -> bool {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return false;
    }
    let declared = u32::from_le_bytes(bytes[4..8].try_into().expect("four byte RIFF size"));
    let Some(container_len) = (declared as usize).checked_add(8) else {
        return false;
    };
    if container_len > total_len {
        return false;
    }

    let mut offset = 12_usize;
    let mut has_format = false;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_len = u32::from_le_bytes(
            bytes[offset + 4..offset + 8]
                .try_into()
                .expect("WAV chunk size"),
        ) as usize;
        let data_start = offset + 8;
        let Some(data_end) = data_start.checked_add(chunk_len) else {
            return false;
        };
        if data_end > total_len || data_end > container_len {
            return false;
        }
        if chunk_id == b"fmt " && chunk_len >= 16 {
            has_format = true;
        }
        if chunk_id == b"data" {
            return has_format && chunk_len > 0;
        }
        let Some(next) = data_end.checked_add(chunk_len & 1) else {
            return false;
        };
        if next > bytes.len() || next > container_len {
            return false;
        }
        offset = next;
    }
    false
}

fn valid_ogg(bytes: &[u8], total_len: usize) -> bool {
    if bytes.len() < 27 || &bytes[..4] != b"OggS" || bytes[4] != 0 {
        return false;
    }
    let segments = bytes[26] as usize;
    if segments == 0 {
        return false;
    }
    let Some(table_end) = 27_usize.checked_add(segments) else {
        return false;
    };
    if table_end > bytes.len() {
        return false;
    }
    let body_len = bytes[27..table_end]
        .iter()
        .try_fold(0_usize, |total, value| total.checked_add(*value as usize));
    let Some(body_len) = body_len.filter(|length| *length > 0) else {
        return false;
    };
    let Some(page_end) = table_end.checked_add(body_len) else {
        return false;
    };
    page_end <= bytes.len() && page_end <= total_len
}

fn valid_ogg_opus(bytes: &[u8], total_len: usize) -> bool {
    if !valid_ogg(bytes, total_len) || bytes[5] & 0x02 == 0 {
        return false;
    }
    let segments = bytes[26] as usize;
    let table_end = 27 + segments;
    let mut first_packet_len = 0_usize;
    let mut first_packet_complete = false;
    for segment in &bytes[27..table_end] {
        first_packet_len = match first_packet_len.checked_add(*segment as usize) {
            Some(length) => length,
            None => return false,
        };
        if *segment < u8::MAX {
            first_packet_complete = true;
            break;
        }
    }
    let Some(first_packet) = bytes
        .get(table_end..)
        .and_then(|body| body.get(..first_packet_len))
    else {
        return false;
    };
    first_packet_complete
        && first_packet.len() >= 19
        && first_packet.starts_with(b"OpusHead")
        && first_packet[8] == 1
        && first_packet[9] != 0
}

fn valid_flac(bytes: &[u8], total_len: usize) -> bool {
    if bytes.len() < 8 || &bytes[..4] != b"fLaC" {
        return false;
    }
    let mut offset = 4_usize;
    let mut stream_info = false;
    loop {
        if offset.checked_add(4).is_none_or(|end| end > bytes.len()) {
            return false;
        }
        let header = bytes[offset];
        let block_type = header & 0x7f;
        let is_last = header & 0x80 != 0;
        let block_len = ((bytes[offset + 1] as usize) << 16)
            | ((bytes[offset + 2] as usize) << 8)
            | bytes[offset + 3] as usize;
        if offset == 4 {
            stream_info = block_type == 0 && block_len == 34;
        }
        let Some(next) = offset
            .checked_add(4)
            .and_then(|value| value.checked_add(block_len))
        else {
            return false;
        };
        if next > bytes.len() || next > total_len {
            return false;
        }
        offset = next;
        if is_last {
            return stream_info && total_len > offset;
        }
    }
}

fn mp3_frame_length(header: u32) -> Option<usize> {
    if header & 0xffe0_0000 != 0xffe0_0000 {
        return None;
    }
    let version = (header >> 19) & 0x3;
    let layer = (header >> 17) & 0x3;
    let bitrate_index = ((header >> 12) & 0xf) as usize;
    let sample_index = ((header >> 10) & 0x3) as usize;
    if version == 1 || layer != 1 || bitrate_index == 0 || bitrate_index == 15 || sample_index == 3
    {
        return None;
    }
    const MPEG1_LAYER3_KBPS: [usize; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    const MPEG2_LAYER3_KBPS: [usize; 16] = [
        0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
    ];
    const MPEG1_SAMPLE_RATES: [usize; 3] = [44_100, 48_000, 32_000];
    let bitrate = if version == 3 {
        MPEG1_LAYER3_KBPS[bitrate_index]
    } else {
        MPEG2_LAYER3_KBPS[bitrate_index]
    } * 1_000;
    let sample_rate = match version {
        3 => MPEG1_SAMPLE_RATES[sample_index],
        2 => MPEG1_SAMPLE_RATES[sample_index] / 2,
        0 => MPEG1_SAMPLE_RATES[sample_index] / 4,
        _ => return None,
    };
    let padding = ((header >> 9) & 1) as usize;
    let coefficient: usize = if version == 3 { 144 } else { 72 };
    coefficient
        .checked_mul(bitrate)?
        .checked_div(sample_rate)?
        .checked_add(padding)
}

fn valid_mp3(bytes: &[u8], total_len: usize) -> bool {
    let mut offset = 0_usize;
    if bytes.starts_with(b"ID3") {
        if bytes.len() < 10 || bytes[6..10].iter().any(|value| value & 0x80 != 0) {
            return false;
        }
        let tag_len = bytes[6..10]
            .iter()
            .fold(0_usize, |size, value| (size << 7) | *value as usize);
        let Some(frame_offset) = 10_usize.checked_add(tag_len) else {
            return false;
        };
        offset = frame_offset;
    }
    if offset.checked_add(4).is_none_or(|end| end > bytes.len()) {
        return false;
    }
    let header = u32::from_be_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("four byte MPEG header"),
    );
    let Some(frame_len) = mp3_frame_length(header) else {
        return false;
    };
    offset
        .checked_add(frame_len)
        .is_some_and(|frame_end| frame_end <= total_len)
}

fn valid_audio_container(bytes: &[u8], total_len: usize, format: &str) -> bool {
    match format {
        "wav" => valid_wav(bytes, total_len),
        "ogg" => valid_ogg(bytes, total_len),
        "flac" => valid_flac(bytes, total_len),
        "mp3" => valid_mp3(bytes, total_len),
        _ => false,
    }
}

pub(crate) fn validate_audio_signature(
    bytes: &[u8],
    format: &str,
    provider: &str,
) -> Result<(), ToolError> {
    if valid_audio_container(bytes, bytes.len(), format) {
        Ok(())
    } else {
        Err(ToolError::ExecutionFailed(format!(
            "TTS provider '{provider}' returned data without a valid {format} audio container"
        )))
    }
}

fn open_tts_audio_for_validation(path: &Path) -> Result<fs::File, ToolError> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself so the shared handle validator rejects
        // it instead of following a subprocess-created link.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed opening TTS output {}: {error}",
            path.display()
        ))
    })?;
    hermes_config::validate_media_file_handle(&file).map_err(|error| {
        ToolError::ExecutionFailed(format!("Refusing TTS output {}: {error}", path.display()))
    })?;
    Ok(file)
}

pub(crate) async fn validate_audio_file(
    path: &Path,
    format: &str,
    provider: &str,
) -> Result<u64, ToolError> {
    let file = open_tts_audio_for_validation(path).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "TTS provider '{provider}' produced no usable output at {}: {error}",
            path.display()
        ))
    })?;
    let metadata = file.metadata().map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed inspecting TTS output {}: {error}",
            path.display()
        ))
    })?;
    if metadata.len() == 0 {
        return Err(ToolError::ExecutionFailed(format!(
            "TTS provider '{provider}' produced no output"
        )));
    }
    if metadata.len() > MAX_TTS_AUDIO_BYTES as u64 {
        return Err(ToolError::ExecutionFailed(format!(
            "TTS provider '{provider}' output exceeded the {} byte limit",
            MAX_TTS_AUDIO_BYTES
        )));
    }

    let mut file = tokio::fs::File::from_std(file);
    let probe_len = usize::try_from(metadata.len().min(MAX_AUDIO_PROBE_BYTES as u64))
        .expect("audio probe length fits usize");
    let mut probe = Vec::with_capacity(probe_len);
    file.take(probe_len as u64)
        .read_to_end(&mut probe)
        .await
        .map_err(|error| {
            ToolError::ExecutionFailed(format!(
                "Failed reading TTS output {}: {error}",
                path.display()
            ))
        })?;
    if !valid_audio_container(&probe, metadata.len() as usize, format) {
        return Err(ToolError::ExecutionFailed(format!(
            "TTS provider '{provider}' returned data without a valid {format} audio container"
        )));
    }
    Ok(metadata.len())
}
/// Verify that an OGG artifact can be handed to a native voice-delivery API.
/// A generic OGG container is not sufficient: the beginning-of-stream packet
/// must identify an Opus bitstream.
pub(crate) async fn validate_ogg_opus_file(path: &Path, provider: &str) -> Result<(), ToolError> {
    let file = open_tts_audio_for_validation(path)?;
    let metadata = file.metadata().map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed inspecting OGG output {}: {error}",
            path.display()
        ))
    })?;
    let total_len = usize::try_from(metadata.len()).map_err(|_| {
        ToolError::ExecutionFailed(format!(
            "OGG output is too large to validate on this platform: {}",
            path.display()
        ))
    })?;
    let probe_len = usize::try_from(metadata.len().min(MAX_AUDIO_PROBE_BYTES as u64))
        .expect("audio probe length fits usize");
    let mut probe = Vec::with_capacity(probe_len);
    let mut file = tokio::fs::File::from_std(file);
    file.take(probe_len as u64)
        .read_to_end(&mut probe)
        .await
        .map_err(|error| {
            ToolError::ExecutionFailed(format!(
                "Failed reading OGG output {}: {error}",
                path.display()
            ))
        })?;
    if valid_ogg_opus(&probe, total_len) {
        Ok(())
    } else {
        Err(ToolError::ExecutionFailed(format!(
            "TTS provider '{provider}' produced an OGG container without an Opus stream"
        )))
    }
}

fn validate_output_path(path: &Path) -> Result<(), ToolError> {
    let rendered = path.to_string_lossy();
    if rendered.contains('\r') || rendered.contains('\n') {
        return Err(ToolError::InvalidParams(
            "TTS output_path must not contain CR or LF".to_string(),
        ));
    }

    if rendered.contains('\0') {
        return Err(ToolError::InvalidParams(
            "TTS output_path must not contain NUL".to_string(),
        ));
    }
    if path.file_name().is_none() {
        return Err(ToolError::InvalidParams(format!(
            "Invalid TTS output path: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Ensure a caller cannot cause an output write through a symlink or replace
/// a non-file destination. The boolean indicates whether a regular
/// destination already exists.
pub(crate) fn inspect_output_destination(path: &Path) -> Result<bool, ToolError> {
    validate_output_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(ToolError::InvalidParams(format!(
            "TTS output path must be a regular file or a new path: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(ToolError::ExecutionFailed(format!(
            "Failed inspecting TTS output {}: {error}",
            path.display()
        ))),
    }
}

fn ensure_regular_temporary(path: &Path) -> Result<(), ToolError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(ToolError::ExecutionFailed(format!(
            "Refusing non-file TTS temporary output: {}",
            path.display()
        ))),
        Err(error) => Err(ToolError::ExecutionFailed(format!(
            "Failed inspecting TTS temporary output {}: {error}",
            path.display()
        ))),
    }
}

fn atomic_temp_path(path: &Path, format: &str) -> Result<PathBuf, ToolError> {
    validate_output_path(path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            ToolError::InvalidParams(format!("Invalid TTS output path: {}", path.display()))
        })?;
    Ok(parent.join(format!(".{name}.{}.tmp.{format}", uuid::Uuid::new_v4())))
}

#[cfg(target_os = "macos")]
mod unix_acl {
    use std::ffi::{c_void, CString};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const ACL_TYPE_EXTENDED: i32 = 0x0000_0100;

    #[link(name = "System")]
    extern "C" {
        fn acl_get_file(path: *const i8, acl_type: i32) -> *mut c_void;
        fn acl_set_file(path: *const i8, acl_type: i32, acl: *mut c_void) -> i32;
        fn acl_free(acl: *mut c_void) -> i32;
    }

    struct OwnedAcl(*mut c_void);

    impl Drop for OwnedAcl {
        fn drop(&mut self) {
            unsafe {
                acl_free(self.0);
            }
        }
    }

    fn c_path(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "TTS path contains NUL"))
    }

    fn extended_acl(path: &Path) -> io::Result<Option<OwnedAcl>> {
        let path = c_path(path)?;
        let acl = unsafe { acl_get_file(path.as_ptr(), ACL_TYPE_EXTENDED) };
        if !acl.is_null() {
            return Ok(Some(OwnedAcl(acl)));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        Err(error)
    }

    fn ensure_regular_source(path: &Path) -> io::Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => Ok(()),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TTS destination is no longer a regular file",
            )),
            Err(error) => Err(error),
        }
    }

    pub fn ensure_path_has_no_extended_acl(path: &Path) -> io::Result<()> {
        if extended_acl(path)?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing TTS output under a path with an extended ACL",
            ));
        }
        Ok(())
    }

    pub fn ensure_parent_has_no_extended_acl(parent: &Path) -> io::Result<()> {
        ensure_path_has_no_extended_acl(parent)
    }

    pub fn synchronize_destination_acl(
        destination: &Path,
        temporary: &Path,
        destination_exists: bool,
    ) -> io::Result<()> {
        if destination_exists {
            ensure_regular_source(destination)?;
            if let Some(acl) = extended_acl(destination)? {
                let temporary = c_path(temporary)?;
                if unsafe { acl_set_file(temporary.as_ptr(), ACL_TYPE_EXTENDED, acl.0) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
        }
        if extended_acl(temporary)?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing TTS temporary output with an unexpected extended ACL",
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod unix_acl {
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const ACCESS_ACL_XATTR: &[u8] = b"system.posix_acl_access\0";
    const DEFAULT_ACL_XATTR: &[u8] = b"system.posix_acl_default\0";

    fn c_path(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "TTS path contains NUL"))
    }

    fn acl_xattr(path: &Path, name: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let path = c_path(path)?;
        let size =
            unsafe { libc::getxattr(path.as_ptr(), name.as_ptr().cast(), std::ptr::null_mut(), 0) };
        if size < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENODATA) {
                return Ok(None);
            }
            return Err(error);
        }
        let mut value = vec![0_u8; size as usize];
        if size == 0 {
            return Ok(Some(value));
        }
        let actual = unsafe {
            libc::getxattr(
                path.as_ptr(),
                name.as_ptr().cast(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        if actual < 0 {
            return Err(io::Error::last_os_error());
        }
        value.truncate(actual as usize);
        Ok(Some(value))
    }

    fn set_acl_xattr(path: &Path, name: &[u8], value: &[u8]) -> io::Result<()> {
        let path = c_path(path)?;
        if unsafe {
            libc::setxattr(
                path.as_ptr(),
                name.as_ptr().cast(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn ensure_regular_source(path: &Path) -> io::Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => Ok(()),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TTS destination is no longer a regular file",
            )),
            Err(error) => Err(error),
        }
    }

    pub fn ensure_path_has_no_extended_acl(path: &Path) -> io::Result<()> {
        if acl_xattr(path, ACCESS_ACL_XATTR)?.is_some()
            || acl_xattr(path, DEFAULT_ACL_XATTR)?.is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing TTS output under a path with an extended ACL",
            ));
        }
        Ok(())
    }

    pub fn ensure_parent_has_no_extended_acl(parent: &Path) -> io::Result<()> {
        ensure_path_has_no_extended_acl(parent)
    }

    pub fn synchronize_destination_acl(
        destination: &Path,
        temporary: &Path,
        destination_exists: bool,
    ) -> io::Result<()> {
        if destination_exists {
            ensure_regular_source(destination)?;
            if let Some(acl) = acl_xattr(destination, ACCESS_ACL_XATTR)? {
                set_acl_xattr(temporary, ACCESS_ACL_XATTR, &acl)?;
                return Ok(());
            }
        }
        if acl_xattr(temporary, ACCESS_ACL_XATTR)?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing TTS temporary output with an unexpected extended ACL",
            ));
        }
        Ok(())
    }
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
mod unix_acl {
    use std::io;
    use std::path::Path;

    pub fn ensure_path_has_no_extended_acl(_path: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "extended ACL verification is unavailable on this platform",
        ))
    }

    pub fn ensure_parent_has_no_extended_acl(_parent: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "extended ACL verification is unavailable on this platform",
        ))
    }

    pub fn synchronize_destination_acl(
        _destination: &Path,
        _temporary: &Path,
        _destination_exists: bool,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "extended ACL preservation is unavailable on this platform",
        ))
    }
}

#[cfg(unix)]
fn ensure_private_parent_acl(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    unix_acl::ensure_parent_has_no_extended_acl(parent)
}

#[cfg(unix)]
pub(crate) fn ensure_safe_private_media_parent(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let lexical_parent = path.parent().unwrap_or_else(|| Path::new("."));
    let effective_uid = unsafe { libc::geteuid() };

    // A symbolic link can change the directory reached by a later subprocess
    // reopen. Permit only links controlled by this user or root, then inspect
    // the canonical ancestry below.
    let mut lexical = lexical_parent;
    loop {
        let metadata = fs::symlink_metadata(lexical)?;
        if metadata.file_type().is_symlink()
            && metadata.uid() != effective_uid
            && metadata.uid() != 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "TTS temporary parent contains an untrusted symbolic link",
            ));
        }
        let Some(next) = lexical.parent() else {
            break;
        };
        lexical = next;
    }

    let canonical_parent = fs::canonicalize(lexical_parent)?;
    let mut ancestor = canonical_parent.as_path();
    loop {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.file_type().is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "TTS temporary parent must resolve to a directory",
            ));
        }
        // Mode bits cannot reveal permissions granted by an ACL. Refuse any
        // ACL on every ancestor that could otherwise make a checked path
        // replaceable between validation and subprocess use.
        unix_acl::ensure_parent_has_no_extended_acl(ancestor)?;
        let mode = metadata.mode();
        let trusted_owner = metadata.uid() == effective_uid || metadata.uid() == 0;
        let protected_from_untrusted_rename = mode & 0o022 == 0 || mode & 0o1000 != 0;
        if !trusted_owner || !protected_from_untrusted_rename {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "TTS temporary parent ancestry must be user/root controlled and non-writable or sticky",
            ));
        }
        let Some(next) = ancestor.parent() else {
            break;
        };
        ancestor = next;
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn validate_private_media_file(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private media file must be a current-user-owned owner-only regular file",
        ));
    }
    unix_acl::ensure_path_has_no_extended_acl(path)
}

#[cfg(windows)]
pub(crate) fn validate_private_media_file(path: &Path) -> std::io::Result<()> {
    windows_private_directory::validate_owner_only_file(path)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn validate_private_media_file(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private media file validation is unsupported on this platform",
    ))
}

#[cfg(windows)]
pub(crate) fn ensure_safe_private_media_parent(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    windows_private_directory::validate_existing(parent)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn ensure_safe_private_media_parent(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private media parent validation is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn synchronize_private_acl(
    destination: &Path,
    temporary: &Path,
    destination_exists: bool,
) -> std::io::Result<()> {
    unix_acl::synchronize_destination_acl(destination, temporary, destination_exists)
}

#[cfg(unix)]
pub(crate) fn create_private_temp_file(
    path: &Path,
    destination: &Path,
    destination_exists: bool,
) -> std::io::Result<fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    // A default/inherited parent ACL can grant access before we have a chance
    // to tighten the newly-created temporary inode. Refuse that situation
    // before creating anything.
    ensure_safe_private_media_parent(path)?;
    ensure_private_parent_acl(path)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let file = options.open(path)?;
    // `mode` is filtered by umask. Set it explicitly so a permissive process
    // umask cannot leave an artifact readable by group or world.
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    // Copy a restrictive existing ACL while the file is still empty. For a
    // new target, reject any unexpected ACL rather than weakening isolation.
    synchronize_private_acl(destination, path, destination_exists)?;
    if !destination_exists {
        if let Err(error) = validate_private_media_file(path) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
    }
    Ok(file)
}

#[cfg(windows)]
pub(crate) fn create_private_temp_file(
    path: &Path,
    destination: &Path,
    destination_exists: bool,
) -> std::io::Result<fs::File> {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{FromRawHandle, RawHandle};

    ensure_safe_private_media_parent(path)?;

    const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
    const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
    const GROUP_SECURITY_INFORMATION: u32 = 0x0000_0002;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const CREATE_NEW: u32 = 1;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        security_descriptor: *mut c_void,
        inherit_handle: i32,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            string_security_descriptor: *const u16,
            string_sd_revision: u32,
            security_descriptor: *mut *mut c_void,
            security_descriptor_size: *mut u32,
        ) -> i32;
        fn GetFileSecurityW(
            file_name: *const u16,
            requested_information: u32,
            security_descriptor: *mut c_void,
            length: u32,
            length_needed: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            file_name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *const SecurityAttributes,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: isize,
        ) -> isize;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    fn wide(path: &Path) -> std::io::Result<Vec<u16>> {
        let mut value = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if value.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path contains NUL",
            ));
        }
        value.push(0);
        Ok(value)
    }

    let temporary_wide = wide(path)?;
    let mut inherited_descriptor = Vec::<u8>::new();
    let mut allocated_descriptor: *mut c_void = std::ptr::null_mut();
    let descriptor = if destination_exists {
        let destination_wide = wide(destination)?;
        let requested =
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        let mut needed = 0_u32;
        let result = unsafe {
            GetFileSecurityW(
                destination_wide.as_ptr(),
                requested,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        if result != 0 || needed == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "existing TTS destination has no readable security descriptor",
            ));
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER) {
            return Err(error);
        }
        inherited_descriptor.resize(needed as usize, 0);
        if unsafe {
            GetFileSecurityW(
                destination_wide.as_ptr(),
                requested,
                inherited_descriptor.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        inherited_descriptor.as_mut_ptr().cast()
    } else {
        // Owner Rights gives the creating owner full access while the
        // protected DACL blocks inherited broad access from the parent.
        let sddl = "D:P(A;;FA;;;OW)"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut descriptor_size = 0_u32;
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut allocated_descriptor,
                &mut descriptor_size,
            )
        } == 0
            || allocated_descriptor.is_null()
            || descriptor_size == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        allocated_descriptor
    };

    let attributes = SecurityAttributes {
        length: size_of::<SecurityAttributes>() as u32,
        security_descriptor: descriptor,
        inherit_handle: 0,
    };
    let handle = unsafe {
        CreateFileW(
            temporary_wide.as_ptr(),
            GENERIC_WRITE,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            0,
        )
    };
    if !allocated_descriptor.is_null() {
        unsafe {
            LocalFree(allocated_descriptor);
        }
    }
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let file = unsafe { fs::File::from_raw_handle(handle as RawHandle) };
    if !destination_exists {
        if let Err(error) = windows_private_directory::validate_owner_only_file(path) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn create_private_temp_file(
    _path: &Path,
    _destination: &Path,
    _destination_exists: bool,
) -> std::io::Result<fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "owner-only TTS output files are unsupported on this platform",
    ))
}

#[cfg(windows)]
mod windows_private_directory {
    use std::ffi::c_void;
    use std::io;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
    const ERROR_ALREADY_EXISTS: i32 = 183;
    const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const INVALID_FILE_ATTRIBUTES: u32 = 0xffff_ffff;
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_USER: u32 = 1;

    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        security_descriptor: *mut c_void,
        inherit_handle: i32,
    }

    #[repr(C)]
    struct SidAndAttributes {
        sid: *mut c_void,
        attributes: u32,
    }

    #[repr(C)]
    struct TokenUser {
        user: SidAndAttributes,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            string_security_descriptor: *const u16,
            string_sd_revision: u32,
            security_descriptor: *mut *mut c_void,
            security_descriptor_size: *mut u32,
        ) -> i32;
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            security_descriptor: *const c_void,
            requested_string_sd_revision: u32,
            security_information: u32,
            string_security_descriptor: *mut *mut u16,
            string_security_descriptor_len: *mut u32,
        ) -> i32;
        fn GetFileSecurityW(
            file_name: *const u16,
            requested_information: u32,
            security_descriptor: *mut c_void,
            length: u32,
            length_needed: *mut u32,
        ) -> i32;
        fn GetSecurityDescriptorOwner(
            security_descriptor: *const c_void,
            owner: *mut *mut c_void,
            owner_defaulted: *mut i32,
        ) -> i32;
        fn OpenProcessToken(
            process_handle: isize,
            desired_access: u32,
            token_handle: *mut isize,
        ) -> i32;
        fn GetTokenInformation(
            token_handle: isize,
            token_information_class: u32,
            token_information: *mut c_void,
            token_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
        fn EqualSid(sid1: *const c_void, sid2: *const c_void) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateDirectoryW(
            path_name: *const u16,
            security_attributes: *const SecurityAttributes,
        ) -> i32;
        fn GetFileAttributesW(file_name: *const u16) -> u32;
        fn GetCurrentProcess() -> isize;
        fn CloseHandle(handle: isize) -> i32;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    struct LocalSecurityDescriptor(*mut c_void);

    impl Drop for LocalSecurityDescriptor {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    LocalFree(self.0);
                }
            }
        }
    }

    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut value = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if value.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains NUL",
            ));
        }
        value.push(0);
        Ok(value)
    }

    fn owner_only_descriptor() -> io::Result<LocalSecurityDescriptor> {
        let sddl = "D:P(A;;FA;;;OW)"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut descriptor = std::ptr::null_mut();
        let mut descriptor_size = 0_u32;
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                &mut descriptor_size,
            )
        } == 0
            || descriptor.is_null()
            || descriptor_size == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(LocalSecurityDescriptor(descriptor))
    }

    fn file_security_descriptor(path: &[u16]) -> io::Result<Vec<u8>> {
        let requested = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        let mut needed = 0_u32;
        if unsafe {
            GetFileSecurityW(
                path.as_ptr(),
                requested,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        } != 0
            || needed == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory has no readable security descriptor",
            ));
        }
        if io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER) {
            return Err(io::Error::last_os_error());
        }
        let mut descriptor = vec![0_u8; needed as usize];
        if unsafe {
            GetFileSecurityW(
                path.as_ptr(),
                requested,
                descriptor.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(descriptor)
    }

    fn owner_is_current_user(security_descriptor: &[u8]) -> io::Result<bool> {
        let mut owner = std::ptr::null_mut();
        let mut owner_defaulted = 0_i32;
        if unsafe {
            GetSecurityDescriptorOwner(
                security_descriptor.as_ptr().cast(),
                &mut owner,
                &mut owner_defaulted,
            )
        } == 0
            || owner.is_null()
        {
            return Err(io::Error::last_os_error());
        }
        let mut token = 0_isize;
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut needed = 0_u32;
        let initial =
            unsafe { GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut needed) };
        if initial != 0 || needed == 0 {
            unsafe {
                CloseHandle(token);
            }
            return Err(io::Error::last_os_error());
        }
        if io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER) {
            unsafe {
                CloseHandle(token);
            }
            return Err(io::Error::last_os_error());
        }
        let mut token_user = vec![0_u8; needed as usize];
        let result = unsafe {
            GetTokenInformation(
                token,
                TOKEN_USER,
                token_user.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        unsafe {
            CloseHandle(token);
        }
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        let user = unsafe { &*(token_user.as_ptr().cast::<TokenUser>()) };
        Ok(unsafe { EqualSid(owner, user.user.sid) } != 0)
    }

    fn has_exact_owner_only_dacl(security_descriptor: &[u8]) -> io::Result<bool> {
        let mut rendered_ptr = std::ptr::null_mut();
        let mut rendered_len = 0_u32;
        if unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                security_descriptor.as_ptr().cast(),
                1,
                DACL_SECURITY_INFORMATION,
                &mut rendered_ptr,
                &mut rendered_len,
            )
        } == 0
            || rendered_ptr.is_null()
        {
            return Err(io::Error::last_os_error());
        }
        let rendered_slice =
            unsafe { std::slice::from_raw_parts(rendered_ptr, rendered_len as usize) };
        let rendered = String::from_utf16_lossy(rendered_slice);
        unsafe {
            LocalFree(rendered_ptr.cast());
        }
        Ok(rendered.trim_end_matches('\0') == "D:P(A;;FA;;;OW)")
    }

    pub fn validate_existing(path: &Path) -> io::Result<()> {
        let path_wide = wide(path)?;
        let attributes = unsafe { GetFileAttributesW(path_wide.as_ptr()) };
        if attributes == INVALID_FILE_ATTRIBUTES {
            return Err(io::Error::last_os_error());
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY == 0
            || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "TTS directory must be a non-reparse directory",
            ));
        }
        let descriptor = file_security_descriptor(&path_wide)?;
        if !owner_is_current_user(&descriptor)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "TTS directory is not owned by the current user",
            ));
        }
        if !has_exact_owner_only_dacl(&descriptor)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "TTS directory does not have a protected owner-only DACL",
            ));
        }
        Ok(())
    }

    pub fn validate_owner_only_file(path: &Path) -> io::Result<()> {
        let path_wide = wide(path)?;
        let attributes = unsafe { GetFileAttributesW(path_wide.as_ptr()) };
        if attributes == INVALID_FILE_ATTRIBUTES {
            return Err(io::Error::last_os_error());
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY != 0
            || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "TTS temporary output must be a non-reparse file",
            ));
        }
        let descriptor = file_security_descriptor(&path_wide)?;
        if !owner_is_current_user(&descriptor)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "TTS temporary output is not owned by the current user",
            ));
        }
        if !has_exact_owner_only_dacl(&descriptor)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "TTS temporary output does not have a protected owner-only DACL",
            ));
        }
        Ok(())
    }

    pub fn create_exclusive(path: &Path) -> io::Result<()> {
        let path_wide = wide(path)?;
        let descriptor = owner_only_descriptor()?;
        let attributes = SecurityAttributes {
            length: size_of::<SecurityAttributes>() as u32,
            security_descriptor: descriptor.0,
            inherit_handle: 0,
        };
        if unsafe { CreateDirectoryW(path_wide.as_ptr(), &attributes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        validate_existing(path)
    }

    pub fn ensure_existing_or_create(path: &Path) -> io::Result<()> {
        match create_exclusive(path) {
            Ok(()) => Ok(()),
            Err(error) if error.raw_os_error() == Some(ERROR_ALREADY_EXISTS) => {
                validate_existing(path)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(unix)]
fn create_private_tts_workspace(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    ensure_safe_private_media_parent(path)?;
    ensure_private_parent_acl(path)?;
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "TTS workspace is not owner-only",
        ));
    }
    ensure_private_parent_acl(&path.join(".tts-acl-probe"))
}

#[cfg(windows)]
fn create_private_tts_workspace(path: &Path) -> std::io::Result<()> {
    // Windows cannot safely fall back to inherited directory ACLs: a local
    // TTS process reopens this pathname after creation.
    ensure_safe_private_media_parent(path)?;
    windows_private_directory::create_exclusive(path)
}

#[cfg(not(any(unix, windows)))]
fn create_private_tts_workspace(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "owner-only TTS workspaces are unsupported on this platform",
    ))
}

#[cfg(unix)]
fn ensure_private_tts_audio_cache_dir() -> Result<PathBuf, ToolError> {
    ensure_private_tts_audio_cache_dir_at(Path::new(&hermes_config::default_media_cache_dir()))
}

#[cfg(unix)]
fn ensure_private_tts_audio_cache_dir_at(cache_root: &Path) -> Result<PathBuf, ToolError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let cache_root = cache_root.to_path_buf();
    if !cache_root.is_absolute() {
        return Err(ToolError::InvalidParams(
            "HERMES_MEDIA_CACHE_DIR must be an absolute path for TTS output".to_string(),
        ));
    }
    let parent = cache_root.parent().ok_or_else(|| {
        ToolError::InvalidParams(format!(
            "Invalid Hermes media cache path: {}",
            cache_root.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed creating Hermes media cache parent {}: {error}",
            parent.display()
        ))
    })?;
    ensure_safe_private_media_parent(&cache_root).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed verifying Hermes media cache parent {}: {error}",
            parent.display()
        ))
    })?;
    ensure_private_parent_acl(&cache_root).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed verifying Hermes media cache parent {}: {error}",
            parent.display()
        ))
    })?;
    match fs::symlink_metadata(&cache_root) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(ToolError::InvalidParams(format!(
                "Hermes media cache path must be a directory: {}",
                cache_root.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(&cache_root).map_err(|error| {
                ToolError::ExecutionFailed(format!(
                    "Failed creating Hermes media cache {}: {error}",
                    cache_root.display()
                ))
            })?;
        }
        Err(error) => {
            return Err(ToolError::ExecutionFailed(format!(
                "Failed inspecting Hermes media cache {}: {error}",
                cache_root.display()
            )))
        }
    }
    let root_metadata = fs::symlink_metadata(&cache_root).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed inspecting Hermes media cache {}: {error}",
            cache_root.display()
        ))
    })?;
    if !root_metadata.file_type().is_dir() || root_metadata.uid() != unsafe { libc::geteuid() } {
        return Err(ToolError::ExecutionFailed(format!(
            "Hermes media cache must be an owner-controlled directory: {}",
            cache_root.display()
        )));
    }
    if root_metadata.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(&cache_root, fs::Permissions::from_mode(0o700)).map_err(|error| {
            ToolError::ExecutionFailed(format!(
                "Failed securing Hermes media cache {}: {error}",
                cache_root.display()
            ))
        })?;
    }
    let root_metadata = fs::symlink_metadata(&cache_root).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed verifying Hermes media cache {}: {error}",
            cache_root.display()
        ))
    })?;
    if !root_metadata.file_type().is_dir()
        || root_metadata.uid() != unsafe { libc::geteuid() }
        || root_metadata.permissions().mode() & 0o077 != 0
    {
        return Err(ToolError::ExecutionFailed(format!(
            "Hermes media cache is not owner-only: {}",
            cache_root.display()
        )));
    }
    let audio_cache = cache_root.join("audio_cache");
    ensure_private_parent_acl(&audio_cache).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed verifying Hermes TTS cache parent {}: {error}",
            cache_root.display()
        ))
    })?;
    match fs::symlink_metadata(&audio_cache) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(ToolError::InvalidParams(format!(
                "Hermes TTS cache path must be a directory: {}",
                audio_cache.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(&audio_cache).map_err(|error| {
                ToolError::ExecutionFailed(format!(
                    "Failed creating Hermes TTS cache {}: {error}",
                    audio_cache.display()
                ))
            })?;
        }
        Err(error) => {
            return Err(ToolError::ExecutionFailed(format!(
                "Failed inspecting Hermes TTS cache {}: {error}",
                audio_cache.display()
            )))
        }
    }
    let metadata = fs::symlink_metadata(&audio_cache).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed verifying Hermes TTS cache ownership {}: {error}",
            audio_cache.display()
        ))
    })?;
    if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(ToolError::ExecutionFailed(format!(
            "Hermes TTS cache must be an owner-controlled directory: {}",
            audio_cache.display()
        )));
    }
    fs::set_permissions(&audio_cache, fs::Permissions::from_mode(0o700)).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed securing Hermes TTS cache {}: {error}",
            audio_cache.display()
        ))
    })?;
    let metadata = fs::symlink_metadata(&audio_cache).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed verifying Hermes TTS cache {}: {error}",
            audio_cache.display()
        ))
    })?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(ToolError::ExecutionFailed(format!(
            "Hermes TTS cache is not owner-only: {}",
            audio_cache.display()
        )));
    }
    // Check the directory itself for ACLs after creation; an inherited ACL
    // could bypass restrictive Unix mode bits.
    ensure_private_parent_acl(&audio_cache.join(".tts-acl-probe")).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed verifying Hermes TTS cache ACL {}: {error}",
            audio_cache.display()
        ))
    })?;
    fs::canonicalize(&audio_cache).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed canonicalizing Hermes TTS cache {}: {error}",
            audio_cache.display()
        ))
    })
}

#[cfg(unix)]
fn ensure_owner_only_temporary(path: &Path) -> Result<(), ToolError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::symlink_metadata(path)
        .map_err(|error| {
            ToolError::ExecutionFailed(format!(
                "Failed reading TTS temporary output permissions {}: {error}",
                path.display()
            ))
        })?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        return Ok(());
    }
    Err(ToolError::ExecutionFailed(format!(
        "Refusing TTS temporary output with group or world permissions: {}",
        path.display()
    )))
}

#[cfg(unix)]
fn preserve_destination_mode(temporary: &Path, destination: &Path) -> Result<(), ToolError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_file() => metadata.permissions().mode() & 0o777,
        Ok(_) => {
            return Err(ToolError::InvalidParams(format!(
                "TTS output path must be a regular file or a new path: {}",
                destination.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0o600,
        Err(error) => {
            return Err(ToolError::ExecutionFailed(format!(
                "Failed reading TTS output permissions {}: {error}",
                destination.display()
            )))
        }
    };
    fs::set_permissions(temporary, fs::Permissions::from_mode(mode)).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed preserving TTS output permissions for {}: {error}",
            destination.display()
        ))
    })
}

fn commit_private_temporary(temporary: &Path, destination: &Path) -> Result<(), ToolError> {
    let destination_exists = inspect_output_destination(destination)?;
    ensure_regular_temporary(temporary)?;
    #[cfg(unix)]
    {
        ensure_owner_only_temporary(temporary)?;
        preserve_destination_mode(temporary, destination)?;
        synchronize_private_acl(destination, temporary, destination_exists).map_err(|error| {
            ToolError::ExecutionFailed(format!(
                "Failed preserving TTS output ACL for {}: {error}",
                destination.display()
            ))
        })?;
    }
    hermes_config::atomic_replace_file(temporary, destination).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed committing TTS output {}: {error}",
            destination.display()
        ))
    })
}

#[cfg(windows)]
fn ensure_private_tts_audio_cache_dir() -> Result<PathBuf, ToolError> {
    let cache_root = PathBuf::from(hermes_config::default_media_cache_dir());
    if !cache_root.is_absolute() {
        return Err(ToolError::InvalidParams(
            "HERMES_MEDIA_CACHE_DIR must be an absolute path for TTS output".to_string(),
        ));
    }
    let parent = cache_root.parent().ok_or_else(|| {
        ToolError::InvalidParams(format!(
            "Invalid Hermes media cache path: {}",
            cache_root.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed creating Hermes media cache parent {}: {error}",
            parent.display()
        ))
    })?;
    windows_private_directory::ensure_existing_or_create(&cache_root).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed validating Hermes media cache {}: {error}",
            cache_root.display()
        ))
    })?;
    let audio_cache = cache_root.join("audio_cache");
    windows_private_directory::ensure_existing_or_create(&audio_cache).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed validating Hermes TTS cache {}: {error}",
            audio_cache.display()
        ))
    })?;
    fs::canonicalize(&audio_cache).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed canonicalizing Hermes TTS cache {}: {error}",
            audio_cache.display()
        ))
    })
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_tts_audio_cache_dir() -> Result<PathBuf, ToolError> {
    Err(ToolError::ExecutionFailed(
        "owner-only Hermes TTS cache directories are unsupported on this platform".to_string(),
    ))
}

#[derive(Debug)]
struct ManagedTtsCacheEntry {
    path: PathBuf,
    bytes: u64,
}

fn is_managed_tts_cache_name(name: &str) -> bool {
    name.starts_with("hermes_tts_") || name.starts_with("hermes_minimax_")
}

fn is_managed_tts_cache_temporary_name(name: &str) -> bool {
    name.starts_with(".hermes_tts_") || name.starts_with(".hermes_minimax_")
}

fn configured_tts_audio_cache_max_bytes() -> u64 {
    std::env::var(TTS_AUDIO_CACHE_MAX_BYTES_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TTS_AUDIO_CACHE_MAX_BYTES)
}

fn configured_tts_audio_cache_ttl() -> Duration {
    std::env::var(TTS_AUDIO_CACHE_TTL_SECONDS_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TTS_AUDIO_CACHE_TTL)
}

fn collect_and_cleanup_managed_tts_cache(
    cache_dir: &Path,
    ttl: Duration,
) -> Result<Vec<ManagedTtsCacheEntry>, ToolError> {
    let now = SystemTime::now();
    let mut entries = Vec::new();
    for entry in fs::read_dir(cache_dir).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed reading Hermes TTS cache {}: {error}",
            cache_dir.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            ToolError::ExecutionFailed(format!(
                "Failed reading Hermes TTS cache entry {}: {error}",
                cache_dir.display()
            ))
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_managed_tts_cache_name(name) && !is_managed_tts_cache_temporary_name(name) {
            continue;
        }
        // A temporary artifact can still be actively written by a different
        // process. It is never eligible for cache cleanup or quota accounting.
        if is_managed_tts_cache_temporary_name(name) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            ToolError::ExecutionFailed(format!(
                "Failed inspecting Hermes TTS cache entry {}: {error}",
                path.display()
            ))
        })?;
        if !metadata.file_type().is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let expired = now.duration_since(modified).unwrap_or_default() > ttl;
        if expired {
            fs::remove_file(&path).map_err(|error| {
                ToolError::ExecutionFailed(format!(
                    "Failed removing expired Hermes TTS cache entry {}: {error}",
                    path.display()
                ))
            })?;
            continue;
        }
        if is_managed_tts_cache_name(name) {
            entries.push(ManagedTtsCacheEntry {
                path,
                bytes: metadata.len(),
            });
        }
    }
    Ok(entries)
}

#[cfg(unix)]
struct ManagedTtsCacheProcessLock {
    _file: fs::File,
}

#[cfg(unix)]
fn lock_managed_tts_cache_process(
    cache_dir: &Path,
) -> Result<ManagedTtsCacheProcessLock, ToolError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let path = cache_dir.join(TTS_AUDIO_CACHE_QUOTA_LOCK_FILE);
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    let file = options.open(&path).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed opening Hermes TTS cache coordination lock {}: {error}",
            path.display()
        ))
    })?;
    let metadata = file.metadata().map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed inspecting Hermes TTS cache coordination lock {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(ToolError::ExecutionFailed(format!(
            "Hermes TTS cache coordination lock is not a private owner-only regular file: {}",
            path.display()
        )));
    }
    let deadline = std::time::Instant::now() + TTS_AUDIO_CACHE_LOCK_TIMEOUT;
    loop {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK => {
                if std::time::Instant::now() >= deadline {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Timed out acquiring Hermes TTS cache coordination lock {}",
                        path.display()
                    )));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                return Err(ToolError::ExecutionFailed(format!(
                    "Failed acquiring Hermes TTS cache coordination lock {}: {error}",
                    path.display()
                )));
            }
        }
    }
    // Reject a lock pathname swapped while its inode was being acquired.
    let named_metadata = fs::symlink_metadata(&path).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed rechecking Hermes TTS cache coordination lock {}: {error}",
            path.display()
        ))
    })?;
    if named_metadata.dev() != metadata.dev() || named_metadata.ino() != metadata.ino() {
        return Err(ToolError::ExecutionFailed(format!(
            "Hermes TTS cache coordination lock changed while acquiring it: {}",
            path.display()
        )));
    }
    validate_private_media_file(&path).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Hermes TTS cache coordination lock is not private: {}: {error}",
            path.display()
        ))
    })?;
    Ok(ManagedTtsCacheProcessLock { _file: file })
}

#[cfg(windows)]
struct ManagedTtsCacheProcessLock {
    file: fs::File,
}

#[cfg(windows)]
fn lock_managed_tts_cache_process(
    cache_dir: &Path,
) -> Result<ManagedTtsCacheProcessLock, ToolError> {
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: isize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LockFileEx(
            file: isize,
            flags: u32,
            reserved: u32,
            bytes_low: u32,
            bytes_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }

    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    let path = cache_dir.join(TTS_AUDIO_CACHE_QUOTA_LOCK_FILE);
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&path)
        .map_err(|error| {
            ToolError::ExecutionFailed(format!(
                "Failed opening Hermes TTS cache coordination lock {}: {error}",
                path.display()
            ))
        })?;
    validate_private_media_file(&path).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Hermes TTS cache coordination lock is not private: {}: {error}",
            path.display()
        ))
    })?;
    let deadline = std::time::Instant::now() + TTS_AUDIO_CACHE_LOCK_TIMEOUT;
    loop {
        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: 0,
        };
        if unsafe {
            LockFileEx(
                file.as_raw_handle() as isize,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        } != 0
        {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        let detail = if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
            "timed out"
        } else {
            "failed"
        };
        return Err(ToolError::ExecutionFailed(format!(
            "{detail} acquiring Hermes TTS cache coordination lock {}: {error}",
            path.display()
        )));
    }
    // `UnlockFileEx` needs an identically shaped OVERLAPPED, so retain only
    // the file; closing an exclusive file lock also releases it on process
    // exit. The normal path unlocks explicitly in Drop below.
    Ok(ManagedTtsCacheProcessLock { file })
}

#[cfg(windows)]
impl Drop for ManagedTtsCacheProcessLock {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle;

        #[repr(C)]
        struct Overlapped {
            internal: usize,
            internal_high: usize,
            offset: u32,
            offset_high: u32,
            event: isize,
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn UnlockFileEx(
                file: isize,
                reserved: u32,
                bytes_low: u32,
                bytes_high: u32,
                overlapped: *mut Overlapped,
            ) -> i32;
        }

        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: 0,
        };
        unsafe {
            UnlockFileEx(
                self.file.as_raw_handle() as isize,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            );
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn lock_managed_tts_cache_process(_cache_dir: &Path) -> Result<(), ToolError> {
    Err(ToolError::ExecutionFailed(
        "cross-process Hermes TTS cache quota coordination is unsupported on this platform"
            .to_string(),
    ))
}

fn enforce_managed_tts_cache_quota(
    cache_dir: &Path,
    replacing: Option<&Path>,
    incoming_bytes: u64,
) -> Result<(), ToolError> {
    enforce_managed_tts_cache_quota_with_limits(
        cache_dir,
        replacing,
        incoming_bytes,
        configured_tts_audio_cache_max_bytes(),
        configured_tts_audio_cache_ttl(),
    )
}

fn enforce_managed_tts_cache_quota_with_limits(
    cache_dir: &Path,
    replacing: Option<&Path>,
    incoming_bytes: u64,
    max_bytes: u64,
    ttl: Duration,
) -> Result<(), ToolError> {
    let entries = collect_and_cleanup_managed_tts_cache(cache_dir, ttl)?;
    let replacing = replacing.map(Path::to_path_buf);
    let retained_bytes = entries
        .iter()
        .filter(|entry| Some(entry.path.as_path()) != replacing.as_deref())
        .fold(0_u64, |total, entry| total.saturating_add(entry.bytes));
    // Returned paths can still be awaiting gateway delivery. Only expired
    // managed files are eligible for cleanup; reject new work rather than
    // deleting a fresh artifact before its consumer can read it.
    if retained_bytes.saturating_add(incoming_bytes) > max_bytes {
        return Err(ToolError::ExecutionFailed(format!(
            "Hermes TTS cache quota exceeded (cached={retained_bytes}, incoming={incoming_bytes}, max={max_bytes})"
        )));
    }
    Ok(())
}

/// Reserve a canonical, owner-only cache path for a default TTS artifact.
/// Explicit caller-supplied paths never use this API and are never cache-evicted.
pub(crate) fn default_tts_audio_path(prefix: &str, format: &str) -> Result<PathBuf, ToolError> {
    if !matches!(prefix, "hermes_tts" | "hermes_minimax")
        || !matches!(format, "mp3" | "wav" | "ogg" | "flac")
    {
        return Err(ToolError::InvalidParams(
            "Invalid default TTS cache artifact name".to_string(),
        ));
    }
    let _lock = TTS_AUDIO_CACHE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let cache_dir = ensure_private_tts_audio_cache_dir()?;
    let _process_lock = lock_managed_tts_cache_process(&cache_dir)?;
    enforce_managed_tts_cache_quota(&cache_dir, None, 0)?;
    Ok(cache_dir.join(format!("{prefix}_{}.{}", uuid::Uuid::new_v4(), format)))
}

fn commit_managed_tts_cache_temporary(
    temporary: &Path,
    destination: &Path,
    bytes: u64,
) -> Result<(), ToolError> {
    let _lock = TTS_AUDIO_CACHE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let cache_dir = ensure_private_tts_audio_cache_dir()?;
    let _process_lock = lock_managed_tts_cache_process(&cache_dir)?;
    let parent = destination.parent().ok_or_else(|| {
        ToolError::InvalidParams(format!(
            "Invalid Hermes TTS cache destination: {}",
            destination.display()
        ))
    })?;
    let parent = fs::canonicalize(parent).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed canonicalizing Hermes TTS cache destination parent {}: {error}",
            parent.display()
        ))
    })?;
    if parent != cache_dir
        || !destination
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_managed_tts_cache_name)
    {
        return Err(ToolError::InvalidParams(format!(
            "Managed TTS cache destination is outside its private cache: {}",
            destination.display()
        )));
    }
    enforce_managed_tts_cache_quota(&cache_dir, Some(destination), bytes)?;
    commit_private_temporary(temporary, destination)
}

fn is_managed_tts_cache_destination(path: &Path) -> bool {
    if !path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_managed_tts_cache_name)
    {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(parent) = fs::canonicalize(parent) else {
        return false;
    };
    let configured_cache =
        PathBuf::from(hermes_config::default_media_cache_dir()).join("audio_cache");
    fs::canonicalize(configured_cache).is_ok_and(|cache| cache == parent)
}

struct TempAudioGuard(Option<PathBuf>);

impl TempAudioGuard {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn disarm(&mut self) {
        self.0.take();
    }
}

impl Drop for TempAudioGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

pub(crate) async fn write_validated_audio(
    path: &Path,
    bytes: &[u8],
    format: &str,
    provider: &str,
) -> Result<u64, ToolError> {
    // Validate the caller-controlled destination before inspecting provider
    // bytes or constructing any temporary sibling.
    let destination_exists = inspect_output_destination(path)?;
    if bytes.len() > MAX_TTS_AUDIO_BYTES {
        return Err(ToolError::ExecutionFailed(format!(
            "TTS provider '{provider}' output exceeded the {} byte limit",
            MAX_TTS_AUDIO_BYTES
        )));
    }
    validate_audio_signature(bytes, format, provider)?;

    let managed_destination = is_managed_tts_cache_destination(path);
    let temporary = atomic_temp_path(path, format)?;
    let mut temporary_guard = TempAudioGuard::new(temporary.clone());
    let file = create_private_temp_file(&temporary, path, destination_exists).map_err(|error| {
        ToolError::ExecutionFailed(format!("Failed creating private TTS output: {error}"))
    })?;
    let mut file = tokio::fs::File::from_std(file);
    file.write_all(bytes).await.map_err(|error| {
        ToolError::ExecutionFailed(format!("Failed writing TTS output: {error}"))
    })?;
    file.sync_all().await.map_err(|error| {
        ToolError::ExecutionFailed(format!("Failed syncing TTS output: {error}"))
    })?;
    drop(file);
    if managed_destination {
        commit_managed_tts_cache_temporary(&temporary, path, bytes.len() as u64)?;
    } else {
        commit_private_temporary(&temporary, path)?;
    }
    temporary_guard.disarm();
    Ok(bytes.len() as u64)
}

pub(crate) async fn commit_generated_audio(
    generated: &Path,
    destination: &Path,
    format: &str,
    provider: &str,
) -> Result<u64, ToolError> {
    ensure_regular_temporary(generated)?;
    let bytes = validate_audio_file(generated, format, provider).await?;
    if is_managed_tts_cache_destination(destination) {
        commit_managed_tts_cache_temporary(generated, destination, bytes)?;
    } else {
        commit_private_temporary(generated, destination)?;
    }
    Ok(bytes)
}

pub(crate) fn validate_requested_output_format(
    output_path: Option<&str>,
    expected_format: &str,
    provider: &str,
) -> Result<(), ToolError> {
    let Some(output_path) = output_path else {
        return Ok(());
    };
    validate_output_path(Path::new(output_path))?;
    let extension = Path::new(output_path)
        .extension()
        .and_then(|value| value.to_str());
    if extension.is_some_and(|value| value.eq_ignore_ascii_case(expected_format)) {
        return Ok(());
    }
    Err(ToolError::InvalidParams(format!(
        "TTS provider '{provider}' produces {expected_format} audio; output_path must end in .{expected_format}"
    )))
}

pub(crate) fn generated_audio_path(path: &Path, format: &str) -> Result<PathBuf, ToolError> {
    let destination_exists = inspect_output_destination(path)?;
    let temporary = atomic_temp_path(path, format)?;
    if let Err(error) = create_private_temp_file(&temporary, path, destination_exists) {
        let _ = fs::remove_file(&temporary);
        return Err(ToolError::ExecutionFailed(format!(
            "Failed creating private TTS output: {error}"
        )));
    }
    Ok(temporary)
}

/// Create a private, empty directory beside a destination for a local TTS
/// subprocess. The caller can monitor this directory without admitting
/// unrelated files from an explicit output path's parent.
pub(crate) fn generated_audio_workspace_path(
    destination: &Path,
    format: &str,
) -> Result<(PathBuf, PathBuf), ToolError> {
    if !matches!(format, "mp3" | "wav" | "ogg" | "flac") {
        return Err(ToolError::InvalidParams(
            "Invalid TTS workspace audio format".to_string(),
        ));
    }
    inspect_output_destination(destination)?;
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let workspace = parent.join(format!(".hermes_tts_workspace_{}", uuid::Uuid::new_v4()));
    create_private_tts_workspace(&workspace).map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Failed creating private TTS workspace {}: {error}",
            workspace.display()
        ))
    })?;
    let generated = workspace.join(format!("audio.{format}"));
    if let Err(error) = create_private_temp_file(&generated, &generated, false) {
        let _ = fs::remove_dir(&workspace);
        return Err(ToolError::ExecutionFailed(format!(
            "Failed creating private TTS workspace output {}: {error}",
            generated.display()
        )));
    }
    Ok((workspace, generated))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_wav() -> Vec<u8> {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&38_u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&8_000_u32.to_le_bytes());
        wav.extend_from_slice(&16_000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&2_u32.to_le_bytes());
        wav.extend_from_slice(&0_i16.to_le_bytes());
        wav
    }

    fn tiny_ogg() -> Vec<u8> {
        let mut ogg = vec![0_u8; 28];
        ogg[..4].copy_from_slice(b"OggS");
        ogg[26] = 1;
        ogg[27] = 4;
        ogg.extend_from_slice(b"Opus");
        ogg
    }

    fn tiny_opus_ogg() -> Vec<u8> {
        let mut ogg = vec![0_u8; 28];
        ogg[..4].copy_from_slice(b"OggS");
        ogg[5] = 2; // beginning-of-stream
        ogg[26] = 1;
        ogg[27] = 19;
        ogg.extend_from_slice(b"OpusHead");
        ogg.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        ogg
    }

    fn tiny_flac() -> Vec<u8> {
        let mut flac = b"fLaC".to_vec();
        flac.extend_from_slice(&[0x80, 0, 0, 34]);
        flac.extend_from_slice(&[0_u8; 34]);
        flac.push(0);
        flac
    }

    fn tiny_mp3() -> Vec<u8> {
        let mut frame = vec![0_u8; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
        frame
    }

    #[cfg(unix)]
    fn set_modified_to_epoch(path: &Path) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let times = [
            libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
        ];
        assert_eq!(unsafe { libc::utimes(path.as_ptr(), times.as_ptr()) }, 0);
    }

    #[test]
    fn accepts_minimal_structural_audio_containers() {
        for (format, bytes) in [
            ("wav", tiny_wav()),
            ("ogg", tiny_ogg()),
            ("flac", tiny_flac()),
            ("mp3", tiny_mp3()),
        ] {
            validate_audio_signature(&bytes, format, "fixture").unwrap();
        }
    }

    #[test]
    fn rejects_truncated_structural_audio_containers() {
        for (format, mut bytes) in [
            ("wav", tiny_wav()),
            ("ogg", tiny_ogg()),
            ("flac", tiny_flac()),
            ("mp3", tiny_mp3()),
        ] {
            bytes.pop();
            let error = validate_audio_signature(&bytes, format, "fixture").unwrap_err();
            assert!(error.to_string().contains("valid"));
        }
    }

    #[test]
    fn rejects_magic_only_and_unknown_formats() {
        for (format, bytes) in [
            ("wav", b"RIFF0000WAVE".as_slice()),
            ("ogg", b"OggS".as_slice()),
            ("flac", b"fLaC".as_slice()),
            ("mp3", &[0xff, 0xfb, 0x90, 0x64]),
            ("aac", b"audio".as_slice()),
        ] {
            assert!(validate_audio_signature(bytes, format, "fixture").is_err());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hard_linked_subprocess_output_is_rejected_from_its_open_handle() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.wav");
        let output = directory.path().join("output.wav");
        std::fs::write(&source, tiny_wav()).unwrap();
        std::fs::hard_link(&source, &output).unwrap();

        let error = validate_audio_file(&output, "wav", "fixture")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("hard-linked"));
    }

    #[tokio::test]
    async fn voice_delivery_ogg_requires_an_opus_stream() {
        let directory = tempfile::tempdir().unwrap();
        let non_opus = directory.path().join("non-opus.ogg");
        let opus = directory.path().join("opus.ogg");
        std::fs::write(&non_opus, tiny_ogg()).unwrap();
        std::fs::write(&opus, tiny_opus_ogg()).unwrap();

        let error = validate_ogg_opus_file(&non_opus, "fixture")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("without an Opus stream"));
        validate_ogg_opus_file(&opus, "fixture").await.unwrap();
    }
    #[test]
    fn requested_output_path_rejects_crlf_before_extension_validation() {
        let error =
            validate_requested_output_format(Some("speech\r\n.wav"), "wav", "fixture").unwrap_err();
        assert!(matches!(error, ToolError::InvalidParams(_)));
        assert!(error.to_string().contains("CR or LF"));
    }

    #[tokio::test]
    async fn crlf_output_path_creates_no_file() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("speech\r\n.wav");
        let error = write_validated_audio(&output, &tiny_wav(), "wav", "fixture")
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidParams(_)));
        assert!(error.to_string().contains("CR or LF"));
        assert!(!output.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn audio_outputs_are_owner_only_and_retain_existing_mode() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let new_output = directory.path().join("new.wav");
        write_validated_audio(&new_output, &tiny_wav(), "wav", "fixture")
            .await
            .unwrap();
        assert_eq!(
            std::fs::metadata(&new_output).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let existing_output = directory.path().join("existing.wav");
        std::fs::write(&existing_output, b"old destination").unwrap();
        std::fs::set_permissions(&existing_output, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_validated_audio(&existing_output, &tiny_wav(), "wav", "fixture")
            .await
            .unwrap();

        assert_eq!(std::fs::read(&existing_output).unwrap(), tiny_wav());
        assert_eq!(
            std::fs::metadata(&existing_output)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn restrictive_macos_acl_is_preserved_across_replacement() {
        fn acl_entries(path: &Path) -> String {
            let output = std::process::Command::new("/bin/ls")
                .args(["-le", path.to_str().unwrap()])
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n")
        }

        let directory = tempfile::tempdir().unwrap();
        let fresh_output = directory.path().join("fresh.wav");
        write_validated_audio(&fresh_output, &tiny_wav(), "wav", "fixture")
            .await
            .unwrap();
        assert!(acl_entries(&fresh_output).is_empty());

        let destination = directory.path().join("existing.wav");
        std::fs::write(&destination, b"old destination").unwrap();
        let status = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone deny write", destination.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());
        let before = acl_entries(&destination);
        assert!(before.contains("group:everyone deny write"));

        write_validated_audio(&destination, &tiny_wav(), "wav", "fixture")
            .await
            .unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), tiny_wav());
        assert_eq!(acl_entries(&destination), before);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn inherited_parent_acl_fails_before_creating_a_temporary_output() {
        let directory = tempfile::tempdir().unwrap();
        let acl_parent = directory.path().join("acl-parent");
        std::fs::create_dir(&acl_parent).unwrap();
        let status = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone deny write", acl_parent.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());

        let output = acl_parent.join("speech.wav");
        let result = write_validated_audio(&output, &tiny_wav(), "wav", "fixture").await;
        let cleanup = std::process::Command::new("/bin/chmod")
            .args(["-N", acl_parent.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(cleanup.success());

        let error = result.unwrap_err();
        assert!(error.to_string().contains("extended ACL"));
        assert!(!output.exists());
        assert!(std::fs::read_dir(&acl_parent).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn generated_audio_temporary_is_owner_only_from_creation() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("destination.wav");
        let generated = generated_audio_path(&destination, "wav").unwrap();

        assert_eq!(
            std::fs::metadata(&generated).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_file(generated).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn insecure_generated_temporary_cannot_replace_existing_destination() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("existing.wav");
        let generated = directory.path().join("generated.wav");
        std::fs::write(&destination, b"old destination").unwrap();
        std::fs::write(&generated, tiny_wav()).unwrap();
        std::fs::set_permissions(&generated, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = commit_generated_audio(&generated, &destination, "wav", "fixture")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("group or world"));
        assert_eq!(std::fs::read(&destination).unwrap(), b"old destination");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_atomic_replace_preserves_existing_destination() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("existing.wav");
        let temporary = directory.path().join("replacement.wav");
        std::fs::write(&destination, b"old destination").unwrap();
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&temporary, tiny_wav()).unwrap();
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600)).unwrap();

        let original_directory_mode = std::fs::metadata(directory.path())
            .unwrap()
            .permissions()
            .mode();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = commit_private_temporary(&temporary, &destination);
        std::fs::set_permissions(
            directory.path(),
            std::fs::Permissions::from_mode(original_directory_mode),
        )
        .unwrap();

        assert!(result.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"old destination");
    }

    #[tokio::test]
    async fn failed_temporary_commit_preserves_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("existing.wav");
        std::fs::write(&destination, b"old destination").unwrap();
        let missing_temporary = directory.path().join("missing.wav");

        let error = commit_private_temporary(&missing_temporary, &destination).unwrap_err();

        assert!(error.to_string().contains("temporary"));
        assert_eq!(std::fs::read(&destination).unwrap(), b"old destination");
    }
    #[cfg(unix)]
    #[test]
    fn default_tts_cache_directory_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let cache = ensure_private_tts_audio_cache_dir_at(directory.path()).unwrap();

        assert_eq!(
            cache,
            std::fs::canonicalize(directory.path().join("audio_cache")).unwrap()
        );
        assert_eq!(
            std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "invoked by cache_quota_lock_serializes_processes"]
    fn cache_quota_lock_child() {
        let cache = PathBuf::from(
            std::env::var("HERMES_TTS_CACHE_LOCK_TEST_DIR").expect("child cache directory"),
        );
        let ready = PathBuf::from(
            std::env::var("HERMES_TTS_CACHE_LOCK_TEST_READY").expect("child readiness path"),
        );
        let _lock = lock_managed_tts_cache_process(&cache).expect("child acquires cache lock");
        std::fs::write(ready, b"locked").expect("child signals lock");
        std::thread::sleep(Duration::from_millis(300));
    }

    #[cfg(unix)]
    #[test]
    fn cache_quota_lock_serializes_processes() {
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("audio_cache");
        std::fs::create_dir(&cache).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
        let ready = directory.path().join("lock-ready");
        let executable = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(executable)
            .args([
                "--ignored",
                "--exact",
                "tts_audio::tests::cache_quota_lock_child",
                "--nocapture",
            ])
            .env("HERMES_TTS_CACHE_LOCK_TEST_DIR", &cache)
            .env("HERMES_TTS_CACHE_LOCK_TEST_READY", &ready)
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "cache lock child did not signal readiness"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let waited = std::time::Instant::now();
        let _lock = lock_managed_tts_cache_process(&cache).unwrap();
        assert!(
            waited.elapsed() >= Duration::from_millis(100),
            "cache quota lock did not wait for the other process"
        );
        assert!(child.wait().unwrap().success());
    }

    #[cfg(unix)]
    #[test]
    fn expired_cache_temporary_is_not_cleaned_while_it_may_be_active() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("audio_cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
        let temporary = cache.join(".hermes_tts_active.tmp.wav");
        std::fs::write(&temporary, tiny_wav()).unwrap();
        set_modified_to_epoch(&temporary);

        let entries =
            collect_and_cleanup_managed_tts_cache(&cache, Duration::from_secs(60)).unwrap();
        assert!(entries.is_empty());
        assert!(temporary.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repeated_managed_cache_generation_stays_under_quota_and_keeps_explicit_output() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("audio_cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
        let explicit_output = directory.path().join("explicit.wav");
        write_validated_audio(&explicit_output, &tiny_wav(), "wav", "fixture")
            .await
            .unwrap();

        let bytes = tiny_wav();
        let quota = (bytes.len() as u64) * 2;
        for index in 0..2 {
            enforce_managed_tts_cache_quota_with_limits(
                &cache,
                None,
                bytes.len() as u64,
                quota,
                Duration::from_secs(60),
            )
            .unwrap();
            let output = cache.join(format!("hermes_tts_{index}.wav"));
            write_validated_audio(&output, &bytes, "wav", "fixture")
                .await
                .unwrap();
        }
        let blocked = enforce_managed_tts_cache_quota_with_limits(
            &cache,
            None,
            bytes.len() as u64,
            quota,
            Duration::from_secs(60),
        )
        .unwrap_err();
        assert!(blocked.to_string().contains("quota exceeded"));
        let expired = cache.join("hermes_tts_0.wav");

        set_modified_to_epoch(&expired);
        enforce_managed_tts_cache_quota_with_limits(
            &cache,
            None,
            bytes.len() as u64,
            quota,
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(!expired.exists());
        write_validated_audio(&cache.join("hermes_tts_2.wav"), &bytes, "wav", "fixture")
            .await
            .unwrap();

        let managed_bytes = std::fs::read_dir(&cache)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                is_managed_tts_cache_name(name.to_str()?).then_some(entry.path())
            })
            .map(|path| std::fs::metadata(path).unwrap().len())
            .sum::<u64>();
        assert!(managed_bytes <= quota);
        assert_eq!(std::fs::read(explicit_output).unwrap(), bytes);
    }
    #[cfg(unix)]
    #[test]
    fn generated_workspace_rejects_nonsticky_group_writable_parent() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let destination = directory.path().join("speech.wav");
        let error = generated_audio_workspace_path(&destination, "wav").unwrap_err();

        assert!(error.to_string().contains("ancestry"));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn private_media_parent_rejects_replaceable_ancestor() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let shared = directory.path().join("shared");
        let protected_child = shared.join("protected-child");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::create_dir(&protected_child).unwrap();
        std::fs::set_permissions(&protected_child, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error =
            ensure_safe_private_media_parent(&protected_child.join("voice.wav")).unwrap_err();
        assert!(error.to_string().contains("ancestry"));
    }

    #[cfg(unix)]
    #[test]
    fn private_media_file_validator_rejects_non_owner_only_modes() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("private.wav");
        std::fs::write(&output, b"audio").unwrap();
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o600)).unwrap();
        validate_private_media_file(&output).unwrap();

        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(validate_private_media_file(&output).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_media_file_validator_rejects_extended_acl() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("private.wav");
        std::fs::write(&output, b"audio").unwrap();
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o600)).unwrap();
        let status = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone deny write", output.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());

        let result = validate_private_media_file(&output);
        let cleanup = std::process::Command::new("/bin/chmod")
            .args(["-N", output.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(cleanup.success());
        assert!(result.is_err());
    }
    #[cfg(windows)]
    #[test]
    fn windows_private_directory_creation_validates_its_owner_only_dacl() {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");

        windows_private_directory::create_exclusive(&private).unwrap();
        windows_private_directory::validate_existing(&private).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_directory_collision_never_rewrites_existing_acl() {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        windows_private_directory::create_exclusive(&private).unwrap();

        let error = windows_private_directory::create_exclusive(&private).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(183));
        windows_private_directory::validate_existing(&private).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_workspace_rejects_replaceable_parent() {
        let directory = tempfile::tempdir().unwrap();
        let replaceable_parent = directory.path().join("replaceable");
        let workspace = replaceable_parent.join("workspace");
        std::fs::create_dir(&replaceable_parent).unwrap();

        // A normal inherited temp-directory DACL is deliberately not accepted
        // as a containment root for a subprocess workspace.
        assert!(windows_private_directory::validate_existing(&replaceable_parent).is_err());
        let error = create_private_tts_workspace(&workspace).unwrap_err();
        assert!(error.to_string().contains("owner-only DACL"));
        assert!(!workspace.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_new_temporary_file_has_owner_only_security() {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        windows_private_directory::create_exclusive(&private).unwrap();
        let destination = private.join("destination.wav");
        let temporary = private.join(".destination.tmp.wav");
        let file = create_private_temp_file(&temporary, &destination, false).unwrap();
        drop(file);

        windows_private_directory::validate_owner_only_file(&temporary).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_directory_rejects_junctions() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let junction = directory.path().join("junction");
        std::fs::create_dir(&target).unwrap();
        let command = format!(
            "mklink /J \"{}\" \"{}\" >NUL",
            junction.display(),
            target.display()
        );
        let status = std::process::Command::new("cmd")
            .args(["/C", &command])
            .status()
            .unwrap();
        assert!(status.success());

        let error = windows_private_directory::validate_existing(&junction).unwrap_err();
        assert!(error.to_string().contains("non-reparse"));
    }
}
