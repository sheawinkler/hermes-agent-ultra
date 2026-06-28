#[cfg(test)]
mod tests {
    use super::*;

    lazy_static::lazy_static! {
        static ref FUSION_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    }

    /// A minimal test provider.
    struct TestProvider {
        provider_name: String,
        prompt_block: String,
        prefetch_result: String,
        tools: Vec<Value>,
    }

    impl TestProvider {
        fn new(name: &str) -> Self {
            Self {
                provider_name: name.to_string(),
                prompt_block: String::new(),
                prefetch_result: String::new(),
                tools: Vec::new(),
            }
        }

        fn with_prompt(mut self, block: &str) -> Self {
            self.prompt_block = block.to_string();
            self
        }

        fn with_prefetch(mut self, result: &str) -> Self {
            self.prefetch_result = result.to_string();
            self
        }

        fn with_tool(mut self, name: &str) -> Self {
            self.tools.push(serde_json::json!({"name": name}));
            self
        }
    }

    impl MemoryProviderPlugin for TestProvider {
        fn name(&self) -> &str {
            &self.provider_name
        }
        fn system_prompt_block(&self) -> String {
            self.prompt_block.clone()
        }
        fn prefetch(&self, _query: &str, _session_id: &str) -> String {
            self.prefetch_result.clone()
        }
        fn get_tool_schemas(&self) -> Vec<Value> {
            self.tools.clone()
        }
        fn handle_tool_call(&self, tool_name: &str, _args: &Value) -> String {
            serde_json::json!({"ok": tool_name}).to_string()
        }
    }

    #[derive(Default)]
    struct RecordingProvider {
        prefetched: Arc<std::sync::Mutex<Vec<String>>>,
        queued: Arc<std::sync::Mutex<Vec<String>>>,
        synced: Arc<std::sync::Mutex<Vec<String>>>,
        synced_assistant: Arc<std::sync::Mutex<Vec<String>>>,
        structured_messages: Arc<std::sync::Mutex<Vec<Vec<Value>>>>,
        memory_writes: Arc<std::sync::Mutex<Vec<String>>>,
        tool_args: Arc<std::sync::Mutex<Vec<Value>>>,
        turn_messages: Arc<std::sync::Mutex<Vec<String>>>,
        session_messages: Arc<std::sync::Mutex<Vec<Value>>>,
    }

    impl RecordingProvider {
        fn new() -> Self {
            Self::default()
        }
    }

    impl MemoryProviderPlugin for RecordingProvider {
        fn name(&self) -> &str {
            "recording"
        }

        fn prefetch(&self, query: &str, _session_id: &str) -> String {
            self.prefetched.lock().unwrap().push(query.to_string());
            String::new()
        }

        fn queue_prefetch(&self, query: &str, _session_id: &str) {
            self.queued.lock().unwrap().push(query.to_string());
        }

        fn sync_turn(&self, user_content: &str, assistant_content: &str, _session_id: &str) {
            self.synced.lock().unwrap().push(user_content.to_string());
            self.synced_assistant
                .lock()
                .unwrap()
                .push(assistant_content.to_string());
        }

        fn sync_turn_with_messages(
            &self,
            user_content: &str,
            assistant_content: &str,
            session_id: &str,
            messages: &[Value],
        ) {
            self.sync_turn(user_content, assistant_content, session_id);
            self.structured_messages
                .lock()
                .unwrap()
                .push(messages.to_vec());
        }

        fn get_tool_schemas(&self) -> Vec<Value> {
            vec![serde_json::json!({"name": "record_memory"})]
        }

        fn handle_tool_call(&self, _tool_name: &str, args: &Value) -> String {
            self.tool_args.lock().unwrap().push(args.clone());
            serde_json::json!({"ok": true}).to_string()
        }

        fn on_turn_start(&self, _turn_number: u32, message: &str) {
            self.turn_messages.lock().unwrap().push(message.to_string());
        }

        fn on_session_end(&self, messages: &[Value]) {
            self.session_messages
                .lock()
                .unwrap()
                .extend(messages.iter().cloned());
        }

        fn on_memory_write(&self, _action: &str, _target: &str, content: &str) {
            self.memory_writes.lock().unwrap().push(content.to_string());
        }
    }

