//! Cancellation-safe, bounded subprocess execution for local media helpers.
//!
//! Tree-owned commands run in a new Unix process group or a Windows Job Object.
//! Stderr draining is capped so a misbehaving child cannot retain unbounded
//! diagnostics or keep a completed command alive through an inherited pipe.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::{Child, Command as StdCommand, ExitStatus};
use std::time::{Duration, Instant as StdInstant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStderr, ChildStdin, Command};
use tokio::task::JoinHandle;
use tokio::time::{timeout, timeout_at, Instant};

const MAX_STDERR_BYTES: usize = 64 * 1024;
const STDERR_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const PROCESS_TERMINATE_GRACE: Duration = Duration::from_millis(150);
const PROCESS_REAP_TIMEOUT: Duration = Duration::from_secs(1);
const PROCESS_WAIT_POLL: Duration = Duration::from_millis(5);
const OUTPUT_LIMIT_POLL: Duration = Duration::from_millis(20);

pub(crate) struct BoundedCommandOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stderr: Vec<u8>,
}

/// Bounds the files a local command may create under its caller-owned output
/// directory while it is still running.
#[derive(Clone, Debug)]
pub(crate) struct BoundedOutputDirectoryLimit {
    directory: PathBuf,
    max_entries: usize,
    max_bytes: u64,
}

impl BoundedOutputDirectoryLimit {
    pub(crate) fn new(directory: PathBuf, max_entries: usize, max_bytes: u64) -> Self {
        Self {
            directory,
            max_entries,
            max_bytes,
        }
    }
}

#[derive(Debug)]
pub(crate) enum BoundedCommandError {
    Start(std::io::Error),
    Stdin(String),
    Wait(std::io::Error),
    Stderr(String),
    OutputLimit(String),
    TimedOut,
}

#[cfg(unix)]
fn configure_command_process_group(command: &mut StdCommand, kill_tree: bool) {
    use std::os::unix::process::CommandExt;

    if kill_tree {
        command.process_group(0);
    }
}

#[cfg(not(unix))]
fn configure_command_process_group(_command: &mut StdCommand, _kill_tree: bool) {}

#[cfg(unix)]
fn terminate_process_tree_sync(pid: Option<u32>, kill_tree: bool) {
    let Some(pid) = pid else {
        return;
    };
    let pid = pid as libc::pid_t;
    let target = if kill_tree { -pid } else { pid };
    unsafe {
        libc::kill(target, libc::SIGKILL);
    }

    // Cancellation invokes Drop synchronously. Reap the direct child before
    // the caller can release its trusted temporary directory; the process
    // group signal above removes descendants that inherited the tree.
    let deadline = StdInstant::now() + PROCESS_REAP_TIMEOUT;
    loop {
        let mut status = 0;
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if waited == pid || waited == -1 || StdInstant::now() >= deadline {
            break;
        }
        std::thread::sleep(PROCESS_WAIT_POLL);
    }
}

#[cfg(windows)]
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtResumeProcess(process_handle: windows_sys::Win32::Foundation::HANDLE) -> i32;
}

#[cfg(windows)]
struct OwnedJobObject {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl OwnedJobObject {
    fn assign(child: &Child) -> std::io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
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
        if unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) } == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(error);
        }
        Ok(Self { handle })
    }

    fn terminate(&self) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        unsafe {
            TerminateJobObject(self.handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for OwnedJobObject {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;

        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
fn configure_windows_suspended_tree(command: &mut StdCommand) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

    // CREATE_SUSPENDED prevents user code (and therefore any descendant) from
    // running before we assign the child to its kill-on-close Job Object.
    command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
}

#[cfg(windows)]
fn resume_windows_process(child: &Child) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;

    let status = unsafe { NtResumeProcess(child.as_raw_handle()) };
    if status < 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("NtResumeProcess failed with NTSTATUS 0x{status:08x}"),
        ));
    }
    Ok(())
}

