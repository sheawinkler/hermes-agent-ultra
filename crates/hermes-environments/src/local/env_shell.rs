#[cfg(unix)]
fn configure_bounded_process_tree(cmd: &mut TokioCommand) {
    use std::os::unix::process::CommandExt;

    // Every managed local command owns a fresh process group.  That is
    // containment for lifecycle purposes only, not an isolation boundary.
    cmd.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
fn configure_bounded_process_tree(_cmd: &mut TokioCommand) {}

#[cfg(windows)]
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtResumeProcess(process_handle: windows_sys::Win32::Foundation::HANDLE) -> i32;
}

/// Kill-on-close Job Object used when Unix process groups are unavailable.
///
/// The command starts suspended, is assigned before user code runs, and is
/// resumed only after assignment. That closes the child-spawn race that a
/// post-spawn `taskkill` approach would leave open.
#[cfg(windows)]
struct OwnedProcessJob {
    handle: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl OwnedProcessJob {
    fn assign(child: &tokio::process::Child) -> Result<Self, std::io::Error> {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(error);
        }
        let child_handle = child.raw_handle().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "spawned Windows child has no process handle",
            )
        })?;
        if unsafe { AssignProcessToJobObject(handle, child_handle) } == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(error);
        }
        // SAFETY: `CreateJobObjectW` returned this live, uniquely owned handle,
        // and all error paths above close it before returning.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        Ok(Self { handle })
    }

    fn terminate(&self) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        unsafe {
            TerminateJobObject(self.handle.as_raw_handle(), 1);
        }
    }
}


#[cfg(windows)]
fn configure_windows_bounded_process_tree(cmd: &mut TokioCommand) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

    cmd.as_std_mut()
        .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
}

#[cfg(windows)]
fn resume_windows_bounded_process_tree(child: &tokio::process::Child) -> Result<(), std::io::Error> {
    let handle = child.raw_handle().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "spawned Windows child has no process handle",
        )
    })?;
    let status = unsafe { NtResumeProcess(handle) };
    if status < 0 {
        return Err(std::io::Error::other(format!(
            "NtResumeProcess failed with NTSTATUS 0x{status:08x}"
        )));
    }
    Ok(())
}

const PROCESS_TREE_TERM_GRACE: std::time::Duration = std::time::Duration::from_millis(150);
const PROCESS_TREE_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const PROCESS_TREE_WAIT_POLL: std::time::Duration = std::time::Duration::from_millis(5);

/// Owns one direct child and its process tree for the complete command
/// lifetime. Unix process groups and Windows Job Objects provide a lifecycle
/// kill boundary; neither provides filesystem, network, or credential
/// isolation.
struct BoundedProcessTree {
    child: Option<tokio::process::Child>,
    pid: Option<u32>,
    #[cfg(windows)]
    job: Option<OwnedProcessJob>,
}

