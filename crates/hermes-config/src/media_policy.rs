//! Shared filesystem authorization for media delivery and external uploads.
//!
//! Delivery retains its compatibility-oriented policy. External uploads use the
//! same parsing, canonicalization, root collection, and deny-list machinery,
//! but never use delivery's permissive or recency-based fallbacks.

use std::ffi::OsStr;
#[cfg(unix)]
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::path::Component;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
#[cfg(windows)]
use std::time::UNIX_EPOCH;

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::{
    ffi::OsStrExt,
    fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
};
#[cfg(windows)]
use std::os::windows::{
    ffi::{OsStrExt, OsStringExt},
    fs::MetadataExt,
    io::{AsRawHandle, FromRawHandle},
};
#[cfg(windows)]
use std::sync::atomic::{AtomicU64, Ordering};

use crate::paths::hermes_home;

const MEDIA_DELIVERY_ALLOW_DIRS_ENV: &str = "HERMES_MEDIA_ALLOW_DIRS";
const MEDIA_DELIVERY_TRUST_RECENT_ENV: &str = "HERMES_MEDIA_TRUST_RECENT_FILES";
const MEDIA_DELIVERY_TRUST_RECENT_SECONDS_ENV: &str = "HERMES_MEDIA_TRUST_RECENT_SECONDS";
const MEDIA_DELIVERY_STRICT_ENV: &str = "HERMES_MEDIA_DELIVERY_STRICT";
const MEDIA_DELIVERY_TRUST_RECENT_DEFAULT_SECONDS: u64 = 600;
const MEDIA_DELIVERY_SNAPSHOT_MAX_BYTES: u64 = 128 * 1024 * 1024;
#[cfg(windows)]
static MEDIA_DELIVERY_STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(not(windows))]
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

#[cfg(windows)]
const WINDOWS_DENIED_HOME_SUBPATHS: &[&str] = &[
    r"AppData\Roaming\Microsoft\Credentials",
    r"AppData\Roaming\Microsoft\Protect",
    r"AppData\Roaming\Microsoft\Crypto",
    r"AppData\Roaming\Microsoft\Vault",
    r"AppData\Local\Microsoft\Credentials",
    r"AppData\Local\Microsoft\Protect",
    r"AppData\Local\Microsoft\Crypto",
    r"AppData\Local\Microsoft\Vault",
];

#[cfg(windows)]
const WINDOWS_APP_DATA_CREDENTIAL_SUBPATHS: &[&str] = &[
    r"Microsoft\Credentials",
    r"Microsoft\Protect",
    r"Microsoft\Crypto",
    r"Microsoft\Vault",
];

/// Return the configured media cache directory, preserving the gateway's
/// existing `HERMES_MEDIA_CACHE_DIR` fallback behavior.
pub fn default_media_cache_dir() -> String {
    std::env::var("HERMES_MEDIA_CACHE_DIR").unwrap_or_else(|_| {
        #[cfg(windows)]
        {
            std::env::temp_dir()
                .join("hermes-media-cache")
                .to_string_lossy()
                .into_owned()
        }
        #[cfg(not(windows))]
        {
            "/tmp/hermes-media-cache".to_string()
        }
    })
}

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
        let hermes_home = hermes_home();
        let hermes_root = home.as_ref().map(|home| home.join(".hermes"));

        let mut allowed_roots = vec![PathBuf::from(default_media_cache_dir())];
        allowed_roots.extend(hermes_media_cache_roots(&hermes_home));
        allowed_roots.extend(extra_media_allow_roots_from_env());

        Self {
            home,
            hermes_home,
            hermes_root,
            allowed_roots,
            denied_prefixes: default_media_delivery_denied_prefixes(),
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

fn default_media_delivery_denied_prefixes() -> Vec<PathBuf> {
    let mut denied = Vec::new();
    #[cfg(not(windows))]
    denied.extend(MEDIA_DELIVERY_DENIED_PREFIXES.iter().map(PathBuf::from));
    #[cfg(windows)]
    denied.extend(windows_system_denied_prefixes());
    denied
}

#[cfg(windows)]
fn windows_env_absolute_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)?;
    path.is_absolute().then_some(path)
}

#[cfg(windows)]
fn windows_system_drive_root() -> Option<PathBuf> {
    let drive = std::env::var_os("SystemDrive").filter(|value| !value.is_empty())?;
    let drive_text = drive.to_string_lossy();
    if drive_text.len() == 2 && drive_text.as_bytes()[1] == b':' {
        return Some(PathBuf::from(format!("{drive_text}\\")));
    }
    let path = PathBuf::from(drive);
    path.is_absolute().then_some(path)
}

