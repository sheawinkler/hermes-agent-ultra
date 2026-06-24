//! Real-time voice dialog engine for Hermes (`hermes talk`).

#![allow(
    clippy::too_many_arguments,
    clippy::collapsible_if,
    clippy::should_implement_trait,
    clippy::needless_question_mark,
    clippy::manual_retain,
    clippy::unnecessary_map_or,
    clippy::needless_borrow,
    clippy::iter_cloned_collect,
    clippy::cast_lossless,
    clippy::manual_is_multiple_of,
    clippy::if_same_then_else,
    clippy::match_same_arms,
    clippy::unnecessary_cast,
    clippy::collapsible_match
)]

pub mod aec;
pub mod asr;
pub mod audio;
pub mod backends;
pub mod config;
pub mod dashscope;
pub mod denoise;
pub mod embed;
pub mod enroll;
pub mod error;
pub mod init;
pub mod kws;
pub mod llm;
pub mod orchestrator;
pub mod session;
pub mod speaker;
pub mod tools;
pub mod tts;
pub mod vad;

pub use config::Config;
pub use enroll::run_enroll;
pub use error::{DemoError, Result};
pub use init::{ensure_talk_home, init_talk_home};
pub use session::Session;
pub use tools::hermes_queue::{HermesMessage, HermesWorkItem, TalkPushBridge};
