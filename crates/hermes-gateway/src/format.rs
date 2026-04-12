//! Cross-platform message format conversion.
//!
//! Re-exports the format conversion functions from `markdown_split` for
//! convenient access. Each function converts standard Markdown to a
//! platform-specific format.

pub use crate::markdown_split::{
    split_markdown, strip_markdown, to_discord_markdown, to_slack_mrkdwn, to_telegram_markdown_v2,
};