async fn wait_for_bounded_child(
    child: &mut Child,
    deadline: Instant,
) -> Result<Option<ExitStatus>, std::io::Error> {
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(Some(status)),
            None => {
                let now = Instant::now();
                if now >= deadline {
                    return Ok(None);
                }
                tokio::time::sleep((deadline - now).min(PROCESS_WAIT_POLL)).await;
            }
        }
    }
}

#[cfg(unix)]
fn child_exit_is_pending_without_reap(pid: u32) -> Result<bool, std::io::Error> {
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
async fn wait_for_unreaped_owned_process_group(
    child: &mut Child,
    pid: u32,
    deadline: Instant,
) -> Result<Option<ExitStatus>, std::io::Error> {
    loop {
        if child_exit_is_pending_without_reap(pid)? {
            // The direct child is known to be exited but remains waitable, so
            // its PID/PGID cannot be reused yet. Kill descendants before the
            // first reap; after that point Drop must never signal -pid.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            return child.wait().map(Some);
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        tokio::time::sleep((deadline - now).min(PROCESS_WAIT_POLL)).await;
    }
}

async fn terminate_bounded_child(child: &mut Child, pid: Option<u32>, kill_tree: bool) -> bool {
    #[cfg(unix)]
    if kill_tree {
        if let Some(pid) = pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
            tokio::time::sleep(PROCESS_TERMINATE_GRACE).await;
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    matches!(
        wait_for_bounded_child(child, Instant::now() + PROCESS_REAP_TIMEOUT).await,
        Ok(Some(_))
    )
}

struct BoundedChildGuard {
    child: Option<Child>,
    pid: Option<u32>,
    kill_tree: bool,
    #[cfg(windows)]
    job: Option<OwnedJobObject>,
}

impl BoundedChildGuard {
    fn new(child: Child, kill_tree: bool) -> Self {
        let pid = Some(child.id());
        Self {
            child: Some(child),
            pid,
            kill_tree,
            #[cfg(windows)]
            job: None,
        }
    }

    #[cfg(windows)]
    fn attach_job_and_resume(&mut self) -> std::io::Result<()> {
        let child = self.child.as_ref().expect("bounded child present");
        let job = OwnedJobObject::assign(child)?;
        if let Err(error) = resume_windows_process(child) {
            drop(job);
            return Err(error);
        }
        self.job = Some(job);
        Ok(())
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("bounded child present")
    }

    async fn wait_until(
        &mut self,
        deadline: Instant,
    ) -> Result<Option<ExitStatus>, std::io::Error> {
        #[cfg(unix)]
        if self.kill_tree {
            let pid = self.pid.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "owned process group leader was unavailable before wait",
                )
            })?;
            return wait_for_unreaped_owned_process_group(self.child_mut(), pid, deadline).await;
        }
        wait_for_bounded_child(self.child_mut(), deadline).await
    }

    fn mark_reaped(&mut self) {
        self.child.take();
        self.pid.take();
    }

    async fn terminate(&mut self) {
        #[cfg(windows)]
        if self.kill_tree {
            if let Some(job) = self.job.as_ref() {
                job.terminate();
            }
        }
        let reaped = if let Some(child) = self.child.as_mut() {
            terminate_bounded_child(child, self.pid, self.kill_tree).await
        } else {
            true
        };
        if reaped {
            self.mark_reaped();
        }
        #[cfg(windows)]
        self.job.take();
    }

    fn disarm(&mut self) {
        self.mark_reaped();
        #[cfg(windows)]
        self.job.take();
    }
}

impl Drop for BoundedChildGuard {
    fn drop(&mut self) {
        let child = self.child.take();
        #[cfg(unix)]
        {
            if let Some(mut child) = child {
                terminate_process_tree_sync(self.pid, self.kill_tree);
                let _ = child.kill();
            }
        }
        #[cfg(windows)]
        {
            // Closing this owned handle is the cancellation action: Windows
            // terminates every process in the Job because of KILL_ON_JOB_CLOSE.
            // Do not spawn taskkill or perform blocking work from Drop.
            drop(self.job.take());
            drop(child);
        }
        #[cfg(not(any(unix, windows)))]
        {
            if let Some(mut child) = child {
                let _ = child.kill();
            }
        }
    }
}

