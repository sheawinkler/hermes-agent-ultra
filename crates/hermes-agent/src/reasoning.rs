//! Reasoning content parser (Requirement 2.7).
//!
//! Extracts reasoning / chain-of-thought content from LLM responses that
//! use different provider conventions:
//!
//! - `reasoning_content` (deepseek, some OpenAI-compatible APIs)
//! - `reasoning` (Anthropic-style extended thinking)
//! - `reasoning_details` (OpenRouter structured format)

use serde_json::Value;

/// Parse reasoning content from a raw LLM response JSON value.
///
/// Examines the response object for known reasoning fields and returns
/// the extracted text if any reasoning content is found.
///
/// # Fields checked (in order)
///
/// 1. `reasoning_content` — a simple string
/// 2. `reasoning` — a simple string
/// 3. `reasoning_details` — an array of `{ "type": "text", "text": "..." }` objects
/// 4. `message.reasoning_content` — nested inside a `message` object
/// 5. `message.reasoning` — nested inside a `message` object
///
/// Returns `None` if no reasoning content is found.
pub fn parse_reasoning(response: &Value) -> Option<String> {
    // Check top-level reasoning_content
    if let Some(s) = response.get("reasoning_content").and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }

    // Check top-level reasoning
    if let Some(s) = response.get("reasoning").and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }

    // Check top-level reasoning_details array
    if let Some(details) = response.get("reasoning_details").and_then(|v| v.as_array()) {
        let text = extract_reasoning_details(details);
        if !text.is_empty() {
            return Some(text);
        }
    }

    // Check nested inside message object
    if let Some(message) = response.get("message") {
        if let Some(s) = message.get("reasoning_content").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }

        if let Some(s) = message.get("reasoning").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }

        if let Some(details) = message.get("reasoning_details").and_then(|v| v.as_array()) {
            let text = extract_reasoning_details(details);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }

    // Check choices[0].message for OpenAI-style responses
    if let Some(choices) = response.get("choices").and_then(|v| v.as_array()) {
        if let Some(choice) = choices.first() {
            if let Some(message) = choice.get("message") {
                if let Some(s) = message.get("reasoning_content").and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }

                if let Some(s) = message.get("reasoning").and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }

                if let Some(details) = message.get("reasoning_details").and_then(|v| v.as_array())
                {
                    let text = extract_reasoning_details(details);
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
        }
    }

    None
}

/// Extract reasoning text from a `reasoning_details` array.
///
/// Each element may be:
/// - `{ "type": "text", "text": "..." }` (standard OpenRouter format)
/// - `{ "type": "thinking", "thinking": "..." }` (thinking blocks)
/// - `{ "text": "..." }` (simplified format)
///
/// All recognized text fields are concatenated with newlines.
pub fn extract_reasoning_details(details: &[Value]) -> String {
    let mut parts = Vec::new();

    for item in details {
        // Standard: { "type": "text", "text": "..." }
        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
            if !text.is_empty() {
                parts.push(text.to_string());
                continue;
            }
        }

        // Thinking blocks: { "type": "thinking", "thinking": "..." }
        if let Some(thinking) = item.get("thinking").and_then(|v| v.as_str()) {
            if !thinking.is_empty() {
                parts.push(thinking.to_string());
                continue;
            }
        }

        // Fallback: try "content" field
        if let Some(content) = item.get("content").and_then(|v| v.as_str()) {
            if !content.is_empty() {
                parts.push(content.to_string());
            }
        }
    }

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_reasoning_content_field() {
        let response = json!({
            "reasoning_content": "I need to think about this step by step.",
            "content": "The answer is 42"
        });
        let reasoning = parse_reasoning(&response).unwrap();
        assert!(reasoning.contains("step by step"));
    }

    #[test]
    fn test_parse_reasoning_field() {
        let response = json!({
            "reasoning": "Let me work through this logically.",
            "content": "Final answer"
        });
        let reasoning = parse_reasoning(&response).unwrap();
        assert!(reasoning.contains("logically"));
    }

    #[test]
    fn test_parse_reasoning_details() {
        let response = json!({
            "reasoning_details": [
                { "type": "text", "text": "Step 1: Analyze the problem" },
                { "type": "text", "text": "Step 2: Find the solution" }
            ]
        });
        let reasoning = parse_reasoning(&response).unwrap();
        assert!(reasoning.contains("Step 1"));
        assert!(reasoning.contains("Step 2"));
    }

    #[test]
    fn test_parse_reasoning_nested_in_message() {
        let response = json!({
            "message": {
                "reasoning_content": "Thinking deeply...",
                "content": "Result"
            }
        });
        let reasoning = parse_reasoning(&response).unwrap();
        assert!(reasoning.contains("Thinking deeply"));
    }

    #[test]
    fn test_parse_reasoning_openai_choices() {
        let response = json!({
            "choices": [{
                "message": {
                    "reasoning_content": "Chain of thought here",
                    "content": "Answer"
                }
            }]
        });
        let reasoning = parse_reasoning(&response).unwrap();
        assert!(reasoning.contains("Chain of thought"));
    }

    #[test]
    fn test_parse_no_reasoning() {
        let response = json!({
            "content": "Just a plain response"
        });
        assert!(parse_reasoning(&response).is_none());
    }

    #[test]
    fn test_parse_empty_reasoning() {
        let response = json!({
            "reasoning_content": "",
            "content": "No reasoning"
        });
        assert!(parse_reasoning(&response).is_none());
    }

    #[test]
    fn test_extract_reasoning_details_thinking() {
        let details = vec![
            json!({ "type": "thinking", "thinking": "Hmm, let me consider..." }),
            json!({ "type": "text", "text": "Conclusion reached" }),
        ];
        let text = extract_reasoning_details(&details);
        assert!(text.contains("Hmm"));
        assert!(text.contains("Conclusion"));
    }

    #[test]
    fn test_parse_reasoning_precedence() {
        // reasoning_content takes precedence over reasoning
        let response = json!({
            "reasoning_content": "Primary",
            "reasoning": "Secondary"
        });
        let reasoning = parse_reasoning(&response).unwrap();
        assert_eq!(reasoning, "Primary");
    }
}