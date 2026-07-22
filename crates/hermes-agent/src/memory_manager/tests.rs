#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    use crate::memory_plugins::builtin::BuiltinMemoryPlugin;
    use parking_lot::Mutex;
    lazy_static::lazy_static! {
        static ref FUSION_ENV_LOCK: Mutex<()> = Mutex::new(());
    }

    fn test_manager() -> (tempfile::TempDir, MemoryManager) {
        let home = tempfile::tempdir().expect("memory home");
        let subject = MemorySubject::local_owner(MemorySurface::Local, "manager-test-session")
            .expect("test subject");
        let manager = MemoryManager::new(subject, home.path()).expect("test memory manager");
        (home, manager)
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

        fn prefetch(
            &self,
            _subject: &MemorySubject,
            _filter: &MemorySubjectStorageKey,
            _query: &str,
        ) -> String {
            self.prefetch_result.clone()
        }

        fn get_tool_schemas(&self) -> Vec<Value> {
            self.tools.clone()
        }

        fn handle_tool_call(
            &self,
            _subject: &MemorySubject,
            _filter: &MemorySubjectStorageKey,
            tool_name: &str,
            _args: &Value,
        ) -> String {
            serde_json::json!({"ok": tool_name}).to_string()
        }
    }

    #[derive(Default)]
    struct RecordingProvider {
        prefetched: Arc<Mutex<Vec<String>>>,
        queued: Arc<Mutex<Vec<String>>>,
        synced: Arc<Mutex<Vec<String>>>,
        synced_assistant: Arc<Mutex<Vec<String>>>,
        structured_messages: Arc<Mutex<Vec<Vec<Value>>>>,
        memory_writes: Arc<Mutex<Vec<String>>>,
        tool_args: Arc<Mutex<Vec<Value>>>,
        turn_messages: Arc<Mutex<Vec<String>>>,
        session_messages: Arc<Mutex<Vec<Value>>>,
        filters: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingProvider {
        fn new() -> Self {
            Self::default()
        }

        fn record_filter(&self, filter: &MemorySubjectStorageKey) {
            self.filters.lock().push(filter.as_str().to_string());
        }
    }

    impl MemoryProviderPlugin for RecordingProvider {
        fn name(&self) -> &str {
            "recording"
        }

        fn prefetch(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            query: &str,
        ) -> String {
            self.record_filter(filter);
            self.prefetched.lock().push(query.to_string());
            String::new()
        }

        fn queue_prefetch(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            query: &str,
        ) {
            self.record_filter(filter);
            self.queued.lock().push(query.to_string());
        }

        fn sync_turn(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            user_content: &str,
            assistant_content: &str,
        ) {
            self.record_filter(filter);
            self.synced.lock().push(user_content.to_string());
            self.synced_assistant.lock().push(assistant_content.to_string());
        }

        fn sync_turn_with_messages(
            &self,
            subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            user_content: &str,
            assistant_content: &str,
            messages: &[Value],
        ) {
            self.sync_turn(subject, filter, user_content, assistant_content);
            self.structured_messages.lock().push(messages.to_vec());
        }

        fn get_tool_schemas(&self) -> Vec<Value> {
            vec![serde_json::json!({"name": "record_memory"})]
        }

        fn handle_tool_call(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            _tool_name: &str,
            args: &Value,
        ) -> String {
            self.record_filter(filter);
            self.tool_args.lock().push(args.clone());
            serde_json::json!({"ok": true}).to_string()
        }

        fn on_turn_start(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            _turn_number: u32,
            message: &str,
        ) {
            self.record_filter(filter);
            self.turn_messages.lock().push(message.to_string());
        }

        fn on_session_end(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            messages: &[Value],
        ) {
            self.record_filter(filter);
            self.session_messages.lock().extend(messages.iter().cloned());
        }

        fn on_memory_write(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            _action: &str,
            _target: &str,
            content: &str,
        ) {
            self.record_filter(filter);
            self.memory_writes.lock().push(content.to_string());
        }
    }

    struct GovernedProvider {
        provider_name: &'static str,
        residual_records: u64,
        available: bool,
    }

    impl GovernedProvider {
        fn new(provider_name: &'static str, residual_records: u64) -> Self {
            Self {
                provider_name,
                residual_records,
                available: true,
            }
        }

        fn unavailable(provider_name: &'static str) -> Self {
            Self {
                provider_name,
                residual_records: 0,
                available: false,
            }
        }
    }

    impl MemoryProviderPlugin for GovernedProvider {
        fn name(&self) -> &str {
            self.provider_name
        }

        fn is_available(&self) -> bool {
            self.available
        }

        fn export_subject(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            _actor: &MemoryAuditActor,
            sink: &mut dyn MemoryExportSink,
        ) -> Result<MemoryExportReceipt, MemoryProviderGovernanceError> {
            let snapshot = MemoryExportSnapshot::new(
                MemoryExportEvidenceDigest::sha256_bytes(
                    format!("{}:snapshot", self.provider_name).as_bytes(),
                ),
                MemoryExportEvidenceDigest::sha256_bytes(
                    format!("{}:revision", self.provider_name).as_bytes(),
                ),
                None,
            );
            let failure = || {
                MemoryProviderGovernanceError::failed(
                    self.provider_name,
                    MemoryGovernanceOperation::Export,
                )
            };
            sink.begin_provider(snapshot.clone()).map_err(|_| failure())?;
            let records = vec![
                json!({"provider": self.provider_name, "ordinal": 0}),
                json!({"provider": self.provider_name, "ordinal": 1}),
            ];
            let page = MemoryExportPage::new(
                None,
                None,
                records
                    .iter()
                    .map(|record| {
                        MemoryExportEvidenceDigest::sha256_bytes(
                            serde_json::to_string(record)
                                .expect("test record")
                                .as_bytes(),
                        )
                    })
                    .collect(),
                records,
            )
            .map_err(|_| failure())?;
            sink.append_page(page).map_err(|_| failure())?;
            sink.finish_provider(MemoryExportCompletion::exhausted(2, 1))
                .map_err(|_| failure())?;
            Ok(MemoryExportReceipt::exhausted(
                self.provider_name,
                filter.clone(),
                snapshot,
                2,
                1,
            ))
        }

        fn forget_subject(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            _actor: &MemoryAuditActor,
        ) -> Result<MemoryForgetReceipt, MemoryProviderGovernanceError> {
            Ok(MemoryForgetReceipt::new(self.provider_name, filter.clone(), 2))
        }

        fn retention_sweep(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            _policy: &MemoryRetentionPolicy,
            _actor: &MemoryAuditActor,
        ) -> Result<MemoryRetentionReceipt, MemoryProviderGovernanceError> {
            Ok(MemoryRetentionReceipt::new(self.provider_name, filter.clone(), 1))
        }

        fn verify_subject_absent(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
        ) -> Result<MemoryReadbackReceipt, MemoryProviderGovernanceError> {
            Ok(if self.residual_records == 0 {
                MemoryReadbackReceipt::absent(self.provider_name, filter.clone())
            } else {
                MemoryReadbackReceipt::residual(
                    self.provider_name,
                    filter.clone(),
                    self.residual_records,
                )
            })
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
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(Arc::new(TestProvider::new("builtin")));
        assert_eq!(mm.providers().len(), 1);
    }

    #[test]
    fn test_accept_multiple_external() {
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(Arc::new(TestProvider::new("builtin")));
        mm.add_provider(Arc::new(TestProvider::new("honcho")));
        mm.add_provider(Arc::new(TestProvider::new("hindsight")));
        assert_eq!(mm.providers().len(), 3);
    }

    #[test]
    fn test_build_system_prompt() {
        let (_memory_home, mut mm) = test_manager();
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
        let _guard = FUSION_ENV_LOCK.lock();
        let orig = std::env::var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE").ok();
        std::env::remove_var("HERMES_MEMORY_FUSION_MIN_CONFIDENCE");
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(Arc::new(
            TestProvider::new("builtin").with_prefetch("User likes Rust."),
        ));
        let ctx = mm.prefetch_all("hello");
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
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(Arc::new(TestProvider::new("builtin")));
        let ctx = mm.prefetch_all("hello");
        assert!(ctx.is_empty());
    }

    #[test]
    fn test_prefetch_all_strips_single_skill_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let prefetched = provider.prefetched.clone();
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(provider);

        let ctx = mm.prefetch_all(SINGLE_SKILL_TURN);

        assert!(ctx.is_empty());
        assert_eq!(
            *prefetched.lock(),
            vec!["make a skill for release triage".to_string()]
        );
    }

    #[test]
    fn test_prefetch_all_redacts_sensitive_query_before_provider_call() {
        let provider = Arc::new(RecordingProvider::new());
        let prefetched = provider.prefetched.clone();
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(provider);

        let _ctx = mm.prefetch_all(&format!("look up api_key={FAKE_SECRET}"));

        let prefetched = prefetched.lock();
        assert_eq!(prefetched.len(), 1);
        assert_fake_secret_redacted(&prefetched[0]);
    }

    #[test]
    fn test_prefetch_all_skips_bare_skill_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let prefetched = provider.prefetched.clone();
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(provider);

        let ctx = mm.prefetch_all(BARE_SKILL_TURN);

        assert!(ctx.is_empty());
        assert!(prefetched.lock().is_empty());
    }

    #[test]
    fn test_queue_prefetch_all_strips_bundle_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let queued = provider.queued.clone();
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(provider);

        mm.queue_prefetch_all(BUNDLE_TURN);

        assert_eq!(
            *queued.lock(),
            vec!["fix the failing retrieval test".to_string()]
        );
    }

    #[test]
    fn test_queue_prefetch_all_redacts_sensitive_query_before_provider_call() {
        let provider = Arc::new(RecordingProvider::new());
        let queued = provider.queued.clone();
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(provider);

        mm.queue_prefetch_all(&format!("queue token={FAKE_SECRET}"));

        let queued = queued.lock();
        assert_eq!(queued.len(), 1);
        assert_fake_secret_redacted(&queued[0]);
    }

    #[test]
    fn test_queue_prefetch_all_skips_bare_skill_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let queued = provider.queued.clone();
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(provider);

        mm.queue_prefetch_all(BARE_SKILL_TURN);

        assert!(queued.lock().is_empty());
    }

    #[test]
    fn test_sync_all_strips_single_skill_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let synced = provider.synced.clone();
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(provider);

        mm.sync_all(SINGLE_SKILL_TURN, "Done.");

        assert_eq!(
            *synced.lock(),
            vec!["make a skill for release triage".to_string()]
        );
    }

    #[test]
    fn test_sync_all_redacts_sensitive_turn_content_before_provider_call() {
        let provider = Arc::new(RecordingProvider::new());
        let synced = provider.synced.clone();
        let synced_assistant = provider.synced_assistant.clone();
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(provider);

        mm.sync_all(&format!("user pasted api_key={FAKE_SECRET}"), &format!("assistant echoed bearer {FAKE_SECRET}"));

        let synced = synced.lock();
        let synced_assistant = synced_assistant.lock();
        assert_eq!(synced.len(), 1);
        assert_eq!(synced_assistant.len(), 1);
        assert_fake_secret_redacted(&synced[0]);
        assert_fake_secret_redacted(&synced_assistant[0]);
    }

    #[test]
    fn test_sync_all_with_messages_forwards_redacted_structured_transcript() {
        let provider = Arc::new(RecordingProvider::new());
        let structured_messages = provider.structured_messages.clone();
        let (_memory_home, mut mm) = test_manager();
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

        mm.sync_all_with_messages(&format!("user pasted api_key={FAKE_SECRET}"), "assistant used a tool", &[
            Message::user(format!("user pasted api_key={FAKE_SECRET}")),
            Message::assistant_with_tool_calls(
                Some("assistant used a tool".to_string()),
                vec![tool_call],
            ),
        ]);

        let captured = structured_messages.lock();
        assert_eq!(captured.len(), 1);
        let rendered = serde_json::to_string(&captured[0]).unwrap();
        assert_fake_secret_redacted(&rendered);
        assert!(rendered.contains("call_terminal"));
    }

    #[test]
    fn test_sync_all_skips_bare_skill_scaffolding() {
        let provider = Arc::new(RecordingProvider::new());
        let synced = provider.synced.clone();
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(provider);

        mm.sync_all(BARE_SKILL_TURN, "Done.");

        assert!(synced.lock().is_empty());
    }

    #[test]
    fn test_tool_routing() {
        let (_memory_home, mut mm) = test_manager();
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
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(provider);

        let result = mm.handle_tool_call(
            "record_memory",
            &serde_json::json!({
                "query": format!("find token={FAKE_SECRET}"),
                "nested": {"content": format!("write api_key={FAKE_SECRET}")},
            }),
        );

        assert!(result.contains("ok"));
        let tool_args = tool_args.lock();
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
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(provider);

        mm.on_turn_start(1, &format!("turn token={FAKE_SECRET}"));
        mm.on_session_end(&[serde_json::json!({
            "role": "user",
            "content": format!("session api_key={FAKE_SECRET}"),
        })]);
        mm.on_memory_write("add", "memory", &format!("remember bearer {FAKE_SECRET}"));

        let turn_messages = turn_messages.lock();
        let session_messages = session_messages.lock();
        let memory_writes = memory_writes.lock();
        assert_eq!(turn_messages.len(), 1);
        assert_eq!(session_messages.len(), 1);
        assert_eq!(memory_writes.len(), 1);
        assert_fake_secret_redacted(&turn_messages[0]);
        assert_fake_secret_redacted(&session_messages[0].to_string());
        assert_fake_secret_redacted(&memory_writes[0]);
    }

    #[test]
    fn test_session_switch_cannot_rebind_subject() {
        struct SwitchProvider {
            seen: Arc<Mutex<Vec<bool>>>,
        }

        impl MemoryProviderPlugin for SwitchProvider {
            fn name(&self) -> &str {
                "switch"
            }

            fn on_session_switch(
                &self,
                _subject: &MemorySubject,
                _filter: &MemorySubjectStorageKey,
                reset: bool,
            ) {
                self.seen.lock().push(reset);
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let (_memory_home, mut mm) = test_manager();
        mm.add_provider(Arc::new(SwitchProvider { seen: seen.clone() }));

        let bound = mm.subject().clone();
        assert!(mm.on_session_switch(&bound, true).is_ok());

        let other =
            MemorySubject::local_owner(MemorySurface::Local, "different-session").expect("subject");
        assert!(matches!(
            mm.on_session_switch(&other, false),
            Err(MemoryManagerError::SubjectMismatch)
        ));
        assert_eq!(*seen.lock(), vec![true]);
    }

    #[test]
    fn test_same_process_subjects_use_distinct_provider_filters() {
        let home = tempfile::tempdir().expect("memory home");
        let first_subject = MemorySubject::new(
            hermes_core::MemoryTenantId::new("tenant-a").expect("tenant"),
            MemoryPrincipalId::new("principal-a").expect("principal"),
            None,
            MemorySessionId::new("session-a").expect("session"),
            MemorySurface::Gateway,
            hermes_core::MemoryVisibility::Private,
        )
        .expect("first subject");
        let second_subject = MemorySubject::new(
            hermes_core::MemoryTenantId::new("tenant-a").expect("tenant"),
            MemoryPrincipalId::new("principal-b").expect("principal"),
            None,
            MemorySessionId::new("session-b").expect("session"),
            MemorySurface::Gateway,
            hermes_core::MemoryVisibility::Private,
        )
        .expect("second subject");
        let first_provider = Arc::new(RecordingProvider::new());
        let first_filters = first_provider.filters.clone();
        let second_provider = Arc::new(RecordingProvider::new());
        let second_filters = second_provider.filters.clone();

        let mut first = MemoryManager::new(first_subject, home.path()).expect("first manager");
        let mut second =
            MemoryManager::new(second_subject, home.path()).expect("second manager");
        first.add_provider(first_provider);
        second.add_provider(second_provider);
        first.prefetch_all("same query");
        second.prefetch_all("same query");

        let first_filter = first.subject_filter_key().as_str().to_string();
        let second_filter = second.subject_filter_key().as_str().to_string();
        assert_ne!(first_filter, second_filter);
        assert_eq!(*first_filters.lock(), vec![first_filter]);
        assert_eq!(*second_filters.lock(), vec![second_filter]);
    }

    #[test]
    fn test_governance_cannot_override_bound_audit_actor() {
        let (home, mut manager) = test_manager();
        manager.add_provider(Arc::new(GovernedProvider::new("builtin", 0)));
        let other_subject = MemorySubject::new(
            hermes_core::MemoryTenantId::new("tenant-a").expect("tenant"),
            MemoryPrincipalId::new("other-principal").expect("principal"),
            None,
            MemorySessionId::new("other-session").expect("session"),
            MemorySurface::Gateway,
            hermes_core::MemoryVisibility::Private,
        )
        .expect("other subject");
        let other_manager =
            MemoryManager::new(other_subject, home.path()).expect("other subject manager");
        let subject = manager.subject().clone();
        assert!(matches!(
            manager.forget_subject(&subject, other_manager.audit_actor()),
            Err(MemoryManagerError::AuditActorMismatch)
        ));
        assert!(
            std::fs::read_to_string(
                home.path()
                    .join("memory-governance")
                    .join("memory-audit-v1.jsonl"),
            )
            .expect("audit ledger")
            .is_empty(),
            "actor mismatch must fail before any governance audit or provider effect"
        );
    }

    #[test]
    fn test_governance_requires_all_provider_receipts_and_absent_readback() {
        let (home, mut complete) = test_manager();
        complete.add_provider(Arc::new(GovernedProvider::new("builtin", 0)));
        complete.add_provider(Arc::new(GovernedProvider::new("governed", 0)));
        let subject = complete.subject().clone();
        let actor = complete.audit_actor().clone();
        assert!(complete
            .export_subject(&subject, &actor)
            .expect("export report")
            .is_complete());
        assert!(complete
            .retention_sweep(
                &subject,
                &MemoryRetentionPolicy::new(Some(30), Some(100)).expect("policy"),
                &actor,
            )
            .expect("retention report")
            .is_complete());
        assert!(complete
            .forget_subject(&subject, &actor)
            .expect("forget report")
            .is_erased());

        let audit_path = home
            .path()
            .join("memory-governance")
            .join("memory-audit-v1.jsonl");
        let audit = std::fs::read_to_string(audit_path).expect("read audit");
        assert!(!audit.contains("manager-test-session"));
        let terminal = audit
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("audit json"))
            .find(|entry| {
                entry.get("kind").and_then(Value::as_str) == Some("governance_terminal")
                    && entry.get("action").and_then(Value::as_str) == Some("forget")
                    && entry.get("provider").and_then(Value::as_str) == Some("governed")
            })
            .expect("governed terminal receipt");
        assert_eq!(
            terminal.get("receipt_count").and_then(Value::as_u64),
            Some(2)
        );
        assert_eq!(
            terminal.get("residual_records").and_then(Value::as_u64),
            Some(0)
        );
        assert!(terminal
            .get("receipt_digest")
            .and_then(Value::as_str)
            .is_some_and(|digest| digest.starts_with("sha256:")));

        let mut residual = MemoryManager::new(
            MemorySubject::local_owner(MemorySurface::Local, "residual-session")
                .expect("residual subject"),
            home.path(),
        )
        .expect("residual manager");
        residual.add_provider(Arc::new(GovernedProvider::new("builtin", 0)));
        residual.add_provider(Arc::new(GovernedProvider::new("governed", 1)));
        let residual_subject = residual.subject().clone();
        let residual_actor = residual.audit_actor().clone();
        match residual.forget_subject(&residual_subject, &residual_actor) {
            Err(MemoryManagerError::ForgetIncomplete { report }) => {
                assert!(!report.is_erased());
                assert_eq!(report.providers.len(), 2);
                assert_eq!(
                    report
                        .providers
                        .iter()
                        .find(|entry| entry.provider == "governed")
                        .and_then(|entry| entry.readback.as_ref())
                        .expect("residual receipt")
                        .residual_records,
                    1
                );
            }
            other => panic!("expected incomplete forget report, got {other:?}"),
        }
        let audit = std::fs::read_to_string(
            home.path()
                .join("memory-governance")
                .join("memory-audit-v1.jsonl"),
        )
        .expect("read terminal audit");
        let partial_terminal = audit
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("audit json"))
            .rev()
            .find(|entry| {
                entry.get("kind").and_then(Value::as_str) == Some("governance_terminal")
                    && entry.get("action").and_then(Value::as_str) == Some("forget")
                    && entry.get("provider").and_then(Value::as_str) == Some("governed")
                    && entry.get("residual_records").and_then(Value::as_u64) == Some(1)
            })
            .expect("partial terminal audit");
        assert_eq!(
            partial_terminal.get("outcome").and_then(Value::as_str),
            Some("verified_residual")
        );
    }

    #[test]
    fn test_committed_export_opens_only_for_bound_subject_actor_and_verified_bytes() {
        let (home, mut manager) = test_manager();
        manager.add_provider(Arc::new(GovernedProvider::new("builtin", 0)));
        manager.add_provider(Arc::new(GovernedProvider::new("governed", 0)));
        let subject = manager.subject().clone();
        let actor = manager.audit_actor().clone();
        let report = manager
            .export_subject(&subject, &actor)
            .expect("committed populated export");
        let artifact = report.artifact.expect("committed export artifact");

        let mut stream = manager
            .open_export_subject(&subject, &actor, &artifact)
            .expect("authenticated verified export stream");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let data_path = home.path().join("memory-governance").join("memory-exports").join(
                format!("{}.jsonl", artifact.artifact_ref.as_str()),
            );
            let replaced_path = data_path.with_extension("replaced");
            std::fs::rename(&data_path, &replaced_path)
                .expect("replace path after verified open");
            std::fs::write(&data_path, b"tampered").expect("write replacement export");
            std::fs::set_permissions(&data_path, std::fs::Permissions::from_mode(0o600))
                .expect("make replacement fixture owner-only");
        }
        let mut bytes = String::new();
        stream
            .read_to_string(&mut bytes)
            .expect("read verified export bytes");
        assert!(bytes.contains("\"kind\":\"memory_export_stream\""));
        assert!(bytes.contains("\"kind\":\"provider_page\""));
        assert!(bytes.contains("\"ordinal\":0"));
        let audit = std::fs::read_to_string(
            home.path()
                .join("memory-governance")
                .join("memory-audit-v1.jsonl"),
        )
        .expect("read export audit");
        assert!(audit.contains("export_artifact_opened"));
        assert!(audit.contains("\"outcome\":\"committed\""));
        assert!(!audit.contains("manager-test-session"));
        assert!(!audit.contains("\"ordinal\""));
        drop(stream);

        let other_subject = MemorySubject::local_owner(MemorySurface::Local, "other-session")
            .expect("other subject");
        assert!(matches!(
            manager.open_export_subject(&other_subject, &actor, &artifact),
            Err(MemoryManagerError::SubjectMismatch)
        ));
        let other_manager =
            MemoryManager::new(other_subject, home.path()).expect("other subject manager");
        assert!(matches!(
            manager.open_export_subject(&subject, other_manager.audit_actor(), &artifact),
            Err(MemoryManagerError::AuditActorMismatch)
        ));

        let wrong_ref = MemoryExportArtifact::new(
            MemoryExportArtifactRef::from_opaque(format!("mea1_{}", "0".repeat(64)))
                .expect("canonical non-existent artifact reference"),
            artifact.manifest_digest.clone(),
            artifact.record_count,
            artifact.page_count,
            artifact.provider_count,
        );
        assert!(matches!(
            manager.open_export_subject(&subject, &actor, &wrong_ref),
            Err(MemoryManagerError::ExportArtifact)
        ));

        #[cfg(unix)]
        assert!(matches!(
            manager.open_export_subject(&subject, &actor, &artifact),
            Err(MemoryManagerError::ExportArtifact)
        ));
    }

    #[test]
    fn test_verified_forget_revokes_prior_export_artifact_access() {
        let (_home, mut manager) = test_manager();
        manager.add_provider(Arc::new(GovernedProvider::new("builtin", 0)));
        manager.add_provider(Arc::new(GovernedProvider::new("governed", 0)));
        let subject = manager.subject().clone();
        let actor = manager.audit_actor().clone();
        let artifact = manager
            .export_subject(&subject, &actor)
            .expect("export before forget")
            .artifact
            .expect("export artifact");
        manager
            .forget_subject(&subject, &actor)
            .expect("verified provider erase");
        assert!(matches!(
            manager.open_export_subject(&subject, &actor, &artifact),
            Err(MemoryManagerError::ExportArtifact)
        ));
    }

    #[test]
    fn test_expected_provider_inventory_survives_restart_and_blocks_absent_provider() {
        let home = tempfile::tempdir().expect("memory home");
        let subject = MemorySubject::local_owner(MemorySurface::Local, "restart-session")
            .expect("restart subject");
        {
            let mut first =
                MemoryManager::new(subject.clone(), home.path()).expect("first manager");
            first.add_provider(Arc::new(GovernedProvider::new("builtin", 0)));
            first.add_provider(Arc::new(GovernedProvider::new("previously-used", 0)));
        }

        let mut restarted =
            MemoryManager::new(subject, home.path()).expect("restarted manager");
        restarted.add_provider(Arc::new(GovernedProvider::new("builtin", 0)));
        let requested_subject = restarted.subject().clone();
        let actor = restarted.audit_actor().clone();
        match restarted.forget_subject(&requested_subject, &actor) {
            Err(MemoryManagerError::ForgetIncomplete { report }) => {
                assert!(!report.is_erased());
                let missing = report
                    .providers
                    .iter()
                    .find(|entry| entry.provider == "previously-used")
                    .expect("missing expected provider receipt");
                assert!(missing.forget.is_none());
                assert!(missing.readback.is_none());
                assert!(missing.error.is_some());
            }
            other => panic!("expected absent-provider failure, got {other:?}"),
        }

        restarted.add_provider(Arc::new(GovernedProvider::new("previously-used", 0)));
        assert!(restarted
            .forget_subject(&requested_subject, &actor)
            .expect("reattached provider erase")
            .is_erased());

        let inventory = std::fs::read_to_string(
            home.path()
                .join("memory-governance")
                .join(MEMORY_PROVIDER_INVENTORY_FILE),
        )
        .expect("provider inventory");
        assert!(!inventory.contains("restart-session"));
        assert!(inventory.contains("\"provider\":\"builtin\""));
        assert!(inventory.contains("\"provider\":\"previously-used\""));
    }

    #[test]
    fn test_governance_fails_closed_for_expected_but_unavailable_provider() {
        let (_home, mut manager) = test_manager();
        manager.add_provider(Arc::new(GovernedProvider::new("builtin", 0)));
        manager.add_provider(Arc::new(GovernedProvider::unavailable("temporarily-offline")));
        let subject = manager.subject().clone();
        let actor = manager.audit_actor().clone();
        match manager.forget_subject(&subject, &actor) {
            Err(MemoryManagerError::ForgetIncomplete { report }) => {
                let unavailable = report
                    .providers
                    .iter()
                    .find(|entry| entry.provider == "temporarily-offline")
                    .expect("unavailable expected provider receipt");
                assert!(unavailable.forget.is_none());
                assert!(unavailable.readback.is_none());
                assert!(unavailable.error.is_some());
            }
            other => panic!("expected unavailable-provider failure, got {other:?}"),
        }
    }


    #[test]
    fn fresh_owner_only_governance_directory_provisions_subject_state() {
        let home = tempfile::tempdir().expect("memory home");
        let subject = MemorySubject::local_owner(MemorySurface::Local, "fresh-owner-only-state")
            .expect("subject");
        let manager =
            MemoryManager::new(subject, home.path()).expect("fresh state directory must be valid");
        assert!(home
            .path()
            .join("memory-governance")
            .join("memory-subject-key-v1.bin")
            .is_file());
        drop(manager);
    }
    #[test]
    fn test_corrupt_provider_inventory_fails_closed_on_restart() {
        let home = tempfile::tempdir().expect("memory home");
        let subject = MemorySubject::local_owner(MemorySurface::Local, "corrupt-inventory-session")
            .expect("subject");
        let _manager =
            MemoryManager::new(subject.clone(), home.path()).expect("initial manager");
        let inventory_path = home
            .path()
            .join("memory-governance")
            .join(MEMORY_PROVIDER_INVENTORY_FILE);
        std::fs::write(&inventory_path, b"{not-json}\n").expect("corrupt inventory");
        assert!(matches!(
            MemoryManager::new(subject, home.path()),
            Err(MemoryManagerError::SubjectState)
        ));
    }

    #[test]
    fn test_missing_existing_subject_state_fails_closed() {
        for missing_file in [
            "memory-subject-key-v1.bin",
            "memory-audit-v1.jsonl",
            MEMORY_PROVIDER_INVENTORY_FILE,
        ] {
            let home = tempfile::tempdir().expect("memory home");
            let subject = MemorySubject::local_owner(MemorySurface::Local, "missing-state-session")
                .expect("subject");
            let manager =
                MemoryManager::new(subject.clone(), home.path()).expect("initial manager");
            drop(manager);
            std::fs::remove_file(home.path().join("memory-governance").join(missing_file))
                .expect("remove durable state");
            assert!(
                matches!(
                    MemoryManager::new(subject, home.path()),
                    Err(MemoryManagerError::SubjectState)
                ),
                "missing {missing_file} must fail closed"
            );
        }
    }

    #[test]
    fn test_governance_audit_append_failure_never_reports_erasure() {
        struct AuditAppendFailProvider {
            audit_path: std::path::PathBuf,
            moved_audit_path: std::path::PathBuf,
        }

        impl MemoryProviderPlugin for AuditAppendFailProvider {
            fn name(&self) -> &str {
                "builtin"
            }

            fn forget_subject(
                &self,
                _subject: &MemorySubject,
                filter: &MemorySubjectStorageKey,
                _actor: &MemoryAuditActor,
            ) -> Result<MemoryForgetReceipt, MemoryProviderGovernanceError> {
                std::fs::rename(&self.audit_path, &self.moved_audit_path).map_err(|_| {
                    MemoryProviderGovernanceError::failed(
                        self.name(),
                        MemoryGovernanceOperation::Forget,
                    )
                })?;
                Ok(MemoryForgetReceipt::new(self.name(), filter.clone(), 1))
            }

            fn verify_subject_absent(
                &self,
                _subject: &MemorySubject,
                filter: &MemorySubjectStorageKey,
            ) -> Result<MemoryReadbackReceipt, MemoryProviderGovernanceError> {
                Ok(MemoryReadbackReceipt::absent(self.name(), filter.clone()))
            }
        }

        let (home, mut manager) = test_manager();
        let audit_path = home
            .path()
            .join("memory-governance")
            .join("memory-audit-v1.jsonl");
        let moved_audit_path = home.path().join("audit-request-before-terminal.jsonl");
        manager.add_provider(Arc::new(AuditAppendFailProvider {
            audit_path: audit_path.clone(),
            moved_audit_path: moved_audit_path.clone(),
        }));
        let subject = manager.subject().clone();
        let actor = manager.audit_actor().clone();
        assert!(matches!(
            manager.forget_subject(&subject, &actor),
            Err(MemoryManagerError::ForgetIncomplete { .. })
        ));

        let records = std::fs::read_to_string(moved_audit_path)
            .expect("preserved request audit")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("audit json"))
            .collect::<Vec<_>>();
        let governance_id = records
            .iter()
            .find(|entry| {
                entry.get("kind").and_then(Value::as_str) == Some("governance_request")
                    && entry.get("action").and_then(Value::as_str) == Some("forget")
            })
            .and_then(|entry| entry.get("governance_id"))
            .and_then(Value::as_str)
            .expect("durable request id");
        assert!(!records.iter().any(|entry| {
            entry.get("kind").and_then(Value::as_str) == Some("governance_terminal")
                && entry.get("governance_id").and_then(Value::as_str) == Some(governance_id)
        }));
    }

    #[test]
    fn test_memory_nudge() {
        let (_memory_home, mut mm) = test_manager();
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
        let _guard = FUSION_ENV_LOCK.lock();
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
        let _guard = FUSION_ENV_LOCK.lock();
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
    fn prepare_builtin_memory_tool_call_rejects_missing_or_conflicting_provider_without_dispatch() {
        struct ConflictingMemoryProvider {
            name_calls: Arc<std::sync::atomic::AtomicUsize>,
            dispatches: Arc<std::sync::atomic::AtomicUsize>,
        }

        impl MemoryProviderPlugin for ConflictingMemoryProvider {
            fn name(&self) -> &str {
                self.name_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                "builtin"
            }

            fn get_tool_schemas(&self) -> Vec<Value> {
                vec![serde_json::json!({"name": "memory"})]
            }

            fn handle_tool_call(
                &self,
                _subject: &MemorySubject,
                _filter: &MemorySubjectStorageKey,
                _tool_name: &str,
                _args: &Value,
            ) -> String {
                self.dispatches
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                "unexpected dispatch".to_string()
            }
        }

        let (missing_home, missing) = test_manager();
        let args = serde_json::json!({
            "action": "add",
            "target": "memory",
            "content": "must-not-mutate",
        });
        assert!(matches!(
            missing.prepare_builtin_memory_tool_call(&args),
            Err(MemoryManagerError::BuiltinMemoryToolUnavailable)
        ));
        assert!(!missing_home.path().join("memories").exists());

        let (conflicting_home, mut conflicting) = test_manager();
        let name_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dispatches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        conflicting.add_provider(Arc::new(ConflictingMemoryProvider {
            name_calls: Arc::clone(&name_calls),
            dispatches: Arc::clone(&dispatches),
        }));
        assert_eq!(conflicting.providers().len(), 1, "spoof remains generically admissible");
        assert!(conflicting.has_tool("memory"), "spoof claims the memory schema");
        let registration_name_calls = name_calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(registration_name_calls > 0, "registration probes its display name");
        for _ in 0..2 {
            assert!(matches!(
                conflicting.prepare_builtin_memory_tool_call(&args),
                Err(MemoryManagerError::BuiltinMemoryToolUnavailable)
            ));
        }
        assert_eq!(
            name_calls.load(std::sync::atomic::Ordering::SeqCst),
            registration_name_calls,
            "canonical preparation must use pointer identity, never provider.name()"
        );
        assert_eq!(
            dispatches.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "preparation must not dispatch a spoofed provider"
        );
        assert!(!conflicting_home.path().join("memories").exists());
    }

    #[test]
    fn typed_builtin_admission_marks_exact_memory_tool_trusted() {
        let (home, mut manager) = test_manager();
        assert!(!manager.has_trusted_builtin_memory_tool());

        manager.add_builtin_provider(Arc::new(
            BuiltinMemoryPlugin::with_home(home.path().to_path_buf())
                .expect("explicit builtin home"),
        ));

        assert!(manager.has_trusted_builtin_memory_tool());
    }

    #[test]
    fn prepare_builtin_memory_tool_call_writes_content_free_requested_audit() {
        let home = tempfile::tempdir().expect("memory home");
        let subject = MemorySubject::local_owner(MemorySurface::Local, "prepared-builtin-owner")
            .expect("authenticated subject");
        let mut manager =
            MemoryManager::new(subject, home.path()).expect("subject-bound memory manager");
        manager.add_builtin_provider(Arc::new(
            BuiltinMemoryPlugin::with_home(home.path().to_path_buf())
                .expect("explicit builtin home"),
        ));
        manager.initialize_all().expect("bind builtin provider");

        let args = serde_json::json!({
            "action": "add",
            "target": "memory",
            "content": "prepared-memory-content",
        });
        let prepared = manager
            .prepare_builtin_memory_tool_call(&args)
            .expect("canonical builtin preparation");
        assert_eq!(prepared, redact_memory_value(&args));
        assert!(
            !home.path().join("memories").exists(),
            "preparation must not mutate the captured backend"
        );

        let audit = std::fs::read_to_string(
            home.path()
                .join("memory-governance")
                .join("memory-audit-v1.jsonl"),
        )
        .expect("durable audit");
        assert!(audit.lines().any(|line| {
            let event: Value = serde_json::from_str(line).expect("audit JSON");
            event.get("kind").and_then(Value::as_str) == Some("memory_access")
                && event.get("action").and_then(Value::as_str) == Some("write")
                && event.get("provider").and_then(Value::as_str) == Some("builtin")
                && event.get("outcome").and_then(Value::as_str) == Some("requested")
        }));
        assert!(!audit.contains("prepared-memory-content"));
    }

    #[test]
    fn builtin_memory_tool_dispatch_rejects_unbound_identities_and_audits_before_mutation() {
        let home = tempfile::tempdir().expect("memory home");
        let subject = MemorySubject::local_owner(MemorySurface::Local, "builtin-tool-owner")
            .expect("authenticated local subject");
        let mut manager =
            MemoryManager::new(subject, home.path()).expect("subject-bound memory manager");
        let plugin = Arc::new(
            BuiltinMemoryPlugin::with_home(home.path().to_path_buf())
                .expect("explicit builtin home"),
        );
        manager.add_builtin_provider(plugin.clone());
        manager.initialize_all().expect("bind builtin provider");
        assert!(manager.has_tool("memory"));
        assert_eq!(manager.get_all_tool_schemas().len(), 1);

        let bound_subject = manager.subject().clone();
        let bound_filter = manager.subject_filter_key().clone();
        let other_subject = MemorySubject::local_owner(MemorySurface::Local, "other-tool-owner")
            .expect("other authenticated subject");
        let other_filter = other_subject.storage_key(
            &hermes_core::MemorySubjectDerivationKey::from_bytes([73_u8; 32]),
        );
        let add_args = serde_json::json!({
            "action": "add",
            "target": "memory",
            "content": "bound-tool-entry",
        });

        let wrong_subject = MemoryProviderPlugin::handle_tool_call(
            plugin.as_ref(),
            &other_subject,
            &bound_filter,
            "memory",
            &add_args,
        );
        assert_eq!(
            serde_json::from_str::<Value>(&wrong_subject)
                .expect("binding rejection JSON")
                .get("error")
                .and_then(Value::as_str),
            Some("memory_subject_binding_rejected")
        );
        let wrong_filter = MemoryProviderPlugin::handle_tool_call(
            plugin.as_ref(),
            &bound_subject,
            &other_filter,
            "memory",
            &add_args,
        );
        assert_eq!(
            serde_json::from_str::<Value>(&wrong_filter)
                .expect("filter rejection JSON")
                .get("error")
                .and_then(Value::as_str),
            Some("memory_subject_binding_rejected")
        );
        assert!(!home
            .path()
            .join("memories")
            .join("subjects")
            .join(other_filter.as_str())
            .exists());

        let audit_path = home
            .path()
            .join("memory-governance")
            .join("memory-audit-v1.jsonl");
        let unavailable_audit_path = home.path().join("audit-unavailable.jsonl");
        std::fs::rename(&audit_path, &unavailable_audit_path)
            .expect("make audit store unavailable");
        let failed = manager.handle_tool_call("memory", &add_args);
        assert_eq!(
            serde_json::from_str::<Value>(&failed)
                .expect("audit failure JSON")
                .get("error")
                .and_then(Value::as_str),
            Some("memory_audit_unavailable")
        );
        let memory_path = home
            .path()
            .join("memories")
            .join("subjects")
            .join(bound_filter.as_str())
            .join("MEMORY.md");
        assert!(
            !memory_path.exists(),
            "backend mutation must not occur when its requested audit cannot persist"
        );
        std::fs::rename(&unavailable_audit_path, &audit_path)
            .expect("restore audit store");

        let result = manager.handle_tool_call("memory", &add_args);
        assert!(
            !result.contains("\"error\""),
            "bound builtin tool must execute through MemoryHandler: {result}"
        );
        assert!(memory_path.exists(), "bound backend mutation");
        let audit = std::fs::read_to_string(audit_path).expect("read durable audit");
        assert!(
            audit.lines().any(|line| {
                let event: Value = serde_json::from_str(line).expect("audit JSON");
                event.get("kind").and_then(Value::as_str) == Some("memory_access")
                    && event.get("action").and_then(Value::as_str) == Some("write")
                    && event.get("provider").and_then(Value::as_str) == Some("builtin")
                    && event.get("outcome").and_then(Value::as_str) == Some("requested")
            }),
            "requested audit event must be durable before builtin mutation"
        );
        assert!(
            !audit.contains("bound-tool-entry"),
            "audit ledger must remain content-free"
        );
    }
    #[test]
    fn test_query_terms_filters_short_tokens() {
        let terms = query_terms("go rust dl tokio memory");
        assert_eq!(terms, vec!["rust", "tokio", "memory"]);
    }
}