struct AbortOnDropJoinHandle<T> {
    handle: Option<JoinHandle<T>>,
}

impl<T> AbortOnDropJoinHandle<T> {
    fn new(handle: JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn handle_mut(&mut self) -> &mut JoinHandle<T> {
        self.handle.as_mut().expect("stderr task present")
    }

    async fn abort_and_wait(&mut self) {
        if let Some(handle) = self.handle.take() {
            if !handle.is_finished() {
                handle.abort();
                let _ = handle.await;
            }
        }
    }

    fn clear(&mut self) {
        self.handle.take();
    }
}

impl<T> Drop for AbortOnDropJoinHandle<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

async fn check_output_directory_limit(limit: &BoundedOutputDirectoryLimit) -> Result<(), String> {
    let mut entries = tokio::fs::read_dir(&limit.directory)
        .await
        .map_err(|error| {
            format!(
                "could not inspect bounded command output directory {}: {error}",
                limit.directory.display()
            )
        })?;
    let mut entry_count = 0_usize;
    let mut total_bytes = 0_u64;
    while let Some(entry) = entries.next_entry().await.map_err(|error| {
        format!(
            "could not enumerate bounded command output directory {}: {error}",
            limit.directory.display()
        )
    })? {
        entry_count = entry_count.checked_add(1).ok_or_else(|| {
            format!(
                "bounded command output directory {} has too many entries",
                limit.directory.display()
            )
        })?;
        if entry_count > limit.max_entries {
            return Err(format!(
                "bounded command created more than {} output entries in {}",
                limit.max_entries,
                limit.directory.display()
            ));
        }
        let file_type = entry.file_type().await.map_err(|error| {
            format!(
                "could not inspect bounded command output entry {}: {error}",
                entry.path().display()
            )
        })?;
        if !file_type.is_file() {
            return Err(format!(
                "bounded command created a non-file output entry {}",
                entry.path().display()
            ));
        }
        let metadata = entry.metadata().await.map_err(|error| {
            format!(
                "could not inspect bounded command output entry {}: {error}",
                entry.path().display()
            )
        })?;
        total_bytes = total_bytes.checked_add(metadata.len()).ok_or_else(|| {
            format!(
                "bounded command output size overflowed in {}",
                limit.directory.display()
            )
        })?;
        if total_bytes > limit.max_bytes {
            return Err(format!(
                "bounded command wrote more than {} bytes in {}",
                limit.max_bytes,
                limit.directory.display()
            ));
        }
    }
    Ok(())
}

async fn monitor_output_directory_limit(limit: BoundedOutputDirectoryLimit) -> Result<(), String> {
    loop {
        check_output_directory_limit(&limit).await?;
        tokio::time::sleep(OUTPUT_LIMIT_POLL).await;
    }
}

async fn abort_output_limit_monitor(
    monitor: &mut Option<AbortOnDropJoinHandle<Result<(), String>>>,
) {
    if let Some(monitor) = monitor {
        monitor.abort_and_wait().await;
    }
}

pub(crate) async fn run_bounded_command(
    command: Command,
    duration: Duration,
    kill_tree: bool,
) -> Result<BoundedCommandOutput, BoundedCommandError> {
    run_bounded_command_inner(command, duration, kill_tree, None, None).await
}

pub(crate) async fn run_bounded_command_with_output_limit(
    command: Command,
    duration: Duration,
    kill_tree: bool,
    output_limit: BoundedOutputDirectoryLimit,
) -> Result<BoundedCommandOutput, BoundedCommandError> {
    run_bounded_command_inner(command, duration, kill_tree, None, Some(output_limit)).await
}

pub(crate) async fn run_bounded_command_with_stdin_and_output_limit(
    command: Command,
    duration: Duration,
    kill_tree: bool,
    input: &[u8],
    output_limit: BoundedOutputDirectoryLimit,
) -> Result<BoundedCommandOutput, BoundedCommandError> {
    run_bounded_command_inner(
        command,
        duration,
        kill_tree,
        Some(input),
        Some(output_limit),
    )
    .await
}

pub(crate) async fn run_bounded_command_with_stdin(
    command: Command,
    duration: Duration,
    kill_tree: bool,
    input: Option<&[u8]>,
) -> Result<BoundedCommandOutput, BoundedCommandError> {
    run_bounded_command_inner(command, duration, kill_tree, input, None).await
}

async fn run_bounded_command_inner(
    command: Command,
    duration: Duration,
    kill_tree: bool,
    input: Option<&[u8]>,
    output_limit: Option<BoundedOutputDirectoryLimit>,
) -> Result<BoundedCommandOutput, BoundedCommandError> {
    let mut command = command.into_std();
    configure_command_process_group(&mut command, kill_tree);
    #[cfg(windows)]
    if kill_tree {
        configure_windows_suspended_tree(&mut command);
    }

    let child = command.spawn().map_err(BoundedCommandError::Start)?;
    let mut child = BoundedChildGuard::new(child, kill_tree);
    #[cfg(windows)]
    if kill_tree {
        if let Err(error) = child.attach_job_and_resume() {
            child.terminate().await;
            return Err(BoundedCommandError::Start(std::io::Error::new(
                error.kind(),
                format!("failed assigning suspended child to kill-on-close Job Object: {error}"),
            )));
        }
    }

    let deadline = Instant::now() + duration;
    let stderr = match child.child_mut().stderr.take() {
        Some(stderr) => Some(ChildStderr::from_std(stderr).map_err(BoundedCommandError::Start)?),
        None => None,
    };
    let mut stderr_task = AbortOnDropJoinHandle::new(tokio::spawn(async move {
        let mut tail = VecDeque::with_capacity(MAX_STDERR_BYTES);
        let mut truncated = false;
        if let Some(mut stderr) = stderr {
            let mut chunk = [0_u8; 8 * 1024];
            loop {
                let read = stderr
                    .read(&mut chunk)
                    .await
                    .map_err(|error| error.to_string())?;
                if read == 0 {
                    break;
                }
                let incoming = &chunk[..read];
                if incoming.len() >= MAX_STDERR_BYTES {
                    tail.clear();
                    tail.extend(
                        incoming[incoming.len().saturating_sub(MAX_STDERR_BYTES)..]
                            .iter()
                            .copied(),
                    );
                    truncated = true;
                    continue;
                }
                let excess = tail
                    .len()
                    .saturating_add(incoming.len())
                    .saturating_sub(MAX_STDERR_BYTES);
                if excess > 0 {
                    tail.drain(..excess);
                    truncated = true;
                }
                tail.extend(incoming.iter().copied());
            }
        }
        let mut bytes = Vec::with_capacity(
            tail.len()
                + if truncated {
                    b"[...stderr truncated; showing tail...]\n".len()
                } else {
                    0
                },
        );
        if truncated {
            bytes.extend_from_slice(b"[...stderr truncated; showing tail...]\n");
        }
        bytes.extend(tail);
        Ok::<_, String>(bytes)
    }));
    let mut output_limit_monitor = output_limit.clone().map(|limit| {
        AbortOnDropJoinHandle::new(tokio::spawn(monitor_output_directory_limit(limit)))
    });

    if let Some(input) = input {
        let Some(stdin) = child.child_mut().stdin.take() else {
            child.terminate().await;
            abort_output_limit_monitor(&mut output_limit_monitor).await;
            stderr_task.abort_and_wait().await;
            return Err(BoundedCommandError::Stdin(
                "child stdin was not configured as piped".to_string(),
            ));
        };
        let mut stdin = match ChildStdin::from_std(stdin) {
            Ok(stdin) => stdin,
            Err(error) => {
                child.terminate().await;
                abort_output_limit_monitor(&mut output_limit_monitor).await;
                stderr_task.abort_and_wait().await;
                return Err(BoundedCommandError::Stdin(error.to_string()));
            }
        };
        enum StdinCompletion {
            Written(Result<(), std::io::Error>),
            OutputLimit(Result<Result<(), String>, tokio::task::JoinError>),
            TimedOut,
        }
        let write_stdin = async {
            stdin.write_all(input).await?;
            stdin.shutdown().await
        };
        let completion = if let Some(monitor) = output_limit_monitor.as_mut() {
            tokio::select! {
                result = write_stdin => StdinCompletion::Written(result),
                result = monitor.handle_mut() => StdinCompletion::OutputLimit(result),
                _ = tokio::time::sleep_until(deadline) => StdinCompletion::TimedOut,
            }
        } else {
            match timeout_at(deadline, write_stdin).await {
                Ok(result) => StdinCompletion::Written(result),
                Err(_) => StdinCompletion::TimedOut,
            }
        };
        match completion {
            StdinCompletion::Written(Ok(())) => {}
            StdinCompletion::Written(Err(error)) => {
                child.terminate().await;
                abort_output_limit_monitor(&mut output_limit_monitor).await;
                stderr_task.abort_and_wait().await;
                return Err(BoundedCommandError::Stdin(error.to_string()));
            }
            StdinCompletion::OutputLimit(result) => {
                child.terminate().await;
                abort_output_limit_monitor(&mut output_limit_monitor).await;
                stderr_task.abort_and_wait().await;
                return match result {
                    Ok(Err(error)) => Err(BoundedCommandError::OutputLimit(error)),
                    Ok(Ok(())) => Err(BoundedCommandError::OutputLimit(
                        "bounded command output monitor stopped unexpectedly".to_string(),
                    )),
                    Err(error) => Err(BoundedCommandError::OutputLimit(format!(
                        "bounded command output monitor failed: {error}"
                    ))),
                };
            }
            StdinCompletion::TimedOut => {
                child.terminate().await;
                abort_output_limit_monitor(&mut output_limit_monitor).await;
                stderr_task.abort_and_wait().await;
                return Err(BoundedCommandError::TimedOut);
            }
        }
    }

    enum CommandCompletion {
        Child(Result<Option<ExitStatus>, std::io::Error>),
        OutputLimit(Result<Result<(), String>, tokio::task::JoinError>),
    }

    let completion = if let Some(monitor) = output_limit_monitor.as_mut() {
        tokio::select! {
            result = child.wait_until(deadline) => CommandCompletion::Child(result),
            result = monitor.handle_mut() => CommandCompletion::OutputLimit(result),
        }
    } else {
        CommandCompletion::Child(child.wait_until(deadline).await)
    };
    let status = match completion {
        CommandCompletion::Child(Ok(Some(status))) => {
            abort_output_limit_monitor(&mut output_limit_monitor).await;
            status
        }
        CommandCompletion::Child(Ok(None)) => {
            child.terminate().await;
            abort_output_limit_monitor(&mut output_limit_monitor).await;
            stderr_task.abort_and_wait().await;
            return Err(BoundedCommandError::TimedOut);
        }
        CommandCompletion::Child(Err(error)) => {
            child.terminate().await;
            abort_output_limit_monitor(&mut output_limit_monitor).await;
            stderr_task.abort_and_wait().await;
            return Err(BoundedCommandError::Wait(error));
        }
        CommandCompletion::OutputLimit(result) => {
            child.terminate().await;
            stderr_task.abort_and_wait().await;
            return match result {
                Ok(Err(error)) => Err(BoundedCommandError::OutputLimit(error)),
                Ok(Ok(())) => Err(BoundedCommandError::OutputLimit(
                    "bounded command output monitor stopped unexpectedly".to_string(),
                )),
                Err(error) => Err(BoundedCommandError::OutputLimit(format!(
                    "bounded command output monitor failed: {error}"
                ))),
            };
        }
    };
    // From here onward stderr draining can await. The direct child has already
    // been reaped and, on Unix, its process group was killed while the PID was
    // still waitable. On Windows, end the owned Job now too: a descendant that
    // outlives the direct child must not mutate output between the final quota
    // check and return.
    #[cfg(windows)]
    if kill_tree {
        child.terminate().await;
    }
    child.mark_reaped();
    if let Some(limit) = output_limit.as_ref() {
        if let Err(error) = check_output_directory_limit(limit).await {
            stderr_task.abort_and_wait().await;
            child.terminate().await;
            return Err(BoundedCommandError::OutputLimit(error));
        }
    }
    let stderr = match timeout(STDERR_DRAIN_TIMEOUT, stderr_task.handle_mut()).await {
        Ok(Ok(Ok(stderr))) => {
            stderr_task.clear();
            stderr
        }
        Ok(Ok(Err(error))) => {
            child.terminate().await;
            stderr_task.abort_and_wait().await;
            return Err(BoundedCommandError::Stderr(error));
        }
        Ok(Err(error)) => {
            child.terminate().await;
            stderr_task.abort_and_wait().await;
            return Err(BoundedCommandError::Stderr(error.to_string()));
        }
        Err(_) => {
            stderr_task.abort_and_wait().await;
            child.terminate().await;
            return Err(BoundedCommandError::Stderr(
                "stderr pipe remained open after command exit".to_string(),
            ));
        }
    };
    child.disarm();
    Ok(BoundedCommandOutput { status, stderr })
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::path::Path;
    use std::process::Stdio;
    use std::time::Duration;

    const PID_ENV: &str = "HERMES_BOUNDED_SUBPROCESS_TEST_PID";

    struct EnvGuard(Option<std::ffi::OsString>);

    impl EnvGuard {
        fn set(value: &Path) -> Self {
            let original = std::env::var_os(PID_ENV);
            unsafe {
                std::env::set_var(PID_ENV, value);
            }
            Self(original)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => unsafe {
                    std::env::set_var(PID_ENV, value);
                },
                None => unsafe {
                    std::env::remove_var(PID_ENV);
                },
            }
        }
    }

