//! Prompt builder — constructs system prompts incorporating personality, skills,
//! tools, context files, memory, and user profile. Also adds cache markers.
//!
//! Requirements 16.6–16.7

use hermes_core::types::{CacheControl, CacheType, Message};
use hermes_core::ToolSchema;

// ---------------------------------------------------------------------------
// PromptBuilder
// ---------------------------------------------------------------------------

/// Builder for constructing rich system prompts for the agent.
#[derive(Debug, Clone)]
pub struct PromptBuilder {
    /// Optional personality/SOUL.md content.
    pub personality: Option<String>,
    /// Skills the agent has access to.
    pub skills: Vec<String>,
    /// Tool schemas describing available tools.
    pub tools: Vec<ToolSchema>,
    /// Context files that have been loaded.
    pub context_files: Vec<String>,
    /// Optional memory content (long-term memory).
    pub memory: Option<String>,
    /// Optional user profile information.
    pub user_profile: Option<String>,
}

impl PromptBuilder {
    /// Create a new empty prompt builder.
    pub fn new() -> Self {
        Self {
            personality: None,
            skills: Vec::new(),
            tools: Vec::new(),
            context_files: Vec::new(),
            memory: None,
            user_profile: None,
        }
    }

    /// Set the personality / SOUL.md content.
    pub fn personality(mut self, personality: impl Into<String>) -> Self {
        self.personality = Some(personality.into());
        self
    }

    /// Add a skill.
    pub fn skill(mut self, skill: impl Into<String>) -> Self {
        self.skills.push(skill.into());
        self
    }

    /// Set all skills at once.
    pub fn skills(mut self, skills: Vec<String>) -> Self {
        self.skills = skills;
        self
    }

    /// Add a tool schema.
    pub fn tool(mut self, tool: ToolSchema) -> Self {
        self.tools.push(tool);
        self
    }

    /// Set all tools at once.
    pub fn tools(mut self, tools: Vec<ToolSchema>) -> Self {
        self.tools = tools;
        self
    }

    /// Add a context file.
    pub fn context_file(mut self, file: impl Into<String>) -> Self {
        self.context_files.push(file.into());
        self
    }

    /// Set the memory content.
    pub fn memory(mut self, memory: impl Into<String>) -> Self {
        self.memory = Some(memory.into());
        self
    }

    /// Set the user profile.
    pub fn user_profile(mut self, profile: impl Into<String>) -> Self {
        self.user_profile = Some(profile.into());
        self
    }

    /// Build the complete system prompt string.
    ///
    /// The prompt is assembled in this order:
    /// 1. Personality / SOUL.md
    /// 2. Skills list
    /// 3. Tool descriptions
    /// 4. Context files
    /// 5. Memory
    /// 6. User profile
    pub fn build_system_prompt(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        // 1. Personality / SOUL.md
        if let Some(personality) = &self.personality {
            parts.push(format!("## Personality\n\n{}", personality));
        }

        // 2. Skills
        if !self.skills.is_empty() {
            let skill_list = self
                .skills
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{}. {}", i + 1, s))
                .collect::<Vec<_>>()
                .join("\n");
            parts.push(format!("## Skills\n\n{}", skill_list));
        }

        // 3. Tools
        if !self.tools.is_empty() {
            let tool_descriptions = self
                .tools
                .iter()
                .map(|t| {
                    let params_desc = if t.parameters.required.is_some() {
                        let required = t.parameters.required.as_ref().unwrap();
                        format!(" (required: {})", required.join(", "))
                    } else {
                        String::new()
                    };
                    format!("- **{}**: {}{}", t.name, t.description, params_desc)
                })
                .collect::<Vec<_>>()
                .join("\n");
            parts.push(format!("## Available Tools\n\n{}", tool_descriptions));
        }

        // 4. Context files
        if !self.context_files.is_empty() {
            let file_list = self
                .context_files
                .iter()
                .enumerate()
                .map(|(i, f)| format!("{}. `{}`", i + 1, f))
                .collect::<Vec<_>>()
                .join("\n");
            parts.push(format!("## Context Files\n\n{}", file_list));
        }

        // 5. Memory
        if let Some(memory) = &self.memory {
            parts.push(format!("## Memory\n\n{}", memory));
        }

        // 6. User profile
        if let Some(profile) = &self.user_profile {
            parts.push(format!("## User Profile\n\n{}", profile));
        }

        parts.join("\n\n")
    }

