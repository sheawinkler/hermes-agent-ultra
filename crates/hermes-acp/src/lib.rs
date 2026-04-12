//! Agent Communication Protocol (ACP) adapter.
//!
//! Implements the ACP JSON-RPC interface so that Hermes can be controlled
//! by external agent orchestrators.

pub mod server;
pub mod protocol;
pub mod handler;

pub use server::AcpServer;
pub use protocol::{AcpRequest, AcpResponse, AcpMethod};
pub use handler::AcpHandler;