#[cfg(windows)]
fn windows_system_denied_prefixes() -> Vec<PathBuf> {
    let mut denied = ["SystemRoot", "ProgramData"]
        .into_iter()
        .filter_map(windows_env_absolute_path)
        .collect::<Vec<_>>();
    let system_drive = windows_system_drive_root().unwrap_or_else(|| PathBuf::from(r"C:\"));
    denied.push(system_drive.join("Windows"));
    denied.push(system_drive.join("ProgramData"));
    denied
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
    let Some(raw) = std::env::var(MEDIA_DELIVERY_TRUST_RECENT_SECONDS_ENV).ok() else {
        return Duration::from_secs(MEDIA_DELIVERY_TRUST_RECENT_DEFAULT_SECONDS);
    };
    let Ok(seconds) = raw.trim().parse::<f64>() else {
        return Duration::ZERO;
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return Duration::ZERO;
    }
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::ZERO)
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

#[cfg(windows)]
fn windows_credential_roots(home: Option<&Path>) -> Vec<PathBuf> {
    let mut denied = Vec::new();
    if let Some(home) = home {
        denied.extend(
            WINDOWS_DENIED_HOME_SUBPATHS
                .iter()
                .map(|subpath| home.join(subpath)),
        );
    }
    for name in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(app_data) = windows_env_absolute_path(name) {
            denied.extend(
                WINDOWS_APP_DATA_CREDENTIAL_SUBPATHS
                    .iter()
                    .map(|subpath| app_data.join(subpath)),
            );
        }
    }
    denied
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
    #[cfg(windows)]
    denied.extend(windows_credential_roots(policy.home.as_deref()));
    for hermes_root in std::iter::once(&policy.hermes_home).chain(policy.hermes_root.iter()) {
        denied.extend(
            MEDIA_DELIVERY_DENIED_HERMES_FILES
                .iter()
                .map(|file| hermes_root.join(file)),
        );
    }
    denied
}

#[cfg(windows)]
fn strip_windows_extended_path_prefix(path: &str) -> String {
    let normalized = path.replace('/', "\\");
    let extended_unc_prefix = r"\\?\UNC\";
    if normalized
        .get(..extended_unc_prefix.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(extended_unc_prefix))
    {
        return format!(r"\\{}", &normalized[extended_unc_prefix.len()..]);
    }
    let extended_prefix = r"\\?\";
    if normalized
        .get(..extended_prefix.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(extended_prefix))
    {
        return normalized[extended_prefix.len()..].to_string();
    }
    normalized
}

#[cfg(windows)]
fn normalize_windows_path(path: &Path) -> PathBuf {
    PathBuf::from(strip_windows_extended_path_prefix(
        &path.as_os_str().to_string_lossy(),
    ))
}

#[cfg(windows)]
fn windows_path_component_eq(left: &std::ffi::OsStr, right: &std::ffi::OsStr) -> bool {
    left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
}

#[cfg(windows)]
fn path_is_same_or_under(path: &Path, prefix: &Path) -> bool {
    let path = normalize_windows_path(path);
    let prefix = normalize_windows_path(prefix);
    let mut path_components = path.components();
    for prefix_component in prefix.components() {
        let Some(path_component) = path_components.next() else {
            return false;
        };
        if !windows_path_component_eq(path_component.as_os_str(), prefix_component.as_os_str()) {
            return false;
        }
    }
    true
}

#[cfg(not(windows))]
fn path_is_same_or_under(path: &Path, prefix: &Path) -> bool {
    path == prefix || path.starts_with(prefix)
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    path_is_same_or_under(left, right) && path_is_same_or_under(right, left)
}

fn path_under_denied_prefix_with_home_exception(
    resolved: &Path,
    policy: &MediaDeliveryPolicy,
    permit_running_home: bool,
) -> bool {
    let home = policy
        .home
        .as_ref()
        .and_then(|home| canonicalize_or_absolute(home));

    for denied in media_delivery_denied_paths(policy) {
        let Some(resolved_denied) = canonicalize_or_absolute(&denied) else {
            continue;
        };
        if !path_is_same_or_under(resolved, &resolved_denied) {
            continue;
        }
        // Compatibility delivery allows a root-run gateway to use its home
        // tree, while still denying its credential subdirectories. External
        // uploads never receive that exception.
        if permit_running_home
            && home
                .as_deref()
                .map(|home| paths_equal(home, &resolved_denied))
                .unwrap_or(false)
        {
            continue;
        }
        return true;
    }

    false
}

fn path_under_denied_prefix(resolved: &Path, policy: &MediaDeliveryPolicy) -> bool {
    path_under_denied_prefix_with_home_exception(resolved, policy, true)
}

fn path_under_external_upload_denied_prefix(
    resolved: &Path,
    policy: &MediaDeliveryPolicy,
) -> bool {
    path_under_denied_prefix_with_home_exception(resolved, policy, false)
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

fn resolve_existing_media_file(path: &str, home: Option<&Path>) -> Option<PathBuf> {
    let candidate = strip_model_path_wrapping(path)?;
    let expanded = expand_home_path(&candidate, home)?;
    if !expanded.is_absolute() {
        return None;
    }

    let resolved = std::fs::canonicalize(&expanded).ok()?;
    resolved.is_file().then_some(resolved)
}

fn validate_media_delivery_path_with_policy(
    path: &str,
    policy: &MediaDeliveryPolicy,
) -> Option<PathBuf> {
    let resolved = resolve_existing_media_file(path, policy.home.as_deref())?;

    for root in &policy.allowed_roots {
        let Some(resolved_root) = canonicalize_or_absolute(root) else {
            continue;
        };
        if path_is_same_or_under(&resolved, &resolved_root) {
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

struct OpenedMediaDeliverySource {
    file: File,
    resolved_path: PathBuf,
}

enum MediaDeliveryStagingDir {
    #[cfg(unix)]
    Unix(tempfile::TempDir),
    #[cfg(windows)]
    Windows(PathBuf),
    #[cfg(not(any(unix, windows)))]
    Unsupported(PathBuf),
}

impl MediaDeliveryStagingDir {
    fn path(&self) -> &Path {
        match self {
            #[cfg(unix)]
            Self::Unix(staging_dir) => staging_dir.path(),
            #[cfg(windows)]
            Self::Windows(path) => path,
            #[cfg(not(any(unix, windows)))]
            Self::Unsupported(path) => path,
        }
    }
}

impl Drop for MediaDeliveryStagingDir {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            let Self::Windows(path) = self;
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// A private staged snapshot approved for native platform media delivery.
///
/// The source is opened without following links and copied before this guard is
/// returned. Keep the guard alive until the adapter's send future resolves;
/// dropping it removes the staged file and its private directory.
pub struct ValidatedMediaDelivery {
    staged_path: PathBuf,
    _staging_dir: MediaDeliveryStagingDir,
}

impl ValidatedMediaDelivery {
    /// The protected staged path to hand to a platform adapter.
    pub fn staged_path(&self) -> &Path {
        &self.staged_path
    }
}

impl AsRef<Path> for ValidatedMediaDelivery {
    fn as_ref(&self) -> &Path {
        self.staged_path()
    }
}

impl std::ops::Deref for ValidatedMediaDelivery {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.staged_path()
    }
}

/// Validate a media file that has already been opened through a no-follow,
/// descriptor-relative path walk.
///
/// The opened handle must name one regular file with exactly one hard link.
/// This prevents an allowed media pathname from aliasing a denied credential
/// or system inode through a hard link.
pub fn validate_media_file_handle(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    validate_media_file_metadata(file, &metadata)
}

fn validate_media_file_metadata(file: &File, metadata: &fs::Metadata) -> io::Result<()> {
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing non-regular media file",
        ));
    }

    #[cfg(unix)]
    let _ = file;

    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing hard-linked media file",
        ));
    }

    #[cfg(windows)]
    if windows_media_file_link_count(file)? != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing hard-linked media file",
        ));
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "media hard-link validation is unavailable on this platform",
        ));
    }

    Ok(())
}