impl BoundedProcessTree {
    fn spawn(mut command: TokioCommand, spawn_label: &str) -> Result<Self, AgentError> {
        #[cfg(all(not(unix), not(windows)))]
        {
            let _ = command;
            return Err(AgentError::Io(format!(
                "{spawn_label} execution is unsupported on platforms without process-tree ownership"
            )));
        }

        configure_bounded_process_tree(&mut command);
        #[cfg(windows)]
        configure_windows_bounded_process_tree(&mut command);
        let child = command
            .spawn()
            .map_err(|error| AgentError::Io(format!("Failed to spawn {spawn_label}: {error}")))?;
        #[cfg(windows)]
        let mut child = child;
        #[cfg(windows)]
        let job = match OwnedProcessJob::assign(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.start_kill();
                return Err(AgentError::Io(format!(
                    "Failed to assign {spawn_label} to Windows Job Object: {error}"
                )));
            }
        };
        #[cfg(windows)]
        if let Err(error) = resume_windows_bounded_process_tree(&child) {
            job.terminate();
            let _ = child.start_kill();
            return Err(AgentError::Io(format!(
                "Failed to resume {spawn_label} after Job Object assignment: {error}"
            )));
        }
        let pid = child.id();
        Ok(Self {
            child: Some(child),
            pid,
            #[cfg(windows)]
            job: Some(job),
        })
    }

    fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn child_mut(&mut self) -> &mut tokio::process::Child {
        self.child.as_mut().expect("bounded process child present")
    }

    fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.child_mut().stdout.take()
    }

    fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.child_mut().stderr.take()
    }

    fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.child_mut().stdin.take()
    }

    #[cfg(unix)]
    fn direct_child_exited_without_reap(pid: u32) -> Result<bool, std::io::Error> {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { info.assume_init().si_pid() } == pid as libc::pid_t)
    }

    #[cfg(unix)]
    fn signal_process_group(pid: u32, signal: libc::c_int) {
        unsafe {
            libc::kill(-(pid as libc::pid_t), signal);
        }
    }

    /// Poll the direct child without ever reaping it before its descendants are
    /// terminated.  A shell may exit while a background child retains stdio;
    /// signaling the group first prevents that descendant from outliving us.
    fn poll_exit(&mut self) -> Result<Option<std::process::ExitStatus>, std::io::Error> {
        #[cfg(unix)]
        {
            if let Some(pid) = self.pid {
                if Self::direct_child_exited_without_reap(pid)? {
                    Self::signal_process_group(pid, libc::SIGKILL);
                    let status = self.child_mut().try_wait()?;
                    if status.is_some() {
                        self.child.take();
                        self.pid.take();
                    }
                    return Ok(status);
                }
                return Ok(None);
            }
            return Ok(None);
        }
        #[cfg(windows)]
        {
            let status = self.child_mut().try_wait()?;
            if status.is_some() {
                // Closing the job kills any descendant that survived its
                // leader; do this before releasing the leader handle.
                self.job.take();
                self.child.take();
                self.pid.take();
            }
            return Ok(status);
        }

        #[cfg(all(not(unix), not(windows)))]
        {
            Ok(None)
        }
    }

    async fn wait_until_exit(&mut self) -> Result<std::process::ExitStatus, std::io::Error> {
        loop {
            if let Some(status) = self.poll_exit()? {
                return Ok(status);
            }
            tokio::time::sleep(PROCESS_TREE_WAIT_POLL).await;
        }
    }

    /// Synchronically terminate and reap the direct child. This is usable
    /// from Drop, where awaiting is impossible, and terminates the owned
    /// process tree before releasing its leader handle.
    fn terminate_and_join_sync(&mut self) {
        #[cfg(unix)]
        {
            let Some(pid) = self.pid.take() else {
                self.child.take();
                return;
            };
            Self::signal_process_group(pid, libc::SIGTERM);
            std::thread::sleep(PROCESS_TREE_TERM_GRACE);
            Self::signal_process_group(pid, libc::SIGKILL);

            let deadline = std::time::Instant::now() + PROCESS_TREE_REAP_TIMEOUT;
            loop {
                let mut status = 0;
                let waited = unsafe {
                    libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG)
                };
                if waited == pid as libc::pid_t
                    || waited == -1
                    || std::time::Instant::now() >= deadline
                {
                    break;
                }
                std::thread::sleep(PROCESS_TREE_WAIT_POLL);
            }
            self.child.take();
        }

        #[cfg(windows)]
        {
            if let Some(job) = self.job.take() {
                job.terminate();
            }
            if let Some(mut child) = self.child.take() {
                let _ = child.start_kill();
                let deadline = std::time::Instant::now() + PROCESS_TREE_REAP_TIMEOUT;
                while std::time::Instant::now() < deadline {
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) => std::thread::sleep(PROCESS_TREE_WAIT_POLL),
                    }
                }
            }
            self.pid.take();
        }

        #[cfg(all(not(unix), not(windows)))]
        {
            self.child.take();
            self.pid.take();
        }
    }

    async fn terminate_and_join(&mut self) {
        self.terminate_and_join_sync();
    }

    fn disarm(&mut self) {
        #[cfg(windows)]
        self.job.take();
        self.child.take();
        self.pid.take();
    }
}