    const SINGLE_SKILL_TURN: &str = concat!(
        "[SYSTEM: The user has invoked the \"skill-creator\" skill, indicating they want ",
        "you to follow its instructions. The full skill content is loaded below.]\n\n",
        "# Skill Creator\n\n",
        "Large skill body that must not be searched or embedded.\n\n",
        "The user has provided the following instruction alongside the skill invocation: ",
        "make a skill for release triage"
    );

    const BUNDLE_TURN: &str = concat!(
        "[IMPORTANT: The user has invoked the \"backend-dev\" skill bundle, ",
        "loading 2 skills together. Treat every skill below as active guidance for this turn.]\n\n",
        "Bundle: backend-dev\n",
        "Skills loaded: test-driven-development, code-review\n\n",
        "User instruction: fix the failing retrieval test\n\n",
        "[Loaded as part of the \"backend-dev\" skill bundle.]\n\n",
        "Large bundled skill body that must not be searched or embedded."
    );

    const BARE_SKILL_TURN: &str = concat!(
        "[SYSTEM: The user has invoked the \"skill-creator\" skill, indicating they want ",
        "you to follow its instructions. The full skill content is loaded below.]\n\n",
        "# Skill Creator\n\n",
        "Large skill body, no user instruction."
    );
    const FAKE_SECRET: &str = "sk-test-secret-token-123456";

    fn assert_fake_secret_redacted(rendered: &str) {
        assert!(!rendered.contains(FAKE_SECRET));
        assert!(!rendered.contains("secret-token"));
        assert!(
            rendered.contains("...") || rendered.contains("***") || rendered.contains("[REDACTED")
        );
    }

    #[test]
    fn test_add_builtin_provider() {
        let mut mm = MemoryManager::new();
        mm.add_provider(Arc::new(TestProvider::new("builtin")));
        assert_eq!(mm.providers().len(), 1);
    }

    #[test]
    fn test_accept_multiple_external() {
        let mut mm = MemoryManager::new();
        mm.add_provider(Arc::new(TestProvider::new("builtin")));
        mm.add_provider(Arc::new(TestProvider::new("honcho")));
        mm.add_provider(Arc::new(TestProvider::new("hindsight")));
        assert_eq!(mm.providers().len(), 3);
    }

    #[test]
    fn test_build_system_prompt() {
        let mut mm = MemoryManager::new();
        mm.add_provider(Arc::new(
            TestProvider::new("builtin").with_prompt("Memory is available."),
        ));
        mm.add_provider(Arc::new(
            TestProvider::new("ext").with_prompt("External memory active."),
        ));
        let prompt = mm.build_system_prompt();
        assert!(prompt.contains("Memory is available."));
        assert!(prompt.contains("External memory active."));
    }

    #[test]
    fn test_prefetch_all_wraps_in_fence() {
        let _guard = FUSION_ENV_LOCK.lock().expect("fusion env lock");
        let orig = std::env::var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE").ok();
        std::env::remove_var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE");
        let mut mm = MemoryManager::new();
        mm.add_provider(Arc::new(
            TestProvider::new("builtin").with_prefetch("User likes Rust."),
        ));
        let ctx = mm.prefetch_all("hello", "");
        assert!(ctx.contains("<memory-context>"));
        assert!(ctx.contains("User likes Rust."));
        assert!(ctx.contains("</memory-context>"));
        match orig {
            Some(v) => std::env::set_var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE", v),
            None => std::env::remove_var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE"),
        }
    }

    #[test]
    fn test_memory_context_block_redacts_sensitive_recall() {
        let block =
            build_memory_context_block(&format!("recalled prompt contained api_key={FAKE_SECRET}"));

        assert!(block.contains("<memory-context>"));
        assert_fake_secret_redacted(&block);
    }

    #[test]
    fn test_prefetch_all_empty() {
        let mut mm = MemoryManager::new();
        mm.add_provider(Arc::new(TestProvider::new("builtin")));
        let ctx = mm.prefetch_all("hello", "");
        assert!(ctx.is_empty());
    }