#[cfg(windows)]
fn windows_media_file_link_count(file: &File) -> io::Result<u32> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(information.nNumberOfLinks)
}

#[cfg(unix)]
fn open_media_delivery_source_no_follow(path: &Path) -> io::Result<OpenedMediaDeliverySource> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "media source must be an absolute path",
        ));
    }

    let mut directory = File::open(Path::new("/"))?;
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "media source contains a non-canonical path component",
            ));
        };
        let component = CString::new(component.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "media source contains an embedded NUL",
            )
        })?;
        let last_component = components.peek().is_none();
        let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        if !last_component {
            flags |= libc::O_DIRECTORY;
        }
        let fd = unsafe { libc::openat(directory.as_raw_fd(), component.as_ptr(), flags, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let opened = unsafe { File::from_raw_fd(fd) };
        if last_component {
            return Ok(OpenedMediaDeliverySource {
                file: opened,
                resolved_path: path.to_path_buf(),
            });
        }
        directory = opened;
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "media source must name a regular file",
    ))
}

#[cfg(windows)]
fn windows_wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut path = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "media source contains an embedded NUL",
        ));
    }
    path.push(0);
    Ok(path)
}

#[cfg(windows)]
fn normalized_windows_handle_path(file: &File) -> io::Result<PathBuf> {
    use std::ffi::OsString;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

    let mut buffer = vec![0u16; 260];
    loop {
        let written = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        };
        if written == 0 {
            return Err(io::Error::last_os_error());
        }
        if (written as usize) < buffer.len() {
            let raw = OsString::from_wide(&buffer[..written as usize]);
            let raw = raw.to_string_lossy();
            if let Some(unc_tail) = raw.strip_prefix(r"\\?\UNC\") {
                return Ok(PathBuf::from(format!(r"\\{unc_tail}")));
            }
            if let Some(path_tail) = raw.strip_prefix(r"\\?\") {
                return Ok(PathBuf::from(path_tail));
            }
            return Ok(PathBuf::from(raw.as_ref()));
        }
        buffer.resize(written as usize + 1, 0);
    }
}

