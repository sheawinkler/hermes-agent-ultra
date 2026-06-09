//! Cross-platform IPC transport abstraction.
//!
//! Windows: Named Pipe (\\.\pipe\AIPC-acp)
//! Unix: Unix Domain Socket (/tmp/hermes-acp.sock)

mod mod_impl;

pub use mod_impl::*;