impl Drop for BoundedProcessTree {
    fn drop(&mut self) {
        self.terminate_and_join_sync();
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn hermes_home_dir() -> Option<PathBuf> {
    std::env::var_os("HERMES_HOME")
        .or_else(|| std::env::var_os("HERMES_AGENT_ULTRA_HOME"))
        .map(PathBuf::from)
}

fn current_username() -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("LOGNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("USERNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
}

fn is_valid_unix_username(username: &str) -> bool {
    !username.is_empty()
        && username
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

#[cfg(unix)]
fn passwd_home_for_username(username: &str) -> Option<PathBuf> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split(':');
        let user = parts.next()?;
        let _passwd = parts.next()?;
        let _uid = parts.next()?;
        let _gid = parts.next()?;
        let _gecos = parts.next()?;
        let home = parts.next()?;
        if user == username && !home.is_empty() {
            return Some(PathBuf::from(home));
        }
    }
    None
}

#[cfg(unix)]
fn lookup_home_for_username(username: &str) -> Option<PathBuf> {
    if current_username().as_deref() == Some(username) {
        return home_dir();
    }
    passwd_home_for_username(username)
}

#[cfg(not(unix))]
fn lookup_home_for_username(username: &str) -> Option<PathBuf> {
    if current_username().as_deref() == Some(username) {
        return home_dir();
    }
    None
}

fn real_home_dir() -> Option<PathBuf> {
    std::env::var_os("HERMES_REAL_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            #[cfg(unix)]
            {
                current_username()
                    .as_deref()
                    .and_then(passwd_home_for_username)
            }
            #[cfg(not(unix))]
            {
                None
            }
        })
        .or_else(home_dir)
}

fn ensure_profile_home_dir() -> Result<PathBuf, AgentError> {
    let hermes_home = hermes_home_dir().ok_or_else(|| {
        AgentError::Config(
            "TERMINAL_HOME_MODE=profile requires HERMES_HOME or HERMES_AGENT_ULTRA_HOME".into(),
        )
    })?;
    let home = hermes_home.join("home");
    std::fs::create_dir_all(&home).map_err(|error| {
        AgentError::Io(format!(
            "Failed to create isolated terminal profile HOME '{}': {error}",
            home.display()
        ))
    })?;
    Ok(home)
}

fn subprocess_home_for_mode(mode: TerminalHomeMode) -> Result<Option<PathBuf>, AgentError> {
    match mode {
        TerminalHomeMode::Auto | TerminalHomeMode::Real => Ok(real_home_dir()),
        // Profile (also called "isolated" in TERMINAL_HOME_MODE) is
        // fail-closed: it never falls back to the operator's real HOME.
        TerminalHomeMode::Profile => ensure_profile_home_dir().map(Some),
    }
}

fn resolve_path(input: &str) -> Result<PathBuf, AgentError> {
    if !input.starts_with('~') {
        let path = PathBuf::from(input);
        if path.is_absolute() {
            return Ok(path);
        }
        if let Some(cwd) = std::env::var_os("TERMINAL_CWD") {
            if !cwd.is_empty() {
                return Ok(PathBuf::from(cwd).join(path));
            }
        }
        return Ok(path);
    }

    let rest = &input[1..];
    if rest.is_empty() {
        return home_dir().ok_or_else(|| AgentError::Io("Failed to resolve home dir".into()));
    }

    if rest.starts_with('/') {
        let home = home_dir().ok_or_else(|| AgentError::Io("Failed to resolve home dir".into()))?;
        let suffix = rest.trim_start_matches('/');
        return Ok(if suffix.is_empty() {
            home
        } else {
            home.join(suffix)
        });
    }

    // ~username or ~username/path. For security, only allow traditional
    // username characters so malicious payloads cannot pass through.
    let (username, suffix) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, ""),
    };

    if !is_valid_unix_username(username) {
        return Ok(PathBuf::from(input));
    }

    if let Some(home) = lookup_home_for_username(username) {
        return Ok(if suffix.is_empty() {
            home
        } else {
            home.join(suffix)
        });
    }

    Ok(PathBuf::from(input))
}

use hermes_core::subprocess_env::{
    hermes_subprocess_env, should_strip_subprocess_env_key, SUBPROCESS_ENV_BLOCKLIST_EXACT,
    SUBPROCESS_ENV_BLOCKLIST_PREFIXES, SUBPROCESS_ENV_PASSTHROUGH_VAR,
};

#[cfg(test)]
const SANE_PATH_ENTRIES: &[&str] = &[
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
];

fn should_strip_subprocess_env(key: &str) -> bool {
    should_strip_subprocess_env_key(key, false)
}

fn normalize_env_passthrough_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_subprocess_env_passthrough(value: &str) -> BTreeSet<String> {
    value
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ':' | ';'))
        .filter_map(normalize_env_passthrough_name)
        .collect()
}