#[cfg(windows)]
fn open_media_delivery_source_no_follow(path: &Path) -> io::Result<OpenedMediaDeliverySource> {
    use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };

    let path = windows_wide_path(path)?;
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing media source reparse point",
        ));
    }
    let resolved_path = normalized_windows_handle_path(&file)?;
    Ok(OpenedMediaDeliverySource {
        file,
        resolved_path,
    })
}

#[cfg(not(any(unix, windows)))]
fn open_media_delivery_source_no_follow(
    _path: &Path,
) -> io::Result<OpenedMediaDeliverySource> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure media staging is unavailable on this platform",
    ))
}

#[cfg(windows)]
fn set_owner_only_windows_dacl(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
        SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER,
        TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    fn status_error(status: u32) -> io::Error {
        io::Error::from_raw_os_error(status as i32)
    }

    let path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut owner: PSID = ptr::null_mut();
    let mut security_descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let owner_status = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut security_descriptor,
        )
    };
    if owner_status != 0 {
        return Err(status_error(owner_status));
    }
    if owner.is_null() {
        unsafe {
            LocalFree(security_descriptor);
        }
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "media staging path has no owner SID",
        ));
    }

    let mut explicit_access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: owner.cast(),
        },
    };
    let mut owner_acl: *mut ACL = ptr::null_mut();
    let acl_status = unsafe { SetEntriesInAclW(1, &mut explicit_access, ptr::null(), &mut owner_acl) };
    if acl_status != 0 {
        unsafe {
            LocalFree(security_descriptor);
        }
        return Err(status_error(acl_status));
    }

    let set_status = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            owner_acl,
            ptr::null(),
        )
    };
    unsafe {
        LocalFree(owner_acl.cast());
        LocalFree(security_descriptor);
    }
    if set_status != 0 {
        return Err(status_error(set_status));
    }
    Ok(())
}

#[cfg(unix)]
fn create_media_delivery_staging_dir() -> io::Result<MediaDeliveryStagingDir> {
    let staging_dir = tempfile::Builder::new()
        .prefix("hermes-media-delivery-")
        .tempdir()?;
    fs::set_permissions(
        staging_dir.path(),
        fs::Permissions::from_mode(0o700),
    )?;
    Ok(MediaDeliveryStagingDir::Unix(staging_dir))
}

#[cfg(windows)]
fn create_owner_only_windows_security_descriptor() -> io::Result<
    windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };

    let sddl = "D:P(A;;FA;;;OW)"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut security_descriptor = std::ptr::null_mut();
    let result = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut security_descriptor,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(security_descriptor)
}

#[cfg(windows)]
fn create_media_delivery_staging_dir() -> io::Result<MediaDeliveryStagingDir> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

    let security_descriptor = create_owner_only_windows_security_descriptor()?;
    let mut security_attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security_descriptor.cast(),
        bInheritHandle: 0,
    };
    let temp_dir = std::env::temp_dir();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let result = (|| {
        for _ in 0..128 {
            let sequence = MEDIA_DELIVERY_STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = temp_dir.join(format!(
                "hermes-media-delivery-{:x}-{:x}-{:x}",
                std::process::id(),
                now,
                sequence,
            ));
            let path_wide = windows_wide_path(&path)?;
            let created = unsafe { CreateDirectoryW(path_wide.as_ptr(), &mut security_attributes) };
            if created != 0 {
                return Ok(MediaDeliveryStagingDir::Windows(path));
            }
            let error = io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(80 | 183)) {
                return Err(error);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a private media staging directory",
        ))
    })();
    unsafe {
        LocalFree(security_descriptor);
    }
    result
}

#[cfg(not(any(unix, windows)))]
fn create_media_delivery_staging_dir() -> io::Result<MediaDeliveryStagingDir> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure media staging is unavailable on this platform",
    ))
}

fn snapshot_media_delivery_path(
    source_path: &Path,
    policy: &MediaDeliveryPolicy,
) -> io::Result<ValidatedMediaDelivery> {
    snapshot_media_delivery_path_with_limit(
        source_path,
        policy,
        MEDIA_DELIVERY_SNAPSHOT_MAX_BYTES,
    )
}

