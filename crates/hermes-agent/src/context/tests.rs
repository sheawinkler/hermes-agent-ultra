#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_get_messages() {
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::system("You are helpful"));
        cm.add_message(Message::user("Hello"));

        assert_eq!(cm.len(), 2);
        assert_eq!(cm.get_messages()[0].role, MessageRole::System);
        assert_eq!(cm.get_messages()[1].role, MessageRole::User);
    }

    #[test]
    fn test_reset() {
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::user("Hello"));
        assert!(!cm.is_empty());

        cm.reset();
        assert!(cm.is_empty());
    }

    #[test]
    fn test_truncate_preserves_system_messages() {
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::system("System prompt"));

        for i in 0..50 {
            cm.add_message(Message::assistant(format!("Response {i}")));
        }
        cm.add_message(Message::user("Final question"));

        let budget = BudgetConfig {
            max_result_size_chars: 100_000,
            max_aggregate_chars: 200,
        };
        cm.truncate_to_budget(&budget);

        // System prompt should still be first
        assert_eq!(cm.get_messages()[0].role, MessageRole::System);
        // Last message should be the user question
        assert_eq!(cm.get_messages().last().unwrap().role, MessageRole::User);
    }

    // ---- ContextCompressor tests ----

    #[test]
    fn test_compressor_no_op_when_small() {
        let compressor = ContextCompressor::new(4);
        let msgs = vec![
            Message::system("You are helpful"),
            Message::user("Hi"),
            Message::assistant("Hello!"),
        ];
        let result = compressor.compress(&msgs);
        // Only 3 messages total, recent_count=4 covers all, middle is empty => no-op.
        assert_eq!(result.len(), msgs.len());
    }

    #[test]
    fn test_compressor_replaces_middle_with_summary() {
        let compressor = ContextCompressor::new(2);
        let msgs = vec![
            Message::system("System prompt"),
            Message::user("Question 1"),
            Message::assistant("Answer 1"),
            Message::user("Question 2"),
            Message::assistant("Answer 2"),
            Message::user("Question 3"),
            Message::assistant("Answer 3"),
        ];

        let result = compressor.compress(&msgs);

        // Expected layout: [system prompt] [summary] [last 2 messages]
        assert!(result.len() <= msgs.len());
        // First message is still the system prompt.
        assert_eq!(result[0].role, MessageRole::System);
        assert_eq!(result[0].content.as_deref(), Some("System prompt"));
        // Second message is the summary (also System role).
        assert_eq!(result[1].role, MessageRole::System);
        assert!(result[1]
            .content
            .as_deref()
            .unwrap_or("")
            .contains("[Conversation summary]"));
        // Last 2 messages preserved.
        assert_eq!(result[result.len() - 2].role, MessageRole::User);
        assert_eq!(result[result.len() - 1].role, MessageRole::Assistant);
    }

    #[test]
    fn test_context_manager_compress_under_threshold() {
        // With 10k budget, 80% = 8k. Short messages won't trigger compression.
        let mut cm = ContextManager::new(10_000);
        cm.add_message(Message::system("System prompt"));
        cm.add_message(Message::user("Hi"));
        cm.add_message(Message::assistant("Hello!"));

        let len_before = cm.len();
        cm.compress();
        assert_eq!(cm.len(), len_before);
    }

    #[test]
    fn test_context_manager_compress_over_threshold() {
        // Budget large enough for the summary + recent messages, but small
        // enough that the 80% threshold (240 chars) is exceeded by the
        // full conversation.
        let mut cm = ContextManager::with_compressor(
            300, // 300 chars budget => 80% threshold = 240 chars
            ContextCompressor::new(2),
        );
        cm.add_message(Message::system("System prompt"));
        // Add enough messages to exceed 240 chars.
        for i in 0..10 {
            cm.add_message(Message::user(format!(
                "This is question number {i} with enough text to be long"
            )));
            cm.add_message(Message::assistant(format!(
                "This is answer number {i} also fairly long to fill up the budget"
            )));
        }
        cm.add_message(Message::user("Short q"));
        cm.add_message(Message::assistant("Short a"));

        let len_before = cm.len();
        assert!(
            cm.total_chars() > 240,
            "should be over threshold before compress"
        );
        cm.compress();
        // After compression the message count should be smaller.
        assert!(
            cm.len() < len_before,
            "compression should reduce message count"
        );
        // System prompt preserved.
        assert_eq!(
            cm.get_messages()[0].content.as_deref(),
            Some("System prompt")
        );
        // A summary message should appear after the system prompt.
        assert!(cm.get_messages().len() >= 2);
        assert_eq!(cm.get_messages()[1].role, MessageRole::System);
        assert!(cm.get_messages()[1]
            .content
            .as_deref()
            .unwrap_or("")
            .contains("[Conversation summary]"));
    }

    #[test]
    fn test_build_summary_truncates() {
        let msgs: Vec<Message> = (0..100)
            .map(|i| Message::user(format!("Message number {i} with some extra content")))
            .collect();
        let summary = ContextCompressor::build_summary(&msgs);
        // Summary must be capped at ~2048 chars.
        assert!(summary.len() <= 2100, "summary should be roughly capped");
        assert!(summary.contains("[Conversation summary]"));
    }

    // ---- SOUL.md and SystemPromptBuilder tests ----

    #[test]
    fn test_load_soul_md_from_nonexistent() {
        let result = load_soul_md_from(Path::new("/tmp/nonexistent/SOUL.md"));
        assert!(result.is_none());
    }

    #[test]
    fn test_load_soul_md_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        let soul_path = tmp.path().join("SOUL.md");
        std::fs::write(&soul_path, "You are a pirate assistant.").unwrap();

        let result = load_soul_md_from(&soul_path);
        assert_eq!(result.as_deref(), Some("You are a pirate assistant."));
    }

    #[test]
    fn test_load_soul_md_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let soul_path = tmp.path().join("SOUL.md");
        std::fs::write(&soul_path, "   ").unwrap();

        let result = load_soul_md_from(&soul_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_load_soul_md_ignores_legacy_template() {
        let tmp = tempfile::tempdir().unwrap();
        let soul_path = tmp.path().join("SOUL.md");
        std::fs::write(
            &soul_path,
            format!("\u{feff}{LEGACY_INSTALLER_SOUL_TEMPLATE}\r\n"),
        )
        .unwrap();

        assert!(is_legacy_template_soul(
            &std::fs::read_to_string(&soul_path).unwrap()
        ));
        assert!(load_soul_md_from(&soul_path).is_none());
    }

    #[test]
    fn test_legacy_template_detection_preserves_custom_persona() {
        let custom = format!("{LEGACY_INSTALLER_SOUL_TEMPLATE}\nYou are a helpful pirate.");

        assert!(!is_legacy_template_soul(&custom));
    }

    #[test]
    fn test_load_soul_md_from_home_uses_explicit_home() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("SOUL.md"), "Use explicit home.").unwrap();

        let result = load_soul_md_from_home(Some(tmp.path().to_string_lossy().as_ref()));

        assert_eq!(result.as_deref(), Some("Use explicit home."));
    }

    #[test]
    fn test_ensure_default_soul_md_creates_default() {
        let tmp = tempfile::tempdir().unwrap();

        let outcome = ensure_default_soul_md(tmp.path()).unwrap();

        assert_eq!(outcome, SoulSeedOutcome::Created);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("SOUL.md")).unwrap(),
            DEFAULT_AGENT_IDENTITY
        );
    }

    #[test]
    fn test_ensure_default_soul_md_upgrades_legacy_template() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("SOUL.md"),
            format!("{LEGACY_UPSTREAM_SOUL_TEMPLATE_WITH_EXAMPLES}\n"),
        )
        .unwrap();

        let outcome = ensure_default_soul_md(tmp.path()).unwrap();

        assert_eq!(outcome, SoulSeedOutcome::UpgradedLegacy);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("SOUL.md")).unwrap(),
            DEFAULT_AGENT_IDENTITY
        );
    }

    #[test]
    fn test_ensure_default_soul_md_preserves_custom_persona() {
        let tmp = tempfile::tempdir().unwrap();
        let soul_path = tmp.path().join("SOUL.md");
        std::fs::write(&soul_path, "Custom persona.").unwrap();

        let outcome = ensure_default_soul_md(tmp.path()).unwrap();

        assert_eq!(outcome, SoulSeedOutcome::Preserved);
        assert_eq!(
            std::fs::read_to_string(soul_path).unwrap(),
            "Custom persona."
        );
    }

    #[test]
    fn test_resolve_personality_builtin() {
        let coder = resolve_personality("coder", None).unwrap_or_default();
        assert!(coder.contains("`coder` persona"));
    }

    #[test]
    fn test_resolve_personality_prefers_user_file() {
        let tmp = tempfile::tempdir().unwrap();
        let personalities_dir = tmp.path().join("personalities");
        std::fs::create_dir_all(&personalities_dir).unwrap();
        std::fs::write(
            personalities_dir.join("coder.md"),
            "Custom coder persona from user profile.",
        )
        .unwrap();

        let resolved = resolve_personality("coder", Some(tmp.path().to_string_lossy().as_ref()))
            .unwrap_or_default();
        assert_eq!(resolved, "Custom coder persona from user profile.");
    }

    #[test]
    fn test_load_context_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx_dir = tmp.path().join("context");
        std::fs::create_dir_all(&ctx_dir).unwrap();
        std::fs::write(ctx_dir.join("01-rules.md"), "Rule 1: Be helpful").unwrap();
        std::fs::write(ctx_dir.join("02-style.txt"), "Use formal tone").unwrap();
        std::fs::write(ctx_dir.join("ignored.json"), "{}").unwrap();

        let content = load_context_files(tmp.path());
        assert!(content.contains("Rule 1: Be helpful"));
        assert!(content.contains("Use formal tone"));
        assert!(!content.contains("{}"));
    }

    #[test]
    fn test_load_context_files_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let content = load_context_files(tmp.path());
        assert!(content.is_empty());
    }

    #[test]
    fn test_system_prompt_builder() {
        let mut builder = SystemPromptBuilder::new()
            .with_personality(Some("You are Hermes."))
            .with_system_message("Be concise.")
            .with_memory_context("<memory-context>User likes Rust</memory-context>")
            .with_timestamp(Some("gpt-4o"), None);

        let prompt = builder.build();
        assert!(prompt.contains("You are Hermes."));
        assert!(prompt.contains("Be concise."));
        assert!(prompt.contains("User likes Rust"));
        assert!(prompt.contains("gpt-4o"));
    }

    #[test]
    fn test_system_prompt_builder_default_identity() {
        let mut builder = SystemPromptBuilder::new().with_personality(None);

        let prompt = builder.build();
        assert!(prompt.contains("Hermes"));
    }

    #[test]
    fn test_system_prompt_builder_caching() {
        let mut builder = SystemPromptBuilder::new().with_personality(Some("Test"));

        // First build
        let p1 = builder.build().to_string();
        // Second build should return cached
        let p2 = builder.build().to_string();
        assert_eq!(p1, p2);

        // Invalidate
        builder.invalidate();
        assert!(builder.cached().is_none());

        // Rebuild
        let p3 = builder.build().to_string();
        assert_eq!(p1, p3);
    }

    #[test]
    fn test_system_prompt_builder_skips_empty() {
        let mut builder = SystemPromptBuilder::new()
            .with_personality(Some("Identity"))
            .with_system_message("")
            .with_memory_context("   ")
            .with_tool_guidance("")
            .with_skills_prompt("");

        let prompt = builder.build();
        assert_eq!(prompt, "Identity");
    }

    fn builtin_subject(label: &str) -> (MemorySubject, MemorySubjectStorageKey) {
        let subject =
            MemorySubject::local_owner(hermes_core::MemorySurface::Local, label).unwrap();
        let key =
            subject.storage_key(&hermes_core::MemorySubjectDerivationKey::from_bytes([17_u8; 32]));
        (subject, key)
    }

    #[cfg(unix)]
    fn owner_only(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .expect("owner-only permissions");
    }

    #[cfg(unix)]
    fn owner_only_snapshot_tree(
        base: &std::path::Path,
        subjects: &std::path::Path,
        alice: &std::path::Path,
        bob: &std::path::Path,
    ) {
        for path in [base, subjects, alice, bob] {
            owner_only(path);
        }
    }

    #[cfg(unix)]
    #[test]
    fn builtin_memory_snapshot_is_subject_scoped_and_quarantines_legacy_global_files() {
        let tmp = tempfile::tempdir().unwrap();
        let (alice, alice_key) = builtin_subject("alice");
        let (bob, bob_key) = builtin_subject("bob");
        let legacy_dir = tmp.path().join("memories");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        owner_only(&legacy_dir);
        std::fs::write(legacy_dir.join("MEMORY.md"), "legacy global secret").unwrap();

        let subjects_dir = legacy_dir.join("subjects");
        let alice_dir = subjects_dir.join(alice_key.as_str());
        let bob_dir = subjects_dir.join(bob_key.as_str());
        std::fs::create_dir_all(&alice_dir).unwrap();
        std::fs::create_dir_all(&bob_dir).unwrap();
        owner_only_snapshot_tree(tmp.path(), &subjects_dir, &alice_dir, &bob_dir);
        std::fs::write(
            alice_dir.join("MEMORY.md"),
            "Use ripgrep for search\n§\nWorkspace is Rust",
        )
        .unwrap();
        std::fs::write(alice_dir.join("USER.md"), "Name: Alice").unwrap();
        std::fs::write(bob_dir.join("MEMORY.md"), "Bob-only note").unwrap();
        for path in [
            legacy_dir.join("MEMORY.md"),
            alice_dir.join("MEMORY.md"),
            alice_dir.join("USER.md"),
            bob_dir.join("MEMORY.md"),
        ] {
            owner_only(&path);
        }

        let (alice_memory, alice_user) = load_builtin_memory_snapshot(
            Some(&alice),
            Some(&alice_key),
            Some(tmp.path().to_string_lossy().as_ref()),
        )
        .unwrap();
        let (bob_memory, bob_user) = load_builtin_memory_snapshot(
            Some(&bob),
            Some(&bob_key),
            Some(tmp.path().to_string_lossy().as_ref()),
        )
        .unwrap();

        let alice_memory = alice_memory.unwrap();
        assert!(alice_memory.contains("Use ripgrep for search"));
        assert!(alice_memory.contains("Workspace is Rust"));
        assert!(!alice_memory.contains("legacy global secret"));
        assert!(!alice_memory.contains("Bob-only note"));
        assert!(alice_user.unwrap().contains("Name: Alice"));
        assert!(bob_memory.unwrap().contains("Bob-only note"));
        assert!(bob_user.is_none());
    }

    #[test]
    fn builtin_memory_snapshot_requires_an_authenticated_binding() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            load_builtin_memory_snapshot(None, None, Some(tmp.path().to_string_lossy().as_ref())),
            Err(BuiltinMemorySnapshotUnavailable::MissingSubjectBinding)
        );
    }

    #[cfg(unix)]
    #[test]
    fn builtin_memory_snapshot_rejects_symlinked_subject_files() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside.md");
        std::fs::write(&outside, "outside secret").unwrap();
        let (subject, key) = builtin_subject("symlink");
        let directory = tmp.path().join("memories").join("subjects").join(key.as_str());
        std::fs::create_dir_all(&directory).unwrap();
        owner_only(&tmp.path().join("memories"));
        let subjects = directory.parent().expect("subjects directory");
        owner_only_snapshot_tree(tmp.path(), subjects, &directory, &directory);
        std::os::unix::fs::symlink(&outside, directory.join("MEMORY.md")).unwrap();

        assert_eq!(
            load_builtin_memory_snapshot(
                Some(&subject),
                Some(&key),
                Some(tmp.path().to_string_lossy().as_ref()),
            ),
            Err(BuiltinMemorySnapshotUnavailable::UnsafeSubjectPath)
        );
    }

    #[cfg(unix)]
    #[test]
    fn builtin_memory_snapshot_rejects_world_readable_subject_files() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let (subject, key) = builtin_subject("world-readable");
        let directory = tmp.path().join("memories").join("subjects").join(key.as_str());
        std::fs::create_dir_all(&directory).unwrap();
        let subjects = directory.parent().expect("subjects directory");
        owner_only_snapshot_tree(tmp.path(), subjects, &directory, &directory);
        owner_only(&tmp.path().join("memories"));
        let memory_file = directory.join("MEMORY.md");
        std::fs::write(&memory_file, "must not be read").unwrap();
        std::fs::set_permissions(&memory_file, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(
            load_builtin_memory_snapshot(
                Some(&subject),
                Some(&key),
                Some(tmp.path().to_string_lossy().as_ref()),
            ),
            Err(BuiltinMemorySnapshotUnavailable::UnsafeSubjectPath)
        );
    }

    #[test]
    fn test_persona_snapshot_coder() {
        let p = resolve_personality("coder", None).unwrap();
        assert!(p.contains("`coder` persona"));
        assert!(p.contains("correctness"));
    }

    #[test]
    fn test_persona_snapshot_writer() {
        let p = resolve_personality("writer", None).unwrap();
        assert!(p.contains("`writer` persona"));
        assert!(p.contains("clarity"));
    }

    #[test]
    fn test_persona_snapshot_analyst() {
        let p = resolve_personality("analyst", None).unwrap();
        assert!(p.contains("`analyst` persona"));
        assert!(p.contains("evidence"));
    }

    #[test]
    fn test_persona_snapshot_concise() {
        let p = resolve_personality("concise", None).unwrap();
        assert!(p.contains("`concise` persona"));
        assert!(p.contains("brevity"));
    }

    #[test]
    fn test_persona_snapshot_creative() {
        let p = resolve_personality("creative", None).unwrap();
        assert!(p.contains("`creative` persona"));
        assert!(p.contains("idea generation"));
    }

    #[test]
    fn test_persona_snapshot_technical() {
        let p = resolve_personality("technical", None).unwrap();
        assert!(p.contains("`technical` persona"));
        assert!(p.contains("systems-level"));
    }

    #[test]
    fn test_persona_snapshot_companion() {
        let p = resolve_personality("companion", None).unwrap();
        assert!(p.contains("`companion` persona"));
        assert!(p.contains("active listening"));
    }

    #[test]
    fn test_persona_snapshot_decision_coach() {
        let p = resolve_personality("decision-coach", None).unwrap();
        assert!(p.contains("`decision-coach` persona"));
        assert!(p.contains("trade-offs"));
    }

    #[test]
    fn test_persona_snapshot_reflective() {
        let p = resolve_personality("reflective", None).unwrap();
        assert!(p.contains("`reflective` persona"));
        assert!(p.contains("follow-up questions"));
    }

    #[test]
    fn test_persona_snapshot_security_auditor() {
        let p = resolve_personality("security-auditor", None).unwrap();
        assert!(p.contains("`security-auditor` persona"));
        assert!(p.contains("threat modeling"));
    }

    #[test]
    fn test_persona_snapshot_release_manager() {
        let p = resolve_personality("release-manager", None).unwrap();
        assert!(p.contains("`release-manager` persona"));
        assert!(p.contains("rollback"));
    }

    #[test]
    fn test_persona_snapshot_ops_sre() {
        let p = resolve_personality("ops-sre", None).unwrap();
        assert!(p.contains("`ops-sre` persona"));
        assert!(p.contains("observability"));
    }

    #[test]
    fn test_persona_snapshot_mcp_integrator() {
        let p = resolve_personality("mcp-integrator", None).unwrap();
        assert!(p.contains("`mcp-integrator` persona"));
        assert!(p.contains("protocol contracts"));
    }

    #[test]
    fn test_persona_snapshot_quant_researcher() {
        let p = resolve_personality("quant-researcher", None).unwrap();
        assert!(p.contains("`quant-researcher` persona"));
        assert!(p.contains("risk-adjusted"));
    }

    #[test]
    fn test_persona_snapshot_performance_engineer() {
        let p = resolve_personality("performance-engineer", None).unwrap();
        assert!(p.contains("`performance-engineer` persona"));
        assert!(p.contains("profiling-first"));
    }

    #[test]
    fn test_persona_snapshot_research_scout() {
        let p = resolve_personality("research-scout", None).unwrap();
        assert!(p.contains("`research-scout` persona"));
        assert!(p.contains("source quality"));
    }

    #[test]
    fn test_builtin_personality_names_contains_new_modes() {
        let names = builtin_personality_names();
        assert!(names.contains(&"companion"));
        assert!(names.contains(&"decision-coach"));
        assert!(names.contains(&"reflective"));
        assert!(names.contains(&"security-auditor"));
        assert!(names.contains(&"release-manager"));
        assert!(names.contains(&"ops-sre"));
        assert!(names.contains(&"mcp-integrator"));
        assert!(names.contains(&"quant-researcher"));
        assert!(names.contains(&"performance-engineer"));
        assert!(names.contains(&"research-scout"));
    }

    #[test]
    fn test_builtin_personality_descriptions_cover_all_names() {
        let names = builtin_personality_names();
        let descriptions = builtin_personality_descriptions();
        assert_eq!(descriptions.len(), names.len());
        for (name, desc) in descriptions {
            assert!(names.contains(name));
            assert!(!desc.trim().is_empty());
        }
    }

    #[test]
    fn test_persona_unknown_returns_none() {
        assert!(resolve_personality("pirate", None).is_none());
    }

    #[test]
    fn test_persona_default_is_neutral() {
        assert!(resolve_personality("default", None).is_none());
    }

    #[test]
    fn test_persona_system_prompt_deltas() {
        let base = {
            let mut b = SystemPromptBuilder::new().with_personality(None);
            b.build().to_string()
        };

        let coder_prompt = {
            let p = resolve_personality("coder", None).unwrap();
            let mut b = SystemPromptBuilder::new().with_personality(Some(&p));
            b.build().to_string()
        };

        assert!(!base.contains("`coder` persona"));
        assert!(coder_prompt.contains("`coder` persona"));
        assert!(coder_prompt.contains("correctness"));
    }
}
