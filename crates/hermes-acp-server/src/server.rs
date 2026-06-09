//! ACP Pipe Server -- accept loop + multi-connection management.
//!
//! # Shutdown
//!
//! `shutdown()` uses `tokio::spawn` internally to poke the listener and
//! unblock the accept loop. The caller must therefore be within a tokio
//! runtime; otherwise `tokio::spawn` will panic. Always call from within an
//! async runtime (e.g. a slash command handler running on the main tokio
//! task).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Lock a `Mutex`, recovering from poisoning instead of panicking.
/// A poisoned mutex means a panic occurred while holding the lock;
/// in this server that is recoverable — we continue with the last state.
fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(e) => e.into_inner(),
    }
}

/// RAII guard that removes a connection entry when dropped.
/// Ensures cleanup even if the connection task panics.
struct ConnectionCleanupGuard {
    conns: Arc<Mutex<HashMap<String, ConnectionInfo>>>,
    conn_id: String,
}

impl Drop for ConnectionCleanupGuard {
    fn drop(&mut self) {
        lock_recover(&self.conns).remove(&self.conn_id);
    }
}

use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use crate::connection::{AcpConnection, AgentInfo, ConnectionMetaCb};
use crate::executor::PromptExecutor;
use crate::platform;
use crate::session::MetaUpdate;

// ---------------------------------------------------------------------------
// Server events (for CLI visibility)
// ---------------------------------------------------------------------------

/// Lifecycle events emitted by the ACP server.
///
/// These are intended for user-facing output (e.g. printed in the CLI
/// panel) so operators can observe connection activity without reading
/// tracing logs.
#[derive(Debug, Clone)]
pub enum AcpServerEvent {
    /// A client has completed initialize and we know its name.
    ClientConnected {
        conn_id: String,
        client_name: Option<String>,
        client_title: Option<String>,
    },
    /// A prompt was received from a client.
    PromptReceived {
        conn_id: String,
        session_id: String,
        prompt_len: usize,
    },
    /// A prompt finished executing.
    PromptCompleted {
        conn_id: String,
        session_id: String,
        stop_reason: String,
    },
    /// A client disconnected.
    ClientDisconnected { conn_id: String },
}

pub type AcpServerEventSink = Arc<dyn Fn(AcpServerEvent) + Send + Sync>;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// ACP Pipe Server configuration.
pub struct AcpServerConfig {
    /// IPC endpoint path.
    pub pipe_path: String,
    /// Maximum concurrent connections (default: 5).
    pub max_connections: usize,
    /// Prompt execution timeout in seconds (default: 300).
    pub prompt_timeout_secs: u64,
    /// Agent brand information.
    pub agent_info: AgentInfo,
    /// Prompt executor.
    pub executor: Arc<dyn PromptExecutor>,
    /// Optional event sink for user-facing connection lifecycle events.
    pub event_sink: Option<AcpServerEventSink>,
}

impl std::fmt::Debug for AcpServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpServerConfig")
            .field("pipe_path", &self.pipe_path)
            .field("max_connections", &self.max_connections)
            .field("prompt_timeout_secs", &self.prompt_timeout_secs)
            .field("agent_info", &self.agent_info)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Connection info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub id: String,
    pub client_name: Option<String>,
    pub client_title: Option<String>,
    pub session_id: Option<String>,
    pub is_cherry: bool,
}

// ---------------------------------------------------------------------------
// Server error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum AcpServerError {
    Io(std::io::Error),
    Platform(crate::platform::IpcError),
}

impl std::fmt::Display for AcpServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcpServerError::Io(e) => write!(f, "I/O error: {}", e),
            AcpServerError::Platform(e) => write!(f, "IPC error: {}", e),
        }
    }
}

impl std::error::Error for AcpServerError {}

impl From<std::io::Error> for AcpServerError {
    fn from(e: std::io::Error) -> Self {
        AcpServerError::Io(e)
    }
}
impl From<crate::platform::IpcError> for AcpServerError {
    fn from(e: crate::platform::IpcError) -> Self {
        AcpServerError::Platform(e)
    }
}

// ---------------------------------------------------------------------------
// AcpPipeServer
// ---------------------------------------------------------------------------

/// Multi-client ACP Pipe Server.
pub struct AcpPipeServer {
    config: AcpServerConfig,
    shutdown: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
    // SAFETY: This Mutex is std::sync (blocking). All lock sites are very
    // short (single HashMap op) and never held across .await points.
    // Do NOT introduce an .await while holding this lock.
    connections: Arc<Mutex<HashMap<String, ConnectionInfo>>>,
    event_sink: Option<AcpServerEventSink>,
}