fn snapshot_media_delivery_path_with_limit(
    source_path: &Path,
    policy: &MediaDeliveryPolicy,
    max_bytes: u64,
) -> io::Result<ValidatedMediaDelivery> {
    let OpenedMediaDeliverySource {
        file: source,
        resolved_path,
    } = open_media_delivery_source_no_follow(source_path)?;
    if validate_media_delivery_path_with_policy(&resolved_path.to_string_lossy(), policy).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "opened media source is no longer policy-approved",
        ));
    }

    let source_metadata = source.metadata()?;
    validate_media_file_metadata(&source, &source_metadata)?;
    if source_metadata.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "media source exceeds staging byte cap",
        ));
    }

    let staging_dir = create_media_delivery_staging_dir()?;
    let staged_path = staging_dir
        .path()
        .join(source_path.file_name().unwrap_or_else(|| OsStr::new("media")));
    let mut staged_options = OpenOptions::new();
    staged_options.write(true).create_new(true);
    #[cfg(unix)]
    staged_options.mode(0o600);
    let mut staged = staged_options.open(&staged_path)?;
    #[cfg(unix)]
    fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o600))?;
    #[cfg(windows)]
    set_owner_only_windows_dacl(&staged_path)?;

    let mut limited_source = source.take(max_bytes.saturating_add(1));
    let copied = io::copy(&mut limited_source, &mut staged)?;
    if copied > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "media source grew beyond staging byte cap",
        ));
    }
    staged.flush()?;
    drop(staged);

    Ok(ValidatedMediaDelivery {
        staged_path,
        _staging_dir: staging_dir,
    })
}

/// Return a private staged snapshot for native platform media delivery.
///
/// Default mode accepts existing regular files outside credential and system
/// deny paths. Strict mode (`HERMES_MEDIA_DELIVERY_STRICT=1`) requires either a
/// Hermes/operator allow root or a freshly-produced file inside the recency
/// window. The approved source is copied from one no-follow handle into a
/// private staging directory before any adapter can reopen it.
pub fn validate_media_delivery_path(path: &str) -> Option<ValidatedMediaDelivery> {
    let policy = MediaDeliveryPolicy::from_env();
    let resolved = validate_media_delivery_path_with_policy(path, &policy)?;
    snapshot_media_delivery_path(&resolved, &policy).ok()
}

/// Canonical file and capability root selected for an external media upload.
///
/// Consumers must open `canonical_path` relative to `canonical_root` without
/// following symlinks, then read that same handle. Reopening the original
/// caller path after this decision would reintroduce a validation/use race.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedExternalMediaUpload {
    pub canonical_path: PathBuf,
    pub canonical_root: PathBuf,
}

