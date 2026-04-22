use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;

use crate::errors::{AgentError, GatewayError, ToolError};
use crate::tool_schema::ToolSchema;
use crate::types::{CommandOutput, LlmResponse, Skill, SkillMeta, StreamChunk};

// ---------------------------------------------------------------------------
// LlmProvider
// ---------------------------------------------------------------------------

/// Trait for LLM provider backends.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Perform a single chat completion request.
    async fn chat_completion(
        &self,
        messages: &[crate::types::Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError>;

    /// Perform a streaming chat completion, returning a stream of chunks.
    fn chat_completion_stream(
        &self,
        messages: &[crate::types::Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>>;
}

// ---------------------------------------------------------------------------
// ToolHandler
// ---------------------------------------------------------------------------

/// Trait for tool execution handlers.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Execute the tool with the given parameters.
    async fn execute(&self, params: Value) -> Result<String, ToolError>;

    /// Return the schema describing this tool's parameters.
    fn schema(&self) -> ToolSchema;
}

// ---------------------------------------------------------------------------
// PlatformAdapter
// ---------------------------------------------------------------------------

/// Parse mode for platform messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseMode {
    Plain,
    Markdown,
    Html,
}

/// Trait for platform communication adapters (Telegram, Discord, etc.).
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// Start the platform adapter (connect, begin listening).
    async fn start(&self) -> Result<(), GatewayError>;

    /// Stop the platform adapter gracefully.
    async fn stop(&self) -> Result<(), GatewayError>;

    /// Send a text message to a chat.
    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError>;

    /// Edit an existing message.
    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError>;

    /// Send a file to a chat with an optional caption.
    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError>;

    /// Send an image by URL using native platform capabilities when possible.
    ///
    /// Default behavior mirrors Python parity fallback: send as plain text
    /// (optionally prefixed by caption) when the platform has no native URL
    /// image path.
    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let content = match caption.map(str::trim).filter(|s| !s.is_empty()) {
            Some(c) => format!("{c}\n{image_url}"),
            None => image_url.to_string(),
        };
        self.send_message(chat_id, &content, Some(ParseMode::Plain))
            .await
    }

    /// Check whether the adapter is currently running.
    fn is_running(&self) -> bool;

    /// Return the name of this platform (e.g. "telegram", "discord").
    fn platform_name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// TerminalBackend
// ---------------------------------------------------------------------------

/// Trait for terminal / shell backend implementations.
#[async_trait]
pub trait TerminalBackend: Send + Sync {
    /// Execute a command in the terminal.
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError>;

    /// Execute a command with optional stdin payload piping support.
    ///
    /// Backends that do not support stdin piping can keep the default
    /// behavior, which rejects non-empty `stdin_data`.
    async fn execute_command_with_stdin(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
        stdin_data: Option<&str>,
    ) -> Result<CommandOutput, AgentError> {
        if stdin_data.is_some() {
            return Err(AgentError::ToolExecution(
                "stdin_data is not supported by this terminal backend".into(),
            ));
        }
        self.execute_command(command, timeout, workdir, background, pty)
            .await
    }

    /// Read a file's contents (with optional offset and line limit).
    async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError>;

    /// Write content to a file.
    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError>;

    /// Check whether a file exists at the given path.
    async fn file_exists(&self, path: &str) -> Result<bool, AgentError>;

    /// List tracked background process sessions.
    async fn list_processes(&self) -> Result<Value, AgentError> {
        Err(AgentError::ToolExecution(
            "Background process management is not supported by this terminal backend".into(),
        ))
    }

    /// Poll status/output preview for a tracked background process session.
    async fn poll_process(&self, _session_id: &str) -> Result<Value, AgentError> {
        Err(AgentError::ToolExecution(
            "Background process polling is not supported by this terminal backend".into(),
        ))
    }

    /// Read output log lines for a tracked background process session.
    async fn read_process_log(
        &self,
        _session_id: &str,
        _offset: Option<u64>,
        _limit: Option<u64>,
    ) -> Result<Value, AgentError> {
        Err(AgentError::ToolExecution(
            "Background process log reads are not supported by this terminal backend".into(),
        ))
    }

    /// Wait for a tracked background process session to finish.
    async fn wait_process(
        &self,
        _session_id: &str,
        _timeout: Option<u64>,
    ) -> Result<Value, AgentError> {
        Err(AgentError::ToolExecution(
            "Background process waiting is not supported by this terminal backend".into(),
        ))
    }

    /// Terminate a tracked background process session.
    async fn kill_process(&self, _session_id: &str) -> Result<Value, AgentError> {
        Err(AgentError::ToolExecution(
            "Background process termination is not supported by this terminal backend".into(),
        ))
    }

    /// Write raw bytes to background process stdin.
    async fn write_process_stdin(
        &self,
        _session_id: &str,
        _data: &str,
    ) -> Result<Value, AgentError> {
        Err(AgentError::ToolExecution(
            "Background process stdin writes are not supported by this terminal backend".into(),
        ))
    }

    /// Submit stdin plus newline to a background process.
    async fn submit_process_stdin(
        &self,
        session_id: &str,
        data: &str,
    ) -> Result<Value, AgentError> {
        self.write_process_stdin(session_id, &format!("{data}\n"))
            .await
    }

    /// Close stdin for a background process.
    async fn close_process_stdin(&self, _session_id: &str) -> Result<Value, AgentError> {
        Err(AgentError::ToolExecution(
            "Background process stdin close is not supported by this terminal backend".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// MemoryProvider
// ---------------------------------------------------------------------------

/// Trait for key-value memory storage backends.
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    /// Save a value under a namespace + key.
    async fn save(&self, namespace: &str, key: &str, value: &str) -> Result<(), AgentError>;

    /// Load a value from a namespace + key.
    async fn load(&self, namespace: &str, key: &str) -> Result<Option<String>, AgentError>;

    /// List all namespaces.
    async fn list_namespaces(&self) -> Result<Vec<String>, AgentError>;

    /// Delete a value from a namespace + key.
    async fn delete(&self, namespace: &str, key: &str) -> Result<(), AgentError>;
}

// ---------------------------------------------------------------------------
// SkillProvider
// ---------------------------------------------------------------------------

/// Trait for skill management backends.
#[async_trait]
pub trait SkillProvider: Send + Sync {
    /// Create a new skill.
    async fn create_skill(
        &self,
        name: &str,
        content: &str,
        category: Option<&str>,
    ) -> Result<Skill, AgentError>;

    /// Get a skill by name.
    async fn get_skill(&self, name: &str) -> Result<Option<Skill>, AgentError>;

    /// List all skills with metadata.
    async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError>;

    /// Update an existing skill's content.
    async fn update_skill(&self, name: &str, content: &str) -> Result<Skill, AgentError>;

    /// Delete a skill by name.
    async fn delete_skill(&self, name: &str) -> Result<(), AgentError>;
}