    #[test]
    fn test_prefetch_all_strips_single_skill_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let prefetched = provider.prefetched.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);

        let ctx = mm.prefetch_all(SINGLE_SKILL_TURN, "session");

        assert!(ctx.is_empty());
        assert_eq!(
            *prefetched.lock().unwrap(),
            vec!["make a skill for release triage".to_string()]
        );
    }

    #[test]
    fn test_prefetch_all_redacts_sensitive_query_before_provider_call() {
        let provider = Arc::new(RecordingProvider::new());
        let prefetched = provider.prefetched.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);

        let _ctx = mm.prefetch_all(&format!("look up api_key={FAKE_SECRET}"), "session");

        let prefetched = prefetched.lock().unwrap();
        assert_eq!(prefetched.len(), 1);
        assert_fake_secret_redacted(&prefetched[0]);
    }

    #[test]
    fn test_prefetch_all_skips_bare_skill_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let prefetched = provider.prefetched.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);

        let ctx = mm.prefetch_all(BARE_SKILL_TURN, "session");

        assert!(ctx.is_empty());
        assert!(prefetched.lock().unwrap().is_empty());
    }

    #[test]
    fn test_queue_prefetch_all_strips_bundle_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let queued = provider.queued.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);

        mm.queue_prefetch_all(BUNDLE_TURN, "session");

        assert_eq!(
            *queued.lock().unwrap(),
            vec!["fix the failing retrieval test".to_string()]
        );
    }

    #[test]
    fn test_queue_prefetch_all_redacts_sensitive_query_before_provider_call() {
        let provider = Arc::new(RecordingProvider::new());
        let queued = provider.queued.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);

        mm.queue_prefetch_all(&format!("queue token={FAKE_SECRET}"), "session");

        let queued = queued.lock().unwrap();
        assert_eq!(queued.len(), 1);
        assert_fake_secret_redacted(&queued[0]);
    }

    #[test]
    fn test_queue_prefetch_all_skips_bare_skill_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let queued = provider.queued.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);

        mm.queue_prefetch_all(BARE_SKILL_TURN, "session");

        assert!(queued.lock().unwrap().is_empty());
    }

    #[test]
    fn test_sync_all_strips_single_skill_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let synced = provider.synced.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);

        mm.sync_all(SINGLE_SKILL_TURN, "Done.", "session");

        assert_eq!(
            *synced.lock().unwrap(),
            vec!["make a skill for release triage".to_string()]
        );
    }

    #[test]
    fn test_sync_all_redacts_sensitive_turn_content_before_provider_call() {
        let provider = Arc::new(RecordingProvider::new());
        let synced = provider.synced.clone();
        let synced_assistant = provider.synced_assistant.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);

        mm.sync_all(
            &format!("user pasted api_key={FAKE_SECRET}"),
            &format!("assistant echoed bearer {FAKE_SECRET}"),
            "session",
        );

        let synced = synced.lock().unwrap();
        let synced_assistant = synced_assistant.lock().unwrap();
        assert_eq!(synced.len(), 1);
        assert_eq!(synced_assistant.len(), 1);
        assert_fake_secret_redacted(&synced[0]);
        assert_fake_secret_redacted(&synced_assistant[0]);
    }

    #[test]
    fn test_sync_all_with_messages_forwards_redacted_structured_transcript() {
        let provider = Arc::new(RecordingProvider::new());
        let structured_messages = provider.structured_messages.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);
        let tool_call = hermes_core::ToolCall {
            id: "call_terminal".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: serde_json::to_string(&serde_json::json!({
                    "cmd": format!("echo {FAKE_SECRET}")
                }))
                .unwrap(),
            },
            extra_content: None,
        };

        mm.sync_all_with_messages(
            &format!("user pasted api_key={FAKE_SECRET}"),
            "assistant used a tool",
            "session",
            &[
                Message::user(format!("user pasted api_key={FAKE_SECRET}")),
                Message::assistant_with_tool_calls(
                    Some("assistant used a tool".to_string()),
                    vec![tool_call],
                ),
            ],
        );

        let captured = structured_messages.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let rendered = serde_json::to_string(&captured[0]).unwrap();
        assert_fake_secret_redacted(&rendered);
        assert!(rendered.contains("call_terminal"));
    }

    #[test]
    fn test_sync_all_skips_bare_skill_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let synced = provider.synced.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);

        mm.sync_all(BARE_SKILL_TURN, "Done.", "session");

        assert!(synced.lock().unwrap().is_empty());
    }

    #[test]
    fn test_tool_routing() {
        let mut mm = MemoryManager::new();
        mm.add_provider(Arc::new(TestProvider::new("builtin").with_tool("memory")));
        assert!(mm.has_tool("memory"));
        assert!(!mm.has_tool("nonexistent"));

        let result = mm.handle_tool_call("memory", &serde_json::json!({}));
        assert!(result.contains("memory"));
    }

    #[test]
    fn test_memory_tool_args_are_redacted_before_provider_call() {
        let provider = Arc::new(RecordingProvider::new());
        let tool_args = provider.tool_args.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);

        let result = mm.handle_tool_call(
            "record_memory",
            &serde_json::json!({
                "query": format!("find token={FAKE_SECRET}"),
                "nested": {"content": format!("write api_key={FAKE_SECRET}")},
            }),
        );

        assert!(result.contains("ok"));
        let tool_args = tool_args.lock().unwrap();
        assert_eq!(tool_args.len(), 1);
        let rendered = tool_args[0].to_string();
        assert_fake_secret_redacted(&rendered);
    }

    #[test]
    fn test_lifecycle_memory_callbacks_redact_sensitive_text_before_provider_call() {
        let provider = Arc::new(RecordingProvider::new());
        let turn_messages = provider.turn_messages.clone();
        let session_messages = provider.session_messages.clone();
        let memory_writes = provider.memory_writes.clone();
        let mut mm = MemoryManager::new();
        mm.add_provider(provider);

        mm.on_turn_start(1, &format!("turn token={FAKE_SECRET}"));
        mm.on_session_end(&[serde_json::json!({
            "role": "user",
            "content": format!("session api_key={FAKE_SECRET}"),
        })]);
        mm.on_memory_write("add", "memory", &format!("remember bearer {FAKE_SECRET}"));

        let turn_messages = turn_messages.lock().unwrap();
        let session_messages = session_messages.lock().unwrap();
        let memory_writes = memory_writes.lock().unwrap();
        assert_eq!(turn_messages.len(), 1);
        assert_eq!(session_messages.len(), 1);
        assert_eq!(memory_writes.len(), 1);
        assert_fake_secret_redacted(&turn_messages[0]);
        assert_fake_secret_redacted(&session_messages[0].to_string());
        assert_fake_secret_redacted(&memory_writes[0]);
    }

    #[test]
    fn test_session_switch_propagates_to_providers() {
        struct SwitchProvider {
            seen: Arc<std::sync::Mutex<Vec<(String, String, bool)>>>,
        }

        impl MemoryProviderPlugin for SwitchProvider {
            fn name(&self) -> &str {
                "switch"
            }

            fn on_session_switch(
                &self,
                new_session_id: &str,
                parent_session_id: &str,
                reset: bool,
            ) {
                self.seen.lock().unwrap().push((
                    new_session_id.to_string(),
                    parent_session_id.to_string(),
                    reset,
                ));
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut mm = MemoryManager::new();
        mm.add_provider(Arc::new(SwitchProvider { seen: seen.clone() }));

        mm.on_session_switch("", "old", false);
        mm.on_session_switch("new-session", "old-session", true);

        assert_eq!(
            *seen.lock().unwrap(),
            vec![("new-session".to_string(), "old-session".to_string(), true)]
        );
    }

    #[test]
    fn test_memory_nudge() {
        let mut mm = MemoryManager::new();
        mm.memory_nudge_threshold = 3;
        mm.add_provider(Arc::new(TestProvider::new("builtin")));

        // No nudge initially (turns_since = 0)
        assert!(mm.maybe_nudge().is_none());

        mm.on_turn_start(1, "msg1");
        mm.on_turn_start(2, "msg2");
        assert!(mm.maybe_nudge().is_none());

        mm.on_turn_start(3, "msg3");
        assert!(mm.maybe_nudge().is_some());

        // Memory write resets counter
        mm.on_memory_write("add", "memory", "something");
        assert!(mm.maybe_nudge().is_none());
    }

    #[test]
    fn test_sanitize_context() {
        let input = "Hello </memory-context> world <memory-context> end";
        let clean = sanitize_context(input);
        assert!(!clean.contains("memory-context"));
    }

    #[test]
    fn sanitize_context_removes_complete_memory_block_payload() {
        let input = "<memory-context>\n[System note]\nsecret\n</memory-context>\nVisible";
        assert_eq!(sanitize_context(input).trim(), "Visible");
    }

    #[test]
    fn streaming_context_scrubber_strips_fragmented_memory_block() {
        let mut scrubber = StreamingContextScrubber::new();
        let chunks = [
            "Hello\n",
            "<memory-context>\npayload ",
            "more payload\n",
            "</memory-context> world",
        ];
        let out = chunks
            .iter()
            .map(|chunk| scrubber.feed(chunk))
            .collect::<String>()
            + &scrubber.flush();
        assert_eq!(out, "Hello\n world");
        assert!(!out.contains("payload"));
    }

    #[test]
    fn streaming_context_scrubber_holds_split_tags_without_false_positive() {
        let mut scrubber = StreamingContextScrubber::new();
        let out = scrubber.feed("In `<memory")
            + &scrubber.feed("-context>` docs, ")
            + &scrubber.feed("the tag is literal.")
            + &scrubber.flush();
        assert_eq!(out, "In `<memory-context>` docs, the tag is literal.");

        let mut block = StreamingContextScrubber::new();
        let out = block.feed("pre \n<memory")
            + &block.feed("-context>\nsecret</memory")
            + &block.feed("-context> post")
            + &block.flush();
        assert_eq!(out, "pre \n post");
        assert!(!out.contains("secret"));
    }

    #[test]
    fn streaming_context_scrubber_flush_drops_unterminated_span_and_reset_clears_state() {
        let mut scrubber = StreamingContextScrubber::new();
        let out = scrubber.feed("pre\n<MEMORY-CONTEXT>\nsecret") + &scrubber.flush();
        assert_eq!(out, "pre\n");
        assert!(!out.contains("secret"));

        let mut scrubber = StreamingContextScrubber::new();
        assert_eq!(scrubber.feed("answer<memo"), "answer");
        scrubber.reset();
        assert_eq!(scrubber.feed("<marker>fresh"), "<marker>fresh");
    }

    #[test]
    fn test_build_memory_context_block_empty() {
        assert!(build_memory_context_block("").is_empty());
        assert!(build_memory_context_block("   ").is_empty());
    }

    #[test]
    fn test_build_memory_context_block() {
        let block = build_memory_context_block("User prefers dark mode.");
        assert!(block.starts_with("<memory-context>"));
        assert!(block.ends_with("</memory-context>"));
        assert!(block.contains("User prefers dark mode."));
        assert!(block.contains("[System note:"));
    }

    #[test]
    fn test_fusion_deduplicates_and_orders_by_score() {
        let _guard = FUSION_ENV_LOCK.lock().expect("fusion env lock");
        let orig = std::env::var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE").ok();
        std::env::remove_var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE");
        let candidates = vec![
            FusedMemoryCandidate {
                provider: "builtin".to_string(),
                content: "User likes Rust and tokio".to_string(),
            },
            FusedMemoryCandidate {
                provider: "contextlattice".to_string(),
                content: "User likes Rust and tokio".to_string(),
            },
            FusedMemoryCandidate {
                provider: "supermemory".to_string(),
                content: "User writes Python and SQL".to_string(),
            },
        ];
        let fused = fuse_memory_candidates(candidates, "Need rust tokio context");
        assert_eq!(fused.len(), 2);
        assert!(fused[0].contains("Rust"));
        assert!(fused[0].contains("score="));
        assert!(fused[0].contains("conf="));
        match orig {
            Some(v) => std::env::set_var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE", v),
            None => std::env::remove_var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE"),
        }
    }

    #[test]
    fn test_fusion_min_confidence_gate_filters_low_confidence() {
        let _guard = FUSION_ENV_LOCK.lock().expect("fusion env lock");
        let orig = std::env::var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE").ok();
        std::env::set_var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE", "0.95");
        let candidates = vec![
            FusedMemoryCandidate {
                provider: "builtin".to_string(),
                content: "lightweight hint without overlap".to_string(),
            },
            FusedMemoryCandidate {
                provider: "contextlattice".to_string(),
                content: "Need rust tokio context and repo details".to_string(),
            },
        ];
        let fused = fuse_memory_candidates(candidates, "Need rust tokio context");
        assert_eq!(fused.len(), 1, "only high-confidence context should remain");
        assert!(fused[0].contains("contextlattice"));
        match orig {
            Some(v) => std::env::set_var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE", v),
            None => std::env::remove_var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE"),
        }
    }

    #[test]
    fn test_query_terms_filters_short_tokens() {
        let terms = query_terms("go rust dl tokio memory");
        assert_eq!(terms, vec!["rust", "tokio", "memory"]);
    }
}
