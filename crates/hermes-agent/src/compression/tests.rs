#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{FunctionCall, ToolCall};
    use hermes_intelligence::auxiliary::{AuxiliarySource, ProviderCandidate};

    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use hermes_core::{LlmProvider, LlmResponse, StreamChunk, ToolSchema, UsageStats};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    // ---------- helpers ----------

    fn msg(role: MessageRole, content: &str) -> Message {
        Message {
            role,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            anthropic_content_blocks: None,
            cache_control: None,
        }
    }

    fn tool_msg(call_id: &str, content: &str) -> Message {
        Message {
            role: MessageRole::Tool,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            name: None,
            reasoning_content: None,
            anthropic_content_blocks: None,
            cache_control: None,
        }
    }

    fn assistant_with_tool_call(call_id: &str, name: &str, args: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: Some(String::new()),
            tool_calls: Some(vec![ToolCall {
                id: call_id.into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: args.into(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            anthropic_content_blocks: None,
            cache_control: None,
        }
    }

    /// LLM provider that returns a canned summary regardless of input.
    struct CannedSummaryProvider {
        canned: String,
        calls: Mutex<usize>,
        prompts: Mutex<Vec<String>>,
    }

    impl CannedSummaryProvider {
        fn new(canned: impl Into<String>) -> Arc<Self> {
            Arc::new(Self {
                canned: canned.into(),
                calls: Mutex::new(0),
                prompts: Mutex::new(Vec::new()),
            })
        }
        fn call_count(&self) -> usize {
            *self.calls.lock().unwrap()
        }
        fn last_prompt(&self) -> Option<String> {
            self.prompts.lock().unwrap().last().cloned()
        }
    }

    #[async_trait]
    impl LlmProvider for CannedSummaryProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            *self.calls.lock().unwrap() += 1;
            self.prompts.lock().unwrap().push(
                messages
                    .first()
                    .and_then(|message| message.content.clone())
                    .unwrap_or_default(),
            );
            Ok(LlmResponse {
                message: Message {
                    role: MessageRole::Assistant,
                    content: Some(self.canned.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                    anthropic_content_blocks: None,
                    cache_control: None,
                },
                finish_reason: Some("stop".into()),
                model: model.unwrap_or("test").to_string(),
                usage: Some(UsageStats {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    estimated_cost: None,
                }),
            })
        }
        fn chat_completion_stream(
            &self,
            _m: &[Message],
            _t: &[ToolSchema],
            _x: Option<u32>,
            _temp: Option<f64>,
            _model: Option<&str>,
            _eb: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            Box::pin(futures::stream::empty())
        }
    }

    /// LLM provider that always returns 402 — exercises the cooldown path.
    struct FailingProvider;
    #[async_trait]
    impl LlmProvider for FailingProvider {
        async fn chat_completion(
            &self,
            _m: &[Message],
            _t: &[ToolSchema],
            _x: Option<u32>,
            _temp: Option<f64>,
            _model: Option<&str>,
            _eb: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Err(AgentError::LlmApi("HTTP 402: insufficient credits".into()))
        }
        fn chat_completion_stream(
            &self,
            _m: &[Message],
            _t: &[ToolSchema],
            _x: Option<u32>,
            _temp: Option<f64>,
            _model: Option<&str>,
            _eb: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            Box::pin(futures::stream::empty())
        }
    }

    /// LLM provider that returns a scripted sequence of outcomes.
    struct SequencedProvider {
        outcomes: Mutex<VecDeque<Result<String, AgentError>>>,
        calls: Mutex<usize>,
    }

    impl SequencedProvider {
        fn new(outcomes: Vec<Result<String, AgentError>>) -> Arc<Self> {
            Arc::new(Self {
                outcomes: Mutex::new(VecDeque::from(outcomes)),
                calls: Mutex::new(0),
            })
        }

        fn call_count(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl LlmProvider for SequencedProvider {
        async fn chat_completion(
            &self,
            _m: &[Message],
            _t: &[ToolSchema],
            _x: Option<u32>,
            _temp: Option<f64>,
            model: Option<&str>,
            _eb: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            *self.calls.lock().unwrap() += 1;
            let outcome = self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(AgentError::LlmApi("sequenced provider exhausted".into())));
            match outcome {
                Ok(text) => Ok(LlmResponse {
                    message: Message {
                        role: MessageRole::Assistant,
                        content: Some(text),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                        reasoning_content: None,
                        anthropic_content_blocks: None,
                        cache_control: None,
                    },
                    finish_reason: Some("stop".into()),
                    model: model.unwrap_or("test").to_string(),
                    usage: Some(UsageStats {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        total_tokens: 2,
                        estimated_cost: None,
                    }),
                }),
                Err(err) => Err(err),
            }
        }
        fn chat_completion_stream(
            &self,
            _m: &[Message],
            _t: &[ToolSchema],
            _x: Option<u32>,
            _temp: Option<f64>,
            _model: Option<&str>,
            _eb: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            Box::pin(futures::stream::empty())
        }
    }

    fn aux_with_provider(provider: Arc<dyn LlmProvider>) -> Arc<AuxiliaryClient> {
        Arc::new(
            AuxiliaryClient::builder()
                .add_candidate(ProviderCandidate::new(
                    AuxiliarySource::Custom,
                    "test-model",
                    provider,
                ))
                .build(),
        )
    }

    fn quiet_config() -> CompressorConfig {
        CompressorConfig {
            quiet_mode: true,
            ..CompressorConfig::default()
        }
    }

    // ---------- prefix normalisation ----------

    #[test]
    fn with_summary_prefix_strips_legacy_prefix() {
        let s = format!("{LEGACY_SUMMARY_PREFIX} hello");
        let out = with_summary_prefix(&s);
        assert!(out.starts_with(SUMMARY_PREFIX));
        assert!(out.ends_with("hello"));
        assert!(!out.contains(LEGACY_SUMMARY_PREFIX));
    }

    #[test]
    fn with_summary_prefix_does_not_double_prefix() {
        let s = format!("{SUMMARY_PREFIX}\nbody");
        let out = with_summary_prefix(&s);
        assert_eq!(out.matches(SUMMARY_PREFIX).count(), 1);
        assert!(out.ends_with("body"));
    }

    #[test]
    fn with_summary_prefix_handles_empty_string() {
        let out = with_summary_prefix("");
        assert_eq!(out, SUMMARY_PREFIX);
    }

    #[test]
    fn redact_sensitive_summary_text_masks_common_secrets() {
        let raw = "api_key=sk-abc123456789\nAuthorization: Bearer tok_super_secret_123456\n-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----";
        let redacted = redact_sensitive_summary_text(raw);
        assert!(!redacted.contains("sk-abc123456789"));
        assert!(!redacted.contains("tok_super_secret_123456"));
        assert!(!redacted.contains("BEGIN PRIVATE KEY"));
        assert!(redacted.contains("[redacted]"));
    }

    // ---------- threshold + budget ----------

    #[test]
    fn should_compress_respects_threshold() {
        let cfg = CompressorConfig {
            context_length: 100_000,
            threshold_percent: 0.5,
            ..quiet_config()
        };
        let compressor =
            ContextCompressor::new(cfg, aux_with_provider(CannedSummaryProvider::new("x")));
        assert!(!compressor.should_compress(Some(40_000)));
        assert!(compressor.should_compress(Some(50_000)));
        assert!(compressor.should_compress(Some(60_000)));
    }

    #[test]
    fn should_compress_uses_prompt_tokens_only() {
        let cfg = CompressorConfig {
            context_length: 200_000,
            threshold_percent: 0.5,
            ..quiet_config()
        };
        let mut compressor =
            ContextCompressor::new(cfg, aux_with_provider(CannedSummaryProvider::new("x")));

        compressor.update_from_usage(40_000);
        assert!(!compressor.should_compress(None));
        assert!(!compressor.should_compress(Some(40_000)));
        assert!(compressor.should_compress(Some(110_000)));
    }

    #[test]
    fn constructor_with_logging_enabled_is_reentrant() {
        let cfg = CompressorConfig {
            quiet_mode: false,
            ..CompressorConfig::default()
        };
        let aux = aux_with_provider(CannedSummaryProvider::new("x"));

        let first = ContextCompressor::new(cfg.clone(), aux.clone());
        let second = ContextCompressor::new(cfg, aux);

        assert_eq!(first.compression_count(), 0);
        assert_eq!(second.compression_count(), 0);
    }

    #[test]
    fn budget_clamped_between_min_and_ceiling() {
        let cfg = CompressorConfig {
            context_length: 1_000_000,
            ..quiet_config()
        };
        let compressor =
            ContextCompressor::new(cfg, aux_with_provider(CannedSummaryProvider::new("x")));
        let big = vec![msg(MessageRole::User, &"x".repeat(10_000_000))];
        let budget = compressor.compute_summary_budget(&big);
        assert!(budget <= SUMMARY_TOKENS_CEILING);
        assert!(budget >= MIN_SUMMARY_TOKENS);
    }

    // ---------- tool-output pruning ----------

    #[test]
    fn prune_replaces_old_oversized_tool_outputs_only() {
        let cfg = quiet_config();
        let compressor =
            ContextCompressor::new(cfg, aux_with_provider(CannedSummaryProvider::new("x")));

        let mut messages = vec![
            msg(MessageRole::System, "sys"),
            msg(MessageRole::User, "hi"),
            assistant_with_tool_call("c1", "shell", "ls"),
            tool_msg("c1", &"a".repeat(500)), // old & big — should prune
            assistant_with_tool_call("c2", "shell", "pwd"),
            tool_msg("c2", "tiny"), // old but small — keep
            assistant_with_tool_call("c3", "shell", "echo"),
            tool_msg("c3", &"b".repeat(800)), // recent — keep
        ];
        // Add tail messages to push the first three tool calls out of the
        // protected zone.
        for i in 0..40 {
            messages.push(msg(MessageRole::User, &format!("t{i}")));
        }

        let (out, pruned) = compressor.prune_old_tool_results(&messages, 20, None);
        assert!(pruned >= 1);
        assert_eq!(
            out[3].content.as_deref(),
            Some(PRUNED_TOOL_PLACEHOLDER),
            "first oversized tool output should be pruned"
        );
        assert_eq!(
            out[5].content.as_deref(),
            Some("tiny"),
            "small tool output kept verbatim"
        );
    }

    // ---------- serialisation ----------

    #[test]
    fn serialize_includes_tool_call_arguments() {
        let cfg = quiet_config();
        let compressor =
            ContextCompressor::new(cfg, aux_with_provider(CannedSummaryProvider::new("x")));
        let turns = vec![
            msg(MessageRole::User, "do thing"),
            assistant_with_tool_call("c1", "fs_write", r#"{"path":"a.txt"}"#),
            tool_msg("c1", "ok"),
        ];
        let block = compressor.serialize_for_summary(&turns);
        assert!(block.contains("[USER]: do thing"));
        assert!(block.contains("fs_write"));
        assert!(block.contains("a.txt"));
        assert!(block.contains("[TOOL RESULT c1]: ok"));
    }

    #[test]
    fn serialize_truncates_oversized_content() {
        let cfg = quiet_config();
        let compressor =
            ContextCompressor::new(cfg, aux_with_provider(CannedSummaryProvider::new("x")));
        let huge = "x".repeat(20_000);
        let turns = vec![msg(MessageRole::User, &huge)];
        let block = compressor.serialize_for_summary(&turns);
        assert!(block.contains("...[truncated]..."));
        assert!(block.len() < huge.len());
    }

    // ---------- generate_summary ----------

    #[tokio::test]
    async fn generate_summary_succeeds_and_records_iteration() {
        let provider = CannedSummaryProvider::new("Goal: build it.");
        let aux = aux_with_provider(provider.clone());
        let mut compressor = ContextCompressor::new(quiet_config(), aux);

        let turns = vec![msg(MessageRole::User, "first turn")];
        let s1 = compressor.generate_summary(&turns).await.unwrap().unwrap();
        assert!(s1.starts_with(SUMMARY_PREFIX));
        assert!(s1.ends_with("Goal: build it."));
        assert_eq!(provider.call_count(), 1);

        // Second call should reuse the previous summary as context.
        let s2 = compressor.generate_summary(&turns).await.unwrap().unwrap();
        assert!(s2.contains("Goal: build it."));
        assert_eq!(provider.call_count(), 2);
    }

    fn messages_with_handoff(summary_body: &str) -> Vec<Message> {
        vec![
            msg(MessageRole::System, "system prompt"),
            msg(
                MessageRole::User,
                &format!("{SUMMARY_PREFIX}\n{summary_body}"),
            ),
            msg(MessageRole::Assistant, "handoff acknowledged after resume"),
            msg(MessageRole::User, "new user turn after resume"),
            msg(MessageRole::Assistant, "new assistant work after resume"),
            msg(MessageRole::User, "more new work after resume"),
            msg(MessageRole::Assistant, "latest tail response"),
            msg(
                MessageRole::User,
                "final active request stays in protected tail",
            ),
        ]
    }

    #[tokio::test]
    async fn compress_rehydrates_previous_summary_from_handoff_message() {
        let old_summary = "RESUMED-SUMMARY-BODY durable continuity facts";
        let provider = CannedSummaryProvider::new("updated summary");
        let aux = aux_with_provider(provider.clone());
        let cfg = CompressorConfig {
            context_length: 1_000,
            threshold_percent: 0.10,
            protect_first_n: 1,
            protect_last_n: 1,
            ..quiet_config()
        };
        let mut compressor = ContextCompressor::new(cfg, aux);

        let _ = compressor
            .compress(messages_with_handoff(old_summary), Some(150))
            .await;

        let prompt = provider.last_prompt().expect("summary prompt");
        assert!(prompt.contains("PREVIOUS SUMMARY:"));
        assert!(prompt.contains("NEW TURNS TO INCORPORATE:"));
        assert!(!prompt.contains("TURNS TO SUMMARIZE:"));
        assert_eq!(prompt.matches(old_summary).count(), 1);
        assert!(!prompt.contains(&format!("[USER]: {SUMMARY_PREFIX}")));
    }

    #[tokio::test]
    async fn compress_does_not_serialize_existing_handoff_twice() {
        let old_summary = "OLD-SUMMARY-BODY unique continuity facts";
        let provider = CannedSummaryProvider::new("updated summary");
        let aux = aux_with_provider(provider.clone());
        let cfg = CompressorConfig {
            context_length: 1_000,
            threshold_percent: 0.10,
            protect_first_n: 1,
            protect_last_n: 1,
            ..quiet_config()
        };
        let mut compressor = ContextCompressor::new(cfg, aux);
        compressor.previous_summary = Some(old_summary.to_string());

        let _ = compressor
            .compress(messages_with_handoff(old_summary), Some(150))
            .await;

        let prompt = provider.last_prompt().expect("summary prompt");
        assert!(prompt.contains("PREVIOUS SUMMARY:"));
        assert!(prompt.contains("NEW TURNS TO INCORPORATE:"));
        assert_eq!(prompt.matches(old_summary).count(), 1);
        assert!(!prompt.contains(&format!("[USER]: {SUMMARY_PREFIX}")));
    }

    #[tokio::test]
    async fn protected_head_handoff_rehydrates_previous_summary() {
        let old_summary = "PROTECTED-HEAD-SUMMARY durable facts from before restart";
        let provider = CannedSummaryProvider::new("updated summary");
        let aux = aux_with_provider(provider.clone());
        let cfg = CompressorConfig {
            context_length: 1_000,
            threshold_percent: 0.10,
            protect_first_n: 2,
            protect_last_n: 1,
            ..quiet_config()
        };
        let mut compressor = ContextCompressor::new(cfg, aux);

        let _ = compressor
            .compress(messages_with_handoff(old_summary), Some(150))
            .await;

        let prompt = provider.last_prompt().expect("summary prompt");
        assert!(prompt.contains("PREVIOUS SUMMARY:"));
        assert!(prompt.contains("NEW TURNS TO INCORPORATE:"));
        assert_eq!(prompt.matches(old_summary).count(), 1);
        assert!(!prompt.contains(&format!("[USER]: {SUMMARY_PREFIX}")));
    }

    #[tokio::test]
    async fn generate_summary_arms_cooldown_on_failure() {
        let aux = aux_with_provider(Arc::new(FailingProvider));
        let mut compressor = ContextCompressor::new(quiet_config(), aux);
        let turns = vec![msg(MessageRole::User, "x")];
        let err = compressor.generate_summary(&turns).await.unwrap_err();
        assert!(matches!(err, CompressionError::Auxiliary(_)));
        // Second call within cooldown window short-circuits.
        let err2 = compressor.generate_summary(&turns).await.unwrap_err();
        assert!(matches!(err2, CompressionError::CooldownActive(_)));
    }

    #[tokio::test]
    async fn generate_summary_retries_once_on_main_after_aux_failure() {
        let provider = SequencedProvider::new(vec![
            Err(AgentError::LlmApi(
                "HTTP 400: provider rejected model".into(),
            )),
            Ok("summary via main model".to_string()),
        ]);
        let aux = aux_with_provider(provider.clone());
        let mut cfg = quiet_config();
        cfg.summary_model_override = Some("broken-aux-model".to_string());
        let mut compressor = ContextCompressor::new(cfg, aux);
        let turns = vec![msg(MessageRole::User, "x")];

        let out = compressor.generate_summary(&turns).await.unwrap();
        assert!(out.unwrap().contains("summary via main model"));
        assert_eq!(provider.call_count(), 2);
        assert_eq!(compressor.last_summary_error(), None);
    }

    #[tokio::test]
    async fn generate_summary_with_no_aux_override_does_not_retry() {
        let provider = SequencedProvider::new(vec![
            Err(AgentError::LlmApi(
                "HTTP 400: provider rejected model".into(),
            )),
            Ok("should not be reached".to_string()),
        ]);
        let aux = aux_with_provider(provider.clone());
        let mut compressor = ContextCompressor::new(quiet_config(), aux);
        let turns = vec![msg(MessageRole::User, "x")];

        let err = compressor.generate_summary(&turns).await.unwrap_err();
        assert!(matches!(err, CompressionError::Auxiliary(_)));
        assert_eq!(provider.call_count(), 1);
        assert!(compressor.last_summary_error().is_some());
    }

    #[tokio::test]
    async fn generate_summary_only_retries_once_when_both_attempts_fail() {
        let provider = SequencedProvider::new(vec![
            Err(AgentError::LlmApi("HTTP 404: model_not_found".into())),
            Err(AgentError::LlmApi("HTTP 500: upstream exploded".into())),
        ]);
        let aux = aux_with_provider(provider.clone());
        let mut cfg = quiet_config();
        cfg.summary_model_override = Some("broken-aux-model".to_string());
        let mut compressor = ContextCompressor::new(cfg, aux);
        let turns = vec![msg(MessageRole::User, "x")];

        let err = compressor.generate_summary(&turns).await.unwrap_err();
        assert!(matches!(err, CompressionError::Auxiliary(_)));
        assert_eq!(provider.call_count(), 2);
        assert!(compressor.last_summary_error().is_some());
    }

    // ---------- sanitiser ----------

    #[test]
    fn sanitiser_removes_orphaned_tool_results() {
        let cfg = quiet_config();
        let compressor =
            ContextCompressor::new(cfg, aux_with_provider(CannedSummaryProvider::new("x")));
        let messages = vec![
            msg(MessageRole::System, "sys"),
            msg(MessageRole::User, "hi"),
            tool_msg("orphan", "leftover"),
            msg(MessageRole::Assistant, "done"),
        ];
        let out = compressor.sanitize_tool_pairs(messages);
        assert!(!out
            .iter()
            .any(|m| m.tool_call_id.as_deref() == Some("orphan")));
    }

    #[test]
    fn sanitiser_inserts_stub_for_missing_results() {
        let cfg = quiet_config();
        let compressor =
            ContextCompressor::new(cfg, aux_with_provider(CannedSummaryProvider::new("x")));
        let messages = vec![
            msg(MessageRole::System, "sys"),
            msg(MessageRole::User, "hi"),
            assistant_with_tool_call("c1", "shell", "ls"),
            // no tool result
            msg(MessageRole::User, "next?"),
        ];
        let out = compressor.sanitize_tool_pairs(messages);
        let stub = out
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("c1") && m.role == MessageRole::Tool)
            .expect("expected stub tool result");
        assert!(stub
            .content
            .as_deref()
            .unwrap_or_default()
            .contains("Result from earlier conversation"));
    }

    // ---------- boundary alignment ----------

    #[test]
    fn align_forward_skips_orphan_tool_messages() {
        let cfg = quiet_config();
        let compressor =
            ContextCompressor::new(cfg, aux_with_provider(CannedSummaryProvider::new("x")));
        let messages = vec![
            tool_msg("c1", "x"),
            tool_msg("c2", "y"),
            msg(MessageRole::User, "real"),
        ];
        assert_eq!(compressor.align_boundary_forward(&messages, 0), 2);
    }

    #[test]
    fn align_backward_pulls_to_parent_assistant() {
        let cfg = quiet_config();
        let compressor =
            ContextCompressor::new(cfg, aux_with_provider(CannedSummaryProvider::new("x")));
        let messages = vec![
            msg(MessageRole::User, "hi"),
            assistant_with_tool_call("c1", "shell", "ls"),
            tool_msg("c1", "ok"),
            msg(MessageRole::User, "next"),
        ];
        // Boundary at idx=3 (the trailing user message) should pull back to
        // the parent assistant (idx=1) so the assistant + tool_result group
        // is summarised atomically.
        assert_eq!(compressor.align_boundary_backward(&messages, 3), 1);
    }

    #[test]
    fn align_backward_is_noop_when_idx_at_end() {
        let cfg = quiet_config();
        let compressor =
            ContextCompressor::new(cfg, aux_with_provider(CannedSummaryProvider::new("x")));
        let messages = vec![
            msg(MessageRole::User, "hi"),
            assistant_with_tool_call("c1", "shell", "ls"),
            tool_msg("c1", "ok"),
        ];
        // idx == messages.len() — Python parity: early return without alignment.
        assert_eq!(compressor.align_boundary_backward(&messages, 3), 3);
    }

    // ---------- end-to-end compress() ----------

    #[tokio::test]
    async fn compress_short_conversation_is_noop() {
        let provider = CannedSummaryProvider::new("ignored");
        let aux = aux_with_provider(provider.clone());
        let cfg = CompressorConfig {
            protect_first_n: 3,
            protect_last_n: 20,
            ..quiet_config()
        };
        let mut compressor = ContextCompressor::new(cfg, aux);
        let messages = vec![
            msg(MessageRole::System, "sys"),
            msg(MessageRole::User, "a"),
            msg(MessageRole::Assistant, "b"),
        ];
        let out = compressor.compress(messages.clone(), Some(50_000)).await;
        assert_eq!(out.len(), messages.len());
        assert_eq!(provider.call_count(), 0);
    }

    #[tokio::test]
    async fn compress_long_conversation_emits_summary_and_keeps_tail() {
        let provider = CannedSummaryProvider::new("Goal: keep going.");
        let aux = aux_with_provider(provider.clone());
        let cfg = CompressorConfig {
            context_length: 20_000,
            threshold_percent: 0.5,
            protect_first_n: 2,
            protect_last_n: 5,
            ..quiet_config()
        };
        let mut compressor = ContextCompressor::new(cfg, aux);

        let mut messages = vec![
            msg(MessageRole::System, "sys"),
            msg(MessageRole::User, "kickoff"),
        ];
        // 30 medium turns to push over the threshold.
        for i in 0..30 {
            messages.push(msg(
                if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                &format!("turn {i}: {}", "x".repeat(800)),
            ));
        }
        // Final 5 short tail turns.
        for i in 0..5 {
            messages.push(msg(MessageRole::User, &format!("tail {i}")));
        }

        let original_len = messages.len();
        let out = compressor.compress(messages, Some(80_000)).await;
        assert!(out.len() < original_len, "compressed list should shrink");
        assert!(provider.call_count() >= 1, "auxiliary summariser invoked");
        assert!(
            out.iter()
                .any(|m| m.content.as_deref().unwrap_or("").contains(SUMMARY_PREFIX)),
            "summary banner should be present"
        );
        // Tail preserved verbatim.
        let last = out.last().unwrap();
        assert_eq!(last.content.as_deref(), Some("tail 4"));
    }

    #[tokio::test]
    async fn compress_falls_back_to_static_marker_on_summary_failure() {
        let aux = aux_with_provider(Arc::new(FailingProvider));
        let cfg = CompressorConfig {
            context_length: 10_000,
            threshold_percent: 0.5,
            protect_first_n: 1,
            protect_last_n: 3,
            ..quiet_config()
        };
        let mut compressor = ContextCompressor::new(cfg, aux);

        let mut messages = vec![msg(MessageRole::System, "sys")];
        for i in 0..30 {
            messages.push(msg(
                if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                &format!("turn {i}: {}", "y".repeat(400)),
            ));
        }
        let out = compressor.compress(messages, Some(60_000)).await;
        let banner = out
            .iter()
            .find_map(|m| {
                let content = m.content.as_deref()?;
                if content.contains("Summary generation was unavailable") {
                    Some(content)
                } else {
                    None
                }
            })
            .expect("static fallback banner missing");
        assert!(banner.contains(SUMMARY_PREFIX));
        assert!(banner.contains("message(s) were removed"));
        assert!(compressor.last_summary_fallback_used());
        assert!(compressor.last_summary_dropped_count() > 0);
        assert!(compressor.last_summary_error().is_some());
    }
}
