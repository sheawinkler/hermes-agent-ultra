struct TemporaryArtifact {
    path: Option<PathBuf>,
}

impl TemporaryArtifact {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CasCrashPoint {
    AfterTempFsync,
    AfterDigestVerification,
    AfterAtomicRename,
}

#[cfg(test)]
fn maybe_inject_cas_crash(
    crash_at: Option<CasCrashPoint>,
    current: CasCrashPoint,
    cleanup: &mut TemporaryArtifact,
) -> Result<(), SealedArtifactError> {
    if crash_at == Some(current) {
        // Model process death: pre-rename temp files remain for startup
        // recovery; post-rename the old temporary pathname no longer exists.
        cleanup.disarm();
        return Err(SealedArtifactError::InjectedFailure(match current {
            CasCrashPoint::AfterTempFsync => "after_temp_fsync",
            CasCrashPoint::AfterDigestVerification => "after_digest_verification",
            CasCrashPoint::AfterAtomicRename => "after_atomic_rename",
        }));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum StoreLockMode {
    Shared,
    Exclusive,
}

struct AdvisoryFileLock {
    _file: File,
}
struct SystemCredentialProvisionGuard {
    _file: File,
}

impl ArtifactCredentialProvisionGuard for SystemCredentialProvisionGuard {}

struct FilesystemArtifactPublicationGuard {
    _store_lock: AdvisoryFileLock,
}

impl ArtifactPublicationGuard for FilesystemArtifactPublicationGuard {}

#[cfg(unix)]
fn acquire_store_lock(
    root: &Path,
    mode: StoreLockMode,
) -> Result<AdvisoryFileLock, SealedArtifactError> {
    acquire_private_artifact_lock(root, STORE_LOCK_FILE, mode)
}

#[cfg(unix)]
fn acquire_capacity_reservation_lock(root: &Path) -> Result<AdvisoryFileLock, SealedArtifactError> {
    acquire_private_artifact_lock(
        root,
        CAPACITY_RESERVATION_LOCK_FILE,
        StoreLockMode::Exclusive,
    )
}

#[cfg(unix)]
fn acquire_private_artifact_lock(
    root: &Path,
    lock_filename: &str,
    mode: StoreLockMode,
) -> Result<AdvisoryFileLock, SealedArtifactError> {
    use std::os::fd::AsRawFd;

    let path = root.join(lock_filename);
    let (file, created) = match create_private_file(&path) {
        Ok(file) => (file, true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            (open_private_file(&path)?, false)
        }
        Err(error) => return Err(SealedArtifactError::Io(error)),
    };
    if created {
        file.sync_all()?;
        sync_directory(root)?;
    }
    let operation = match mode {
        StoreLockMode::Shared => libc::LOCK_SH,
        StoreLockMode::Exclusive => libc::LOCK_EX,
    };
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(SealedArtifactError::Io(error));
        }
    }
    Ok(AdvisoryFileLock { _file: file })
}

#[cfg(not(unix))]
fn acquire_store_lock(
    _root: &Path,
    _mode: StoreLockMode,
) -> Result<AdvisoryFileLock, SealedArtifactError> {
    unsupported_artifact_locking()
}

#[cfg(not(unix))]
fn acquire_capacity_reservation_lock(
    _root: &Path,
) -> Result<AdvisoryFileLock, SealedArtifactError> {
    unsupported_artifact_locking()
}

#[cfg(not(unix))]
fn unsupported_artifact_locking() -> Result<AdvisoryFileLock, SealedArtifactError> {
    Err(SealedArtifactError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "artifact store locking is unsupported on this platform",
    )))
}

fn acquire_system_credential_provision_lock() -> Result<File, SealedArtifactError> {
    let data_root = dirs::data_local_dir().ok_or_else(|| {
        SealedArtifactError::KeyUnavailable(
            "OS user data directory is unavailable for credential provisioning lock".to_string(),
        )
    })?;
    let lock_root = data_root
        .join("hermes-agent")
        .join("credential-provision-locks");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&lock_root)?;
    #[cfg(unix)]
    validate_private_store_root(&lock_root)?;
    #[cfg(not(unix))]
    {
        let metadata = fs::symlink_metadata(&lock_root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SealedArtifactError::KeyUnavailable(
                "credential provisioning lock root must be a non-link directory".to_string(),
            ));
        }
    }

    let path = lock_root.join(".credential-provision.lock");
    #[cfg(unix)]
    let file = match create_private_file(&path) {
        Ok(file) => {
            file.sync_all()?;
            sync_directory(&lock_root)?;
            file
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open_private_file(&path)?,
        Err(error) => return Err(SealedArtifactError::Io(error)),
    };
    #[cfg(not(unix))]
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)?;
    fs2::FileExt::lock_exclusive(&file)?;
    Ok(file)
}

