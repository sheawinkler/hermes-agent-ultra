//! Cross-platform IPC transport abstraction.
//!
//! Windows: Named Pipe
//! Unix: Unix Domain Socket

use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

/// Errors from IPC operations.
#[derive(Debug)]
pub enum IpcError {
    Io(io::Error),
    /// Too many connections.
    ConnectionLimit,
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcError::Io(e) => write!(f, "IPC I/O error: {}", e),
            IpcError::ConnectionLimit => write!(f, "Connection limit reached"),
        }
    }
}

impl std::error::Error for IpcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IpcError::Io(e) => Some(e),
            IpcError::ConnectionLimit => None,
        }
    }
}

impl From<io::Error> for IpcError {
    fn from(e: io::Error) -> Self {
        IpcError::Io(e)
    }
}

/// IPC listener trait.
#[async_trait::async_trait]
pub trait IpcListener: Send + Sync {
    /// Accept a new IPC connection.
    async fn accept(&self) -> Result<Box<dyn IpcStream>, IpcError>;
    /// Return the endpoint path for logging.
    fn endpoint(&self) -> &str;
}

/// IPC bidirectional stream.
pub trait IpcStream: AsyncRead + AsyncWrite + Send + Unpin + 'static {
    /// A label identifying the peer (for logging).
    fn peer_label(&self) -> String;
}

// ---------------------------------------------------------------------------
// Platform-specific implementations
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod platform_impl {
    use super::*;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

    /// Windows system error code indicating all pipe instances are busy.
    const ERROR_PIPE_BUSY: i32 = 231;

    pub struct WindowsPipeListener {
        pipe_path: String,
        server_options: ServerOptions,
    }

    impl WindowsPipeListener {
        pub fn new(pipe_path: &str, max_connections: usize) -> io::Result<Self> {
            let mut opts = ServerOptions::new();
            opts.max_instances(max_connections)
                .reject_remote_clients(true);
            Ok(Self {
                pipe_path: pipe_path.to_string(),
                server_options: opts,
            })
        }
    }

    #[async_trait::async_trait]
    impl IpcListener for WindowsPipeListener {
        async fn accept(&self) -> Result<Box<dyn IpcStream>, IpcError> {
            // Windows limits concurrent pipe instances to max_instances.
            // Once exhausted, create() returns ERROR_PIPE_BUSY until a slot frees.
            // Loop with backoff + a 30s timeout so shutdown isn't blocked forever.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if std::time::Instant::now() > deadline {
                    return Err(IpcError::Io(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for a free pipe instance",
                    )));
                }
                match self.server_options.create(&self.pipe_path) {
                    Ok(server) => {
                        server.connect().await?;
                        return Ok(Box::new(WindowsPipeStream { inner: server }));
                    }
                    Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }

        fn endpoint(&self) -> &str {
            &self.pipe_path
        }
    }

    struct WindowsPipeStream {
        inner: NamedPipeServer,
    }

    impl IpcStream for WindowsPipeStream {
        fn peer_label(&self) -> String {
            "named-pipe-client".to_string()
        }
    }

    impl tokio::io::AsyncRead for WindowsPipeStream {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
        }
    }

    impl tokio::io::AsyncWrite for WindowsPipeStream {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
        }
    }

    pub fn create_listener(
        pipe_path: &str,
        max_connections: usize,
    ) -> Result<Box<dyn IpcListener>, IpcError> {
        let listener = WindowsPipeListener::new(pipe_path, max_connections)?;
        Ok(Box::new(listener))
    }

    /// Make a throwaway client connection to unblock a pending `accept()`.
    /// Used during graceful shutdown to wake the accept loop.
    pub async fn poke_listener(pipe_path: &str) {
        // ClientOptions::open is sync; wrap in a brief async context.
        match ClientOptions::new().open(pipe_path) {
            Ok(_) => tracing::debug!(pipe = %pipe_path, "poke_listener connected"),
            Err(e) => {
                tracing::debug!(pipe = %pipe_path, error = %e, "poke_listener connect failed (pipe may not exist yet)")
            }
        }
    }
}

#[cfg(unix)]
mod platform_impl {
    use super::*;
    use tokio::net::UnixListener;
    use tokio::net::UnixStream;

    pub struct UnixSocketListener {
        pipe_path: String,
        listener: UnixListener,
    }

    impl UnixSocketListener {
        pub fn new(pipe_path: &str) -> io::Result<Self> {
            // Remove stale socket file
            let _ = std::fs::remove_file(pipe_path);
            let listener = UnixListener::bind(pipe_path)?;
            Ok(Self {
                pipe_path: pipe_path.to_string(),
                listener,
            })
        }
    }

    #[async_trait::async_trait]
    impl IpcListener for UnixSocketListener {
        async fn accept(&self) -> Result<Box<dyn IpcStream>, IpcError> {
            let (stream, _addr) = self.listener.accept().await?;
            Ok(Box::new(UnixSocketStream { inner: stream }))
        }

        fn endpoint(&self) -> &str {
            &self.pipe_path
        }
    }

    struct UnixSocketStream {
        inner: UnixStream,
    }

    impl IpcStream for UnixSocketStream {
        fn peer_label(&self) -> String {
            "unix-socket-client".to_string()
        }
    }

    impl tokio::io::AsyncRead for UnixSocketStream {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
        }
    }

    impl tokio::io::AsyncWrite for UnixSocketStream {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
        }
    }

    pub fn create_listener(
        pipe_path: &str,
        _max_connections: usize,
    ) -> Result<Box<dyn IpcListener>, IpcError> {
        let listener = UnixSocketListener::new(pipe_path)?;
        Ok(Box::new(listener))
    }

    /// Make a throwaway client connection to unblock a pending `accept()`.
    pub async fn poke_listener(pipe_path: &str) {
        match UnixStream::connect(pipe_path).await {
            Ok(_) => tracing::debug!(pipe = %pipe_path, "poke_listener connected"),
            Err(e) => {
                tracing::debug!(pipe = %pipe_path, error = %e, "poke_listener connect failed (socket may not exist yet)")
            }
        }
    }
}

#[cfg(windows)]
pub use platform_impl::create_listener;
#[cfg(windows)]
pub use platform_impl::poke_listener;

#[cfg(unix)]
pub use platform_impl::create_listener;
#[cfg(unix)]
pub use platform_impl::poke_listener;

/// Default IPC endpoint path for the current platform.
///
/// Returns `String` (not `&'static str`) because the Unix path includes the
/// process PID to prevent symlink attacks in shared `/tmp`. Called once at
/// server startup — the allocation cost is negligible.
pub fn default_pipe_path() -> String {
    if cfg!(windows) {
        r"\\.\pipe\AIPC-acp".to_string()
    } else {
        // Use $XDG_RUNTIME_DIR when available (usually `/run/user/$UID`),
        // otherwise fall back to the system temp dir.
        let dir = std::env::var("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        // PID-scoped name prevents collisions between multiple hermes instances.
        dir.join(format!("hermes-acp-{}.sock", std::process::id()))
            .to_string_lossy()
            .to_string()
    }
}