    async fn wait_for_pid(path: &Path) -> u32 {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(value) = std::fs::read_to_string(path) {
                    if let Ok(pid) = value.trim().parse() {
                        return pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("immediately forked child did not report its pid")
    }

    async fn assert_pid_exited(pid: u32) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
                if handle.is_null() {
                    return;
                }
                let mut status = 0;
                let queried = unsafe { GetExitCodeProcess(handle, &mut status) };
                unsafe {
                    CloseHandle(handle);
                }
                if queried != 0 && status != 259 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Job Object did not terminate immediately forked child");
    }

    #[tokio::test]
    async fn job_object_owns_immediately_forked_descendant_after_parent_exit() {
        let temp = tempfile::tempdir().unwrap();
        let pid_path = temp.path().join("descendant.pid");
        let script = temp.path().join("immediate-fork.cmd");
        std::fs::write(
            &script,
            "@echo off\r\nstart \"\" /b powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command \"$PID | Set-Content -NoNewline -Encoding ascii $env:HERMES_BOUNDED_SUBPROCESS_TEST_PID; while ($true) { [Console]::Error.WriteLine('holding stderr'); Start-Sleep -Milliseconds 25 }\"\r\nexit /b 0\r\n",
        )
        .unwrap();
        let _pid_env = EnvGuard::set(&pid_path);
        let script_arg = script.to_string_lossy().into_owned();
        let mut command = Command::new("cmd.exe");
        command
            .args(["/C", script_arg.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let task = tokio::spawn(run_bounded_command(command, Duration::from_secs(3), true));
        let descendant_pid = wait_for_pid(&pid_path).await;
        let result = task.await.unwrap();

        assert!(
            result.is_ok() || matches!(result, Err(BoundedCommandError::Stderr(_))),
            "unexpected bounded command result"
        );
        assert_pid_exited(descendant_pid).await;
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command as StdCommand, Stdio};

    async fn wait_for_pid(path: &std::path::Path) -> u32 {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(value) = std::fs::read_to_string(path) {
                    if let Ok(pid) = value.trim().parse() {
                        return pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("descendant did not report its pid")
    }

    async fn assert_pid_exited(pid: u32) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("owned process-group descendant remained alive");
    }

    #[tokio::test]
    async fn exited_leader_group_is_killed_before_reap_and_drop() {
        let temp = tempfile::tempdir().unwrap();
        let pid_path = temp.path().join("descendant.pid");
        let script = temp.path().join("immediate-exit.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nchild_pid_file=$1\nsh -c 'printf \"%s\" \"$$\" > \"$1\"; trap \"\" TERM; while :; do printf \"holding stderr\\\\n\" >&2; done' sh \"$child_pid_file\" &\nwhile [ ! -s \"$child_pid_file\" ]; do :; done\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut command = StdCommand::new(&script);
        command
            .arg(&pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        configure_command_process_group(&mut command, true);
        let child = command.spawn().unwrap();
        let mut child = BoundedChildGuard::new(child, true);
        let descendant_pid = wait_for_pid(&pid_path).await;

        let status = child
            .wait_until(Instant::now() + Duration::from_secs(2))
            .await
            .unwrap()
            .expect("leader did not exit before deadline");

        assert!(status.success());
        child.mark_reaped();
        assert!(child.pid.is_none(), "reaped leader PID remained armed");
        drop(child);
        assert_pid_exited(descendant_pid).await;
    }

    #[tokio::test]
    async fn cancellation_kills_and_reaps_owned_descendant_tree() {
        let temp = tempfile::tempdir().unwrap();
        let pid_path = temp.path().join("descendant.pid");
        let script = temp.path().join("hold-tree.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nchild_pid_file=$1\nsh -c 'printf \"%s\" \"$$\" > \"$1\"; while :; do sleep 1; done' sh \"$child_pid_file\" &\nwhile [ ! -s \"$child_pid_file\" ]; do :; done\nwhile :; do sleep 1; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut command = Command::new(&script);
        command
            .arg(&pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let task = tokio::spawn(run_bounded_command(command, Duration::from_secs(30), true));
        let descendant_pid = wait_for_pid(&pid_path).await;
        task.abort();
        let _ = task.await;
        assert_pid_exited(descendant_pid).await;
    }

    #[tokio::test]
    async fn completed_command_is_rejected_by_final_output_quota_check() {
        let temp = tempfile::tempdir().unwrap();
        let output_dir = temp.path().join("output");
        std::fs::create_dir(&output_dir).unwrap();
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("printf x > \"$1/quick.mp3\"")
            .arg("bounded-output")
            .arg(&output_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let result = run_bounded_command_with_output_limit(
            command,
            Duration::from_secs(1),
            true,
            BoundedOutputDirectoryLimit::new(output_dir, 2, 0),
        )
        .await;
        assert!(
            matches!(result, Err(BoundedCommandError::OutputLimit(_))),
            "a quick over-limit command must fail even if it exits before the polling monitor"
        );
    }

    #[tokio::test]
    async fn output_limit_interrupts_a_blocked_stdin_write() {
        let temp = tempfile::tempdir().unwrap();
        let output_dir = temp.path().join("output");
        std::fs::create_dir(&output_dir).unwrap();
        let script = temp.path().join("write-before-read.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf x > \"$1/quick.mp3\"\nwhile :; do sleep 1; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut command = Command::new(&script);
        command
            .arg(&output_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let input = vec![b'x'; 1024 * 1024];
        let started = std::time::Instant::now();
        let result = run_bounded_command_with_stdin_and_output_limit(
            command,
            Duration::from_secs(3),
            true,
            &input,
            BoundedOutputDirectoryLimit::new(output_dir, 2, 0),
        )
        .await;
        assert!(matches!(result, Err(BoundedCommandError::OutputLimit(_))));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the output limit did not interrupt the blocked stdin write"
        );
    }
}