impl AcpPipeServer {
    pub fn new(config: AcpServerConfig) -> Result<Self, AcpServerError> {
        let event_sink = config.event_sink.clone();
        Ok(Self {
            config,
            shutdown: Arc::new(AtomicBool::new(false)),
            shutdown_notify: Arc::new(Notify::new()),
            connections: Arc::new(Mutex::new(HashMap::new())),
            event_sink,
        })
    }

    /// Start the accept loop. Blocks until shutdown.
    pub async fn run(&self) -> Result<(), AcpServerError> {
        let listener =
            platform::create_listener(&self.config.pipe_path, self.config.max_connections)?;

        info!(
            endpoint = %listener.endpoint(),
            max = self.config.max_connections,
            "ACP server listening"
        );

        loop {
            let accept_result = tokio::select! {
                r = listener.accept() => r,
                _ = self.shutdown_notify.notified() => {
                    info!("ACP server shutdown via notify");
                    break;
                }
            };

            if self.shutdown.load(Ordering::Acquire) {
                info!("ACP server shutting down");
                break;
            }

            match accept_result {
                Ok(stream) => {
                    let conn_count = lock_recover(&self.connections).len();
                    if conn_count >= self.config.max_connections {
                        warn!(
                            current = conn_count,
                            max = self.config.max_connections,
                            "rejecting connection"
                        );
                        continue;
                    }

                    let conn_id = uuid::Uuid::new_v4().to_string();

                    let conns = self.connections.clone();
                    let conn_id_cb = conn_id.clone();
                    let meta_cb: ConnectionMetaCb =
                        Arc::new(move |id: String, update: MetaUpdate| {
                            if let Some(info) = lock_recover(&conns).get_mut(&id) {
                                info.client_name = update.client_name;
                                info.client_title = update.client_title;
                                info.session_id = update.session_id;
                                info.is_cherry = info.client_name.as_deref() == Some("ai-cherry");
                            }
                        });

                    let mut conn = AcpConnection::new(
                        conn_id.clone(),
                        self.config.agent_info.clone(),
                        self.config.executor.clone(),
                    )
                    .with_meta_cb(meta_cb)
                    .with_timeout(self.config.prompt_timeout_secs);

                    if let Some(sink) = &self.event_sink {
                        conn = conn.with_event_sink(sink.clone());
                    }

                    let info = ConnectionInfo {
                        id: conn_id.clone(),
                        client_name: None,
                        client_title: None,
                        session_id: None,
                        is_cherry: false,
                    };
                    lock_recover(&self.connections).insert(conn_id.clone(), info);

                    let conns = self.connections.clone();

                    tokio::spawn(async move {
                        let _guard = ConnectionCleanupGuard {
                            conns: conns.clone(),
                            conn_id: conn_id_cb.clone(),
                        };
                        conn.run(stream).await;
                        debug!(conn_id = %conn_id_cb, "connection task finished");
                    });
                }
                Err(e) => {
                    if self.shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    error!(error = %e, "accept error");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }

        lock_recover(&self.connections).clear();
        #[cfg(unix)]
        if self.config.pipe_path.starts_with('/') {
            let _ = std::fs::remove_file(&self.config.pipe_path);
        }

        info!("ACP server stopped");
        Ok(())
    }

    /// Request graceful shutdown.
    ///
    /// **Panics** if called outside a tokio runtime (uses `tokio::spawn`).
    pub fn shutdown(&self) {
        if self.shutdown.swap(true, Ordering::Release) {
            return;
        }
        info!("ACP server shutdown requested");

        self.shutdown_notify.notify_waiters();
        let pipe_path = self.config.pipe_path.clone();
        tokio::spawn(async move {
            platform::poke_listener(&pipe_path).await;
        });
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    pub fn max_connections(&self) -> usize {
        self.config.max_connections
    }

    pub fn connection_count(&self) -> usize {
        lock_recover(&self.connections).len()
    }

    pub fn has_cherry_client(&self) -> bool {
        lock_recover(&self.connections)
            .values()
            .any(|c| c.is_cherry)
    }

    pub fn connections(&self) -> Vec<ConnectionInfo> {
        lock_recover(&self.connections).values().cloned().collect()
    }

    pub fn endpoint(&self) -> &str {
        &self.config.pipe_path
    }
}
