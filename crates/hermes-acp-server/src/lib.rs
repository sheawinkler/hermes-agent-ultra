//! hermes-acp-server -- Standalone ACP Agent Server with cross-platform IPC transport.
//!
//! Provides an ACP (Agent Client Protocol) server that listens on a Named Pipe (Windows)
//! or Unix Domain Socket (Linux/macOS) and serves ACP clients like AI_Router (Cherry).

pub mod connection;
pub mod event_bridge;
pub mod executor;
pub mod ndjson;
pub mod platform;
pub mod server;
pub mod session;

pub use connection::{AcpConnection, AgentInfo, ConnectionMetaCb};
pub use executor::{PromptExecutor, PromptResult, StreamContent, StreamEvent};
pub use platform::default_pipe_path;
pub use server::{
    AcpPipeServer, AcpServerConfig, AcpServerError, AcpServerEvent, AcpServerEventSink,
    ConnectionInfo,
};
pub use session::{MetaUpdate, PipeSession};