fn subprocess_env_passthrough_set(configured: &[String]) -> BTreeSet<String> {
    // Configuration may opt in ordinary operator variables, but it cannot
    // reopen the credential blocklist or inject dispatcher control variables.
    configured
        .iter()
        .filter_map(|name| normalize_env_passthrough_name(name))
        .filter(|name| !should_strip_subprocess_env(name))
        .collect()
}

#[cfg(test)]
fn normalize_subprocess_path(path: Option<&str>) -> String {
    let Some(path) = path.filter(|value| !value.trim().is_empty()) else {
        return SANE_PATH_ENTRIES.join(":");
    };
    if std::env::split_paths(path).any(|entry| entry == std::path::Path::new("/usr/bin")) {
        return path.to_string();
    }

    let mut entries: Vec<String> = std::env::split_paths(path)
        .map(|entry| entry.to_string_lossy().to_string())
        .filter(|entry| !entry.is_empty())
        .collect();
    for sane in SANE_PATH_ENTRIES {
        if !entries.iter().any(|entry| entry == sane) {
            entries.push((*sane).to_string());
        }
    }
    entries.join(":")
}

fn shell_env_cleanup_snippet(_configured_passthrough: &[String]) -> String {
    // Profiles run in the child process, so sanitize again after explicit
    // profile sourcing. There is intentionally no shell-level escape hatch
    // for force/passthrough control variables.
    let mut snippet = String::new();
    if !SUBPROCESS_ENV_BLOCKLIST_PREFIXES.is_empty() {
        snippet
            .push_str("for __hermes_env in $(env | sed 's/=.*//'); do case \"$__hermes_env\" in ");
        for (idx, prefix) in SUBPROCESS_ENV_BLOCKLIST_PREFIXES.iter().enumerate() {
            if idx > 0 {
                snippet.push('|');
            }
            snippet.push_str(prefix);
            snippet.push('*');
        }
        snippet.push_str(") unset \"$__hermes_env\" ;; esac; done; unset __hermes_env; ");
    }
    for key in SUBPROCESS_ENV_BLOCKLIST_EXACT {
        snippet.push_str("unset ");
        snippet.push_str(key);
        snippet.push_str(" 2>/dev/null || true; ");
    }
    snippet
}

fn parse_shell_init_list(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with('[') {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(trimmed) {
            return values
                .into_iter()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect();
        }
    }
    let delimiter = if trimmed.contains(',') { ',' } else { ':' };
    trimmed
        .split(delimiter)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn bool_env_or_default(name: &str, default: bool) -> bool {
    let Some(value) = std::env::var(name).ok() else {
        return default;
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn terminal_config_from_env() -> (Vec<String>, bool, TerminalHomeMode, Vec<String>) {
    let explicit = std::env::var("TERMINAL_SHELL_INIT_FILES")
        .ok()
        .map(|v| parse_shell_init_list(&v))
        .unwrap_or_default();
    let auto_source_bashrc = bool_env_or_default("TERMINAL_AUTO_SOURCE_BASHRC", true);
    let home_mode = std::env::var("TERMINAL_HOME_MODE")
        .ok()
        .and_then(|v| TerminalHomeMode::from_env_name(&v))
        .unwrap_or_default();
    let env_passthrough = std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR)
        .ok()
        .map(|v| parse_subprocess_env_passthrough(&v).into_iter().collect())
        .unwrap_or_default();
    (explicit, auto_source_bashrc, home_mode, env_passthrough)
}

fn expand_env_refs(input: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let name = &after_open[..end];
        if !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            if let Ok(value) = std::env::var(name) {
                output.push_str(&value);
            }
        } else {
            output.push_str("${");
            output.push_str(name);
            output.push('}');
        }
        rest = &after_open[end + 1..];
    }
    output.push_str(rest);
    output
}

fn shell_home_dir(home_override: Option<&std::path::Path>) -> Option<PathBuf> {
    home_override.map(PathBuf::from).or_else(home_dir)
}

fn expand_shell_init_path(input: &str, home_override: Option<&std::path::Path>) -> PathBuf {
    let expanded = expand_env_refs(input.trim());
    if expanded == "~" {
        return shell_home_dir(home_override).unwrap_or_else(|| PathBuf::from(expanded));
    }
    if let Some(rest) = expanded.strip_prefix("~/") {
        if let Some(home) = shell_home_dir(home_override) {
            return home.join(rest);
        }
    }
    PathBuf::from(expanded)
}