    /// Add Anthropic-style cache control markers to a message list.
    ///
    /// Strategy:
    /// - Mark the system message as `Persistent` (cache for the whole session).
    /// - Mark the last N user/assistant message turns as `Ephemeral` (short-lived cache).
    /// - The number of turns to mark is configurable via `ephemeral_turns`.
    pub fn add_cache_markers(&self, messages: &mut Vec<Message>, ephemeral_turns: usize) {
        if messages.is_empty() {
            return;
        }

        // Mark the first system message as persistent
        for msg in messages.iter_mut() {
            if msg.role == hermes_core::types::MessageRole::System {
                msg.cache_control = Some(CacheControl {
                    cache_type: CacheType::Persistent,
                });
                break;
            }
        }

        // Mark the last N user/assistant turns as ephemeral
        let mut turn_count = 0;
        for msg in messages.iter_mut().rev() {
            if turn_count >= ephemeral_turns {
                break;
            }
            if msg.role == hermes_core::types::MessageRole::User
                || msg.role == hermes_core::types::MessageRole::Assistant
            {
                msg.cache_control = Some(CacheControl {
                    cache_type: CacheType::Ephemeral,
                });
                turn_count += 1;
            }
        }
    }
}

impl Default for PromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::JsonSchema;

    #[test]
    fn test_empty_prompt() {
        let builder = PromptBuilder::new();
        let prompt = builder.build_system_prompt();
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_personality_only() {
        let builder = PromptBuilder::new().personality("You are Hermes, a helpful assistant.");
        let prompt = builder.build_system_prompt();
        assert!(prompt.contains("## Personality"));
        assert!(prompt.contains("Hermes"));
    }

    #[test]
    fn test_full_prompt() {
        let builder = PromptBuilder::new()
            .personality("You are helpful.")
            .skill("rust_expert")
            .skill("code_reviewer")
            .memory("User prefers dark mode")
            .user_profile("John, software engineer");

        let prompt = builder.build_system_prompt();
        assert!(prompt.contains("## Personality"));
        assert!(prompt.contains("## Skills"));
        assert!(prompt.contains("rust_expert"));
        assert!(prompt.contains("## Memory"));
        assert!(prompt.contains("## User Profile"));
    }

    #[test]
    fn test_tools_in_prompt() {
        let tool = ToolSchema::new(
            "read_file",
            "Read a file from disk",
            JsonSchema::new("object"),
        );
        let builder = PromptBuilder::new().tool(tool);
        let prompt = builder.build_system_prompt();
        assert!(prompt.contains("read_file"));
        assert!(prompt.contains("Read a file from disk"));
    }

    #[test]
    fn test_cache_markers() {
        let builder = PromptBuilder::new().personality("You are helpful.");

        let mut messages = vec![
            Message::system("System prompt"),
            Message::user("Hello"),
            Message::assistant("Hi there!"),
            Message::user("How are you?"),
        ];

        builder.add_cache_markers(&mut messages, 2);

        // System message should be persistent
        assert_eq!(
            messages[0].cache_control.as_ref().unwrap().cache_type,
            CacheType::Persistent
        );

        // Last 2 user/assistant turns should be ephemeral
        assert!(messages[2].cache_control.is_some()); // assistant
        assert!(messages[3].cache_control.is_some()); // user

        // First user message should NOT have cache control
        assert!(messages[1].cache_control.is_none());
    }

    #[test]
    fn test_context_files_in_prompt() {
        let builder = PromptBuilder::new()
            .context_file("src/main.rs")
            .context_file("Cargo.toml");
        let prompt = builder.build_system_prompt();
        assert!(prompt.contains("## Context Files"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("Cargo.toml"));
    }
}