fn cleanup_stale_temporary_artifacts(root: &Path) -> Result<(), SealedArtifactError> {
    let mut removed = false;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == STORE_LOCK_FILE
            || name == CAPACITY_RESERVATION_LOCK_FILE
            || name == STORE_ID_FILE
            || is_key_provision_marker_filename(name)?
            || !name.starts_with(".artifact-")
        {
            continue;
        }
        let Some(uuid) = name
            .strip_prefix(".artifact-")
            .and_then(|value| value.strip_suffix(".tmp"))
        else {
            return Err(SealedArtifactError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed artifact temporary entry",
            )));
        };
        uuid::Uuid::parse_str(uuid).map_err(|_| {
            SealedArtifactError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed artifact temporary UUID",
            ))
        })?;
        let path = entry.path();
        drop(open_private_file(&path)?);
        fs::remove_file(path)?;
        removed = true;
    }
    if removed {
        sync_directory(root)?;
    }
    Ok(())
}

fn load_or_create_store_id(root: &Path) -> Result<String, SealedArtifactError> {
    let destination = root.join(STORE_ID_FILE);
    match read_store_id(&destination) {
        Ok(store_id) => return Ok(store_id),
        Err(SealedArtifactError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let store_id = uuid::Uuid::new_v4().simple().to_string();
    let temporary = root.join(format!(".artifact-{}.tmp", uuid::Uuid::new_v4()));
    let mut cleanup = TemporaryArtifact::new(temporary.clone());
    let mut file = create_private_file(&temporary)?;
    file.write_all(store_id.as_bytes())?;
    file.sync_all()?;
    drop(file);
    match atomic_rename_noreplace(&temporary, &destination) {
        Ok(()) => {
            cleanup.disarm();
            sync_directory(root)?;
            let persisted = read_store_id(&destination)?;
            if persisted != store_id {
                return Err(SealedArtifactError::InvalidMetadata(
                    "persisted artifact store identity changed during creation".to_string(),
                ));
            }
            Ok(persisted)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => read_store_id(&destination),
        Err(error) => Err(SealedArtifactError::Io(error)),
    }
}

fn read_store_id(path: &Path) -> Result<String, SealedArtifactError> {
    let mut file = open_private_file(path)?;
    if file.metadata()?.len() != 32 {
        return Err(SealedArtifactError::InvalidMetadata(
            "artifact store identity must contain exactly 32 bytes".to_string(),
        ));
    }
    let mut store_id = String::new();
    file.read_to_string(&mut store_id)?;
    if store_id.len() != 32
        || !store_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SealedArtifactError::InvalidMetadata(
            "artifact store identity must be 32 lowercase hexadecimal characters".to_string(),
        ));
    }
    Ok(store_id)
}

fn digest_open_private_file(file: &mut File) -> Result<String, SealedArtifactError> {
    let length = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
    if length > MAX_ARTIFACT_BYTES {
        return Err(SealedArtifactError::TooLarge {
            actual: length,
            limit: MAX_ARTIFACT_BYTES,
        });
    }
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn atomic_rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in source path"))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in destination path"))?;
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn atomic_rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in source path"))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in destination path"))?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "android"
    ))
))]
fn atomic_rename_noreplace(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace rename is unsupported on this Unix platform",
    ))
}

#[cfg(not(unix))]
fn atomic_rename_noreplace(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace rename is unsupported on this platform",
    ))
}

impl Drop for TemporaryArtifact {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn verify_existing_cas_file(path: &Path, expected_digest: &str) -> Result<(), SealedArtifactError> {
    let mut file = open_private_file(path)?;
    let length = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
    if length > MAX_ARTIFACT_BYTES {
        return Err(SealedArtifactError::TooLarge {
            actual: length,
            limit: MAX_ARTIFACT_BYTES,
        });
    }
    let mut bytes = Vec::with_capacity(length);
    file.read_to_end(&mut bytes)?;
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != expected_digest {
        return Err(SealedArtifactError::CasConflict(
            expected_digest.to_string(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_store_root(path: &Path) -> io::Result<fs::Metadata> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact store root must be a non-link directory",
        ));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact store root is not owned by the current user",
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "artifact store root must deny group and other access",
        ));
    }
    Ok(metadata)
}

#[cfg(windows)]
fn validate_private_store_root(_path: &Path) -> io::Result<fs::Metadata> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "sealed artifact storage requires owner-only Windows DACL support",
    ))
}

#[cfg(not(any(unix, windows)))]
fn validate_private_store_root(_path: &Path) -> io::Result<fs::Metadata> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "sealed artifact storage is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "sealed artifact file creation is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn open_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o177 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "sealed artifact must be an owner-only, single-link regular file",
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_private_file(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "sealed artifact file reads are unsupported on this platform",
    ))
}

#[cfg(unix)]
fn unix_device(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.dev()
}

#[cfg(unix)]
fn unix_inode(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory fsync is unsupported on this platform",
    ))
}