fn resolve_existing_shell_init_files(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    paths.into_iter().filter(|p| p.is_file()).collect()
}

fn auto_shell_init_candidates(
    shell: &str,
    home_override: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    let Some(home) = shell_home_dir(home_override) else {
        return Vec::new();
    };
    match shell {
        "zsh" => [".zshenv", ".zprofile", ".zshrc", ".profile"]
            .into_iter()
            .map(|file| home.join(file))
            .collect(),
        _ => [".profile", ".bash_profile", ".bashrc"]
            .into_iter()
            .map(|file| home.join(file))
            .collect(),
    }
}

fn resolve_shell_init_files_for_shell(
    shell: &str,
    explicit_files: &[String],
    auto_source_bashrc: bool,
    home_override: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    if !explicit_files.is_empty() {
        return resolve_existing_shell_init_files(
            explicit_files
                .iter()
                .map(|path| expand_shell_init_path(path.as_str(), home_override)),
        );
    }
    if !auto_source_bashrc {
        return Vec::new();
    }
    resolve_existing_shell_init_files(auto_shell_init_candidates(shell, home_override))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn shell_source_prelude(files: &[PathBuf]) -> String {
    let mut prelude = String::new();
    if files.is_empty() {
        return prelude;
    }
    prelude.push_str("set +e; ");
    for file in files {
        let quoted = shell_single_quote(&file.to_string_lossy());
        prelude.push_str("[ -r ");
        prelude.push_str(&quoted);
        prelude.push_str(" ] && . ");
        prelude.push_str(&quoted);
        prelude.push_str(" || true; ");
    }
    prelude
}

fn scrub_subprocess_env(cmd: &mut TokioCommand, configured_passthrough: &[String]) {
    // Start from a closed environment, rather than deleting selected values
    // from an inherited one. This prevents future secret names and dispatcher
    // controls from leaking merely because they were not in a local denylist.
    let mut environment = hermes_subprocess_env(false);
    for name in subprocess_env_passthrough_set(configured_passthrough) {
        if let Ok(value) = std::env::var(&name) {
            environment.insert(name, value);
        }
    }
    cmd.env_clear();
    cmd.envs(environment);
}

fn apply_subprocess_home_policy(cmd: &mut TokioCommand, subprocess_home: Option<&PathBuf>) {
    if let Some(real_home) = real_home_dir() {
        cmd.env("HERMES_REAL_HOME", real_home);
    }
    if let Some(home) = subprocess_home {
        cmd.env("HOME", home);
    }
}

fn with_login_profile_sources(
    command: &str,
    explicit_files: &[String],
    auto_source_bashrc: bool,
    env_passthrough: &[String],
    subprocess_home: Option<&std::path::Path>,
) -> String {
    #[cfg(unix)]
    {
        let cleanup = shell_env_cleanup_snippet(env_passthrough);
        let bash_prelude = shell_source_prelude(&resolve_shell_init_files_for_shell(
            "bash",
            explicit_files,
            auto_source_bashrc,
            subprocess_home,
        ));
        let zsh_prelude = shell_source_prelude(&resolve_shell_init_files_for_shell(
            "zsh",
            explicit_files,
            auto_source_bashrc,
            subprocess_home,
        ));
        let bash_command = shell_single_quote(&format!("{bash_prelude}{cleanup}{command}"));
        let zsh_command = shell_single_quote(&format!("{zsh_prelude}{cleanup}{command}"));
        let preferred_shell = std::env::var("SHELL")
            .ok()
            .and_then(|raw| {
                std::path::Path::new(raw.trim())
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.to_ascii_lowercase())
            })
            .filter(|name| matches!(name.as_str(), "bash" | "zsh"));
        let preferred_branch = match preferred_shell.as_deref() {
            Some("zsh") => {
                format!("if command -v zsh >/dev/null 2>&1; then exec zsh -lc {zsh_command}; fi; ")
            }
            Some("bash") => format!(
                "if command -v bash >/dev/null 2>&1; then exec bash -lc {bash_command}; fi; "
            ),
            _ => String::new(),
        };
        format!(
            "{preferred_branch}if command -v bash >/dev/null 2>&1; then exec bash -lc {bash_command}; \
elif command -v zsh >/dev/null 2>&1; then exec zsh -lc {zsh_command}; \
else printf '%s\n' \"Hermes could not find bash or zsh in PATH. Run 'exec zsh -l' or set your default shell where env vars are available.\" >&2; exit 127; fi"
        )
    }
    #[cfg(not(unix))]
    {
        let _ = explicit_files;
        let _ = auto_source_bashrc;
        let _ = env_passthrough;
        let _ = subprocess_home;
        command.to_string()
    }
}