/// Select a canonical media file and containing capability root for upload.
///
/// This is intentionally stricter than [`validate_media_delivery_path`]: it
/// accepts only files under a configured media root or an explicit trusted
/// root. It never falls back to delivery's permissive mode or recency window.
/// Symlinks are resolved for policy selection; the consumer must still open
/// the returned path beneath `canonical_root` and read that same handle.
pub fn validate_external_media_upload_path(
    path: &str,
    extra_trusted_roots: &[PathBuf],
) -> Option<ValidatedExternalMediaUpload> {
    let policy = MediaDeliveryPolicy::from_env();
    let resolved = resolve_existing_media_file(path, policy.home.as_deref())?;

    // Check canonical deny paths first. A caller-provided or configured root
    // must never turn a system or credential path into an external upload.
    if path_under_external_upload_denied_prefix(&resolved, &policy) {
        return None;
    }

    for root in policy.allowed_roots.iter().chain(extra_trusted_roots) {
        let Ok(resolved_root) = std::fs::canonicalize(root) else {
            continue;
        };
        if resolved_root.is_dir() && path_is_same_or_under(&resolved, &resolved_root) {
            return Some(ValidatedExternalMediaUpload {
                canonical_path: resolved,
                canonical_root: resolved_root,
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::MutexGuard;

    use super::*;

    const EXTERNAL_UPLOAD_TEST_ENV: &[&str] = &[
        "HOME",
        "USERPROFILE",
        "HERMES_HOME",
        "HERMES_AGENT_ULTRA_HOME",
        "HERMES_MEDIA_CACHE_DIR",
        "HERMES_MEDIA_ALLOW_DIRS",
        "HERMES_MEDIA_TRUST_RECENT_FILES",
        "HERMES_MEDIA_TRUST_RECENT_SECONDS",
        "HERMES_MEDIA_DELIVERY_STRICT",
    ];

    #[cfg(windows)]
    const WINDOWS_DENY_TEST_ENV: &[&str] = &[
        "SystemRoot",
        "SystemDrive",
        "ProgramData",
        "APPDATA",
        "LOCALAPPDATA",
    ];

    struct ScopedEnvironment {
        _lock: MutexGuard<'static, ()>,
        original: Vec<(&'static str, Option<OsString>)>,
    }

    impl ScopedEnvironment {
        fn new(keys: &[&'static str]) -> Self {
            let lock = crate::managed_gateway::test_lock::lock();
            let original = keys
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect();
            for key in keys {
                std::env::remove_var(key);
            }
            Self {
                _lock: lock,
                original,
            }
        }

        fn set_path(&self, key: &str, value: &Path) {
            std::env::set_var(key, value);
        }

        fn set(&self, key: &str, value: &str) {
            std::env::set_var(key, value);
        }
    }

    impl Drop for ScopedEnvironment {
        fn drop(&mut self) {
            for (key, value) in &self.original {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn configure_external_upload_environment(
        home: &Path,
        hermes_home: &Path,
        cache_dir: &Path,
    ) -> ScopedEnvironment {
        let environment = ScopedEnvironment::new(EXTERNAL_UPLOAD_TEST_ENV);
        environment.set_path("HOME", home);
        environment.set_path("HERMES_HOME", hermes_home);
        environment.set_path("HERMES_MEDIA_CACHE_DIR", cache_dir);
        environment
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
        std::fs::write(&key, b"test fixture credential marker").unwrap();
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
        std::fs::write(&key, b"test fixture credential marker").unwrap();
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

    #[test]
    fn external_upload_accepts_configured_audio_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let cache_dir = tmp.path().join("media-cache");
        let audio = cache_dir.join("audio/clip.wav");
        std::fs::create_dir_all(audio.parent().unwrap()).unwrap();
        std::fs::write(&audio, b"audio fixture").unwrap();
        let _environment = configure_external_upload_environment(&home, &hermes_home, &cache_dir);

        assert_eq!(
            validate_external_media_upload_path(&audio.to_string_lossy(), &[])
                .map(|upload| upload.canonical_path),
            Some(std::fs::canonicalize(&audio).unwrap())
        );
    }

    #[test]
    fn external_upload_accepts_hermes_audio_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let cache_dir = tmp.path().join("media-cache");
        let audio = hermes_home.join("audio_cache/clip.wav");
        std::fs::create_dir_all(audio.parent().unwrap()).unwrap();
        std::fs::write(&audio, b"audio fixture").unwrap();
        let _environment = configure_external_upload_environment(&home, &hermes_home, &cache_dir);

        assert_eq!(
            validate_external_media_upload_path(&audio.to_string_lossy(), &[])
                .map(|upload| upload.canonical_path),
            Some(std::fs::canonicalize(&audio).unwrap())
        );
    }

    #[test]
    fn external_upload_accepts_configured_allow_root() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let cache_dir = tmp.path().join("media-cache");
        let allow_root = tmp.path().join("operator-media");
        let audio = allow_root.join("clip.wav");
        std::fs::create_dir_all(&allow_root).unwrap();
        std::fs::write(&audio, b"audio fixture").unwrap();
        let environment = configure_external_upload_environment(&home, &hermes_home, &cache_dir);
        environment.set_path("HERMES_MEDIA_ALLOW_DIRS", &allow_root);

        assert_eq!(
            validate_external_media_upload_path(&audio.to_string_lossy(), &[])
                .map(|upload| upload.canonical_path),
            Some(std::fs::canonicalize(&audio).unwrap())
        );
    }

    #[test]
    fn external_upload_rejects_recent_file_outside_every_root() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let cache_dir = tmp.path().join("media-cache");
        let arbitrary_file = tmp.path().join("cwd/recent.wav");
        std::fs::create_dir_all(arbitrary_file.parent().unwrap()).unwrap();
        std::fs::write(&arbitrary_file, b"recent audio fixture").unwrap();
        let environment = configure_external_upload_environment(&home, &hermes_home, &cache_dir);
        environment.set("HERMES_MEDIA_TRUST_RECENT_FILES", "true");
        environment.set("HERMES_MEDIA_TRUST_RECENT_SECONDS", "3600");

        assert!(
            validate_external_media_upload_path(&arbitrary_file.to_string_lossy(), &[]).is_none()
        );
    }

    #[test]
    fn external_upload_accepts_explicit_trusted_root() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let cache_dir = tmp.path().join("media-cache");
        let trusted_root = tmp.path().join("trusted-media");
        let audio = trusted_root.join("clip.wav");
        std::fs::create_dir_all(&trusted_root).unwrap();
        std::fs::write(&audio, b"audio fixture").unwrap();
        let _environment = configure_external_upload_environment(&home, &hermes_home, &cache_dir);
        let extra_trusted_roots = [trusted_root];

        assert_eq!(
            validate_external_media_upload_path(&audio.to_string_lossy(), &extra_trusted_roots)
                .map(|upload| upload.canonical_path),
            Some(std::fs::canonicalize(&audio).unwrap())
        );
    }

    #[cfg(unix)]
    #[test]
    fn external_upload_rejects_configured_root_symlink_to_credential_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let credential_dir = home.join(".ssh");
        let configured_root = tmp.path().join("configured-cache");
        let credential = credential_dir.join("id_rsa");
        std::fs::create_dir_all(&credential_dir).unwrap();
        std::fs::write(&credential, b"credential fixture").unwrap();
        std::os::unix::fs::symlink(&credential_dir, &configured_root).unwrap();
        let _environment =
            configure_external_upload_environment(&home, &hermes_home, &configured_root);
        let wrapped = format!("`{}.`", configured_root.join("id_rsa").display());

        assert!(validate_external_media_upload_path(&wrapped, &[]).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn external_upload_rejects_symlink_escape_from_allowed_root() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let cache_dir = tmp.path().join("media-cache");
        let escaped_file = tmp.path().join("outside/clip.wav");
        let link = cache_dir.join("audio/clip.wav");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::fs::create_dir_all(escaped_file.parent().unwrap()).unwrap();
        std::fs::write(&escaped_file, b"outside fixture").unwrap();
        std::os::unix::fs::symlink(&escaped_file, &link).unwrap();
        let _environment = configure_external_upload_environment(&home, &hermes_home, &cache_dir);

        assert!(validate_external_media_upload_path(&link.to_string_lossy(), &[]).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn external_upload_rejects_system_path_before_trusted_root() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let cache_dir = tmp.path().join("media-cache");
        let _environment = configure_external_upload_environment(&home, &hermes_home, &cache_dir);
        let system_file = PathBuf::from("/etc/hosts");
        let extra_trusted_roots = [PathBuf::from("/")];

        assert!(system_file.is_file());
        assert!(validate_external_media_upload_path(
            &system_file.to_string_lossy(),
            &extra_trusted_roots
        )
        .is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn external_upload_keeps_root_home_system_denial_without_delivery_exception() {
        let root_home = PathBuf::from("/root");
        let policy = MediaDeliveryPolicy {
            home: Some(root_home.clone()),
            hermes_home: root_home.join(".hermes-agent-ultra"),
            hermes_root: Some(root_home.join(".hermes")),
            allowed_roots: vec![root_home.clone()],
            denied_prefixes: default_media_delivery_denied_prefixes(),
            strict_mode: false,
            recent_window: Duration::ZERO,
        };
        let root_file = root_home.join("deliverable.wav");

        assert!(!path_under_denied_prefix(&root_file, &policy));
        assert!(path_under_external_upload_denied_prefix(&root_file, &policy));
    }
    #[test]
    fn media_delivery_recency_window_rejects_invalid_or_overflowing_seconds() {
        let environment = ScopedEnvironment::new(EXTERNAL_UPLOAD_TEST_ENV);
        environment.set("HERMES_MEDIA_TRUST_RECENT_FILES", "true");

        for value in ["1e300", "NaN", "-1", "inf", "Infinity"] {
            environment.set("HERMES_MEDIA_TRUST_RECENT_SECONDS", value);
            assert_eq!(media_delivery_recency_window_from_env(), Duration::ZERO);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_media_cache_path_is_absolute() {
        let _environment = ScopedEnvironment::new(EXTERNAL_UPLOAD_TEST_ENV);

        assert!(Path::new(&default_media_cache_dir()).is_absolute());
    }

    #[cfg(windows)]
    #[test]
    fn windows_media_policy_blocks_casefolded_credential_and_system_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let safe_file = home.join("work/proposal.wav");
        let ssh_file = home.join(".ssh/id_rsa");
        let system_root = tmp.path().join("SystemRoot");
        let system_file = system_root.join("System32/config/SAM");
        let system_drive = tmp.path().join("SystemDrive");
        let drive_system_file = system_drive.join("Windows/System32/config/SYSTEM");
        let program_data = tmp.path().join("ProgramData");
        let program_data_file = program_data.join("credentials/token");
        let app_data = home.join("AppData/Roaming");
        let credential_file = app_data.join("Microsoft/Credentials/credential");

        for path in [
            &safe_file,
            &ssh_file,
            &system_file,
            &drive_system_file,
            &program_data_file,
            &credential_file,
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
        }

        let environment = ScopedEnvironment::new(WINDOWS_DENY_TEST_ENV);
        environment.set_path("SystemRoot", &system_root);
        environment.set_path("SystemDrive", &system_drive);
        environment.set_path("ProgramData", &program_data);
        environment.set_path("APPDATA", &app_data);
        environment.set_path("LOCALAPPDATA", &home.join("AppData/Local"));

        let mut denied_prefixes = default_media_delivery_denied_prefixes();
        denied_prefixes.push(home.clone());
        let policy = test_media_policy(&home, &hermes_home, denied_prefixes);
        let mixed_case_ssh = home.join(".SSH/id_rsa");

        assert_eq!(
            validate_media_delivery_path_with_policy(&safe_file.to_string_lossy(), &policy),
            Some(std::fs::canonicalize(&safe_file).unwrap())
        );
        assert!(path_under_denied_prefix(&mixed_case_ssh, &policy));
        for blocked in [
            &mixed_case_ssh,
            &system_file,
            &drive_system_file,
            &program_data_file,
            &credential_file,
        ] {
            assert!(
                validate_media_delivery_path_with_policy(&blocked.to_string_lossy(), &policy)
                    .is_none()
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_containment_normalizes_case_and_extended_prefixes() {
        assert!(path_is_same_or_under(
            Path::new(r"\\?\C:\Media\Cache\clip.wav"),
            Path::new(r"c:\media\cache"),
        ));
        assert!(!path_is_same_or_under(
            Path::new(r"C:\Media\Cache-other\clip.wav"),
            Path::new(r"c:\media\cache"),
        ));
    }
    #[cfg(unix)]
    #[test]
    fn media_delivery_snapshot_survives_source_symlink_swap_and_cleans_up() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let cache_dir = tmp.path().join("media-cache");
        let source = cache_dir.join("audio/clip.wav");
        let displaced_source = cache_dir.join("audio/clip-original.wav");
        let secret = home.join(".ssh/id_rsa");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(secret.parent().unwrap()).unwrap();
        std::fs::write(&source, b"original audio bytes").unwrap();
        std::fs::write(&secret, b"credential bytes must not be delivered").unwrap();
        let _environment = configure_external_upload_environment(&home, &hermes_home, &cache_dir);

        let delivery = validate_media_delivery_path(&source.to_string_lossy()).unwrap();
        let staged_path = delivery.staged_path().to_path_buf();
        assert_eq!(
            staged_path.file_name(),
            source.file_name(),
            "staging preserves the adapter-visible filename"
        );
        assert_eq!(
            std::fs::metadata(staged_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&staged_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        std::fs::rename(&source, &displaced_source).unwrap();
        symlink(&secret, &source).unwrap();
        assert_eq!(std::fs::read(&staged_path).unwrap(), b"original audio bytes");

        drop(delivery);
        assert!(!staged_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn media_delivery_snapshot_rejects_ancestor_symlink_swap_after_approval() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let allowed_root = tmp.path().join("allowed");
        let source_parent = allowed_root.join("audio");
        let source = source_parent.join("clip.wav");
        let displaced_parent = tmp.path().join("displaced-audio");
        let secret_parent = tmp.path().join("secret");
        let secret = secret_parent.join("clip.wav");
        std::fs::create_dir_all(&source_parent).unwrap();
        std::fs::create_dir_all(&secret_parent).unwrap();
        std::fs::write(&source, b"approved media").unwrap();
        std::fs::write(&secret, b"credential bytes must not be delivered").unwrap();

        let mut policy = test_media_policy(tmp.path(), tmp.path(), Vec::new());
        policy.allowed_roots.push(allowed_root);
        let approved =
            validate_media_delivery_path_with_policy(&source.to_string_lossy(), &policy).unwrap();

        std::fs::rename(&source_parent, &displaced_parent).unwrap();
        symlink(&secret_parent, &source_parent).unwrap();

        assert!(snapshot_media_delivery_path(&approved, &policy).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn media_delivery_rejects_hard_linked_credential_inode_in_allowed_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let cache_dir = tmp.path().join("media-cache");
        let credential = home.join(".ssh/id_rsa");
        let staged_name = cache_dir.join("audio/clip.wav");
        std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
        std::fs::create_dir_all(staged_name.parent().unwrap()).unwrap();
        std::fs::write(&credential, b"credential bytes must not be delivered").unwrap();
        std::fs::hard_link(&credential, &staged_name).unwrap();
        let _environment = configure_external_upload_environment(&home, &hermes_home, &cache_dir);

        let source = File::open(&staged_name).unwrap();
        assert_eq!(
            validate_media_file_handle(&source).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(validate_media_delivery_path(&staged_name.to_string_lossy()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn media_file_handle_accepts_single_link_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("clip.wav");
        std::fs::write(&source, b"single-link audio").unwrap();

        assert!(validate_media_file_handle(&File::open(source).unwrap()).is_ok());
    }

    #[test]
    fn media_delivery_snapshot_cleans_up_after_success_and_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let hermes_home = home.join(".hermes-agent-ultra");
        let cache_dir = tmp.path().join("media-cache");
        let source = cache_dir.join("audio/clip.wav");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"snapshot bytes").unwrap();
        let _environment = configure_external_upload_environment(&home, &hermes_home, &cache_dir);

        for should_fail in [false, true] {
            let delivery = validate_media_delivery_path(&source.to_string_lossy()).unwrap();
            let staged_path = delivery.staged_path().to_path_buf();
            let adapter_result: Result<(), ()> = (|| {
                assert_eq!(
                    std::fs::read(delivery.staged_path()).unwrap(),
                    b"snapshot bytes"
                );
                should_fail.then_some(()).map_or(Ok(()), |_| Err(()))
            })();

            drop(delivery);
            assert_eq!(adapter_result.is_err(), should_fail);
            assert!(!staged_path.exists());
        }
    }
    #[test]
    fn media_delivery_snapshot_rejects_files_over_copy_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("clip.wav");
        std::fs::write(&source, b"five!").unwrap();

        let mut policy = test_media_policy(tmp.path(), tmp.path(), Vec::new());
        policy.allowed_roots.push(tmp.path().to_path_buf());
        assert!(snapshot_media_delivery_path_with_limit(&source, &policy, 4).is_err());
    }
}