fn rewrite_compound_background(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    for line in command.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map(|body| (body, "\n"))
            .unwrap_or((line, ""));
        out.push_str(&rewrite_compound_background_line(body));
        out.push_str(newline);
    }
    out
}

fn rewrite_compound_background_line(line: &str) -> String {
    if line.trim_start().starts_with('#') {
        return line.to_string();
    }
    let Some(amp_idx) = trailing_background_ampersand(line) else {
        return line.to_string();
    };
    let Some(op) = last_top_level_chain_operator(line, amp_idx) else {
        return line.to_string();
    };

    let mut tail_start = op.end;
    while tail_start < amp_idx {
        let Some(ch) = line[tail_start..amp_idx].chars().next() else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
        tail_start += ch.len_utf8();
    }
    if line[tail_start..amp_idx].trim().is_empty() {
        return line.to_string();
    }

    let mut rewritten = String::with_capacity(line.len() + 4);
    rewritten.push_str(&line[..tail_start]);
    rewritten.push_str("{ ");
    rewritten.push_str(&line[tail_start..=amp_idx]);
    rewritten.push_str(" }");
    rewritten.push_str(&line[amp_idx + 1..]);
    rewritten
}

#[derive(Clone, Copy)]
struct ChainOperator {
    end: usize,
}

fn trailing_background_ampersand(line: &str) -> Option<usize> {
    let mut idx = line.len();
    while idx > 0 {
        let (prev, ch) = line[..idx].char_indices().next_back()?;
        if !ch.is_whitespace() {
            idx = prev;
            break;
        }
        idx = prev;
    }
    if line[idx..].chars().next()? != '&' || is_escaped(line, idx) {
        return None;
    }
    if idx > 0 && line[..idx].ends_with('&') {
        return None;
    }
    Some(idx)
}

fn is_escaped(input: &str, idx: usize) -> bool {
    let mut count = 0usize;
    let mut pos = idx;
    while pos > 0 {
        let Some((prev, ch)) = input[..pos].char_indices().next_back() else {
            break;
        };
        if ch != '\\' {
            break;
        }
        count += 1;
        pos = prev;
    }
    count % 2 == 1
}

fn last_top_level_chain_operator(line: &str, stop: usize) -> Option<ChainOperator> {
    let mut last = None;
    let mut single = false;
    let mut double = false;
    let mut paren_depth = 0usize;
    let mut command_sub_depth = 0usize;
    let mut iter = line[..stop].char_indices().peekable();

    while let Some((idx, ch)) = iter.next() {
        if is_escaped(line, idx) {
            continue;
        }
        if single {
            if ch == '\'' {
                single = false;
            }
            continue;
        }
        if double {
            if ch == '"' {
                double = false;
                continue;
            }
            if ch == '$' && iter.peek().is_some_and(|(_, next)| *next == '(') {
                command_sub_depth += 1;
                iter.next();
            }
            continue;
        }

        match ch {
            '\'' => single = true,
            '"' => double = true,
            '$' if iter.peek().is_some_and(|(_, next)| *next == '(') => {
                command_sub_depth += 1;
                iter.next();
            }
            '(' if command_sub_depth == 0 => paren_depth += 1,
            ')' if command_sub_depth > 0 => command_sub_depth -= 1,
            ')' if paren_depth > 0 => paren_depth -= 1,
            ';' if paren_depth == 0 && command_sub_depth == 0 => last = None,
            '|' if paren_depth == 0 && command_sub_depth == 0 => {
                if iter.peek().is_some_and(|(_, next)| *next == '|') {
                    iter.next();
                    last = Some(ChainOperator { end: idx + 2 });
                } else {
                    last = None;
                }
            }
            '&' if paren_depth == 0
                && command_sub_depth == 0
                && iter.peek().is_some_and(|(_, next)| *next == '&') =>
            {
                iter.next();
                last = Some(ChainOperator { end: idx + 2 });
            }
            _ => {}
        }
    }
    last
}
