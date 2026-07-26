use super::*;
use crate::memory_manager::{MemoryManager, MemoryProviderPlugin};
use futures::stream::BoxStream as GovernanceBoxStream;
use hermes_core::{
    MemoryAuditActor, MemoryExportCompletion, MemoryExportEvidenceDigest, MemoryExportPage,
    MemoryExportReceipt, MemoryExportSink, MemoryExportSnapshot, MemoryForgetReceipt,
    MemoryProviderGovernanceError, MemoryReadbackReceipt, MemorySubject,
    MemorySubjectStorageKey,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as GovernanceMutex;

struct NoopLlm;

#[async_trait::async_trait]
impl LlmProvider for NoopLlm {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        unreachable!("memory governance does not invoke the LLM")
    }

    fn chat_completion_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> GovernanceBoxStream<'static, Result<StreamChunk, AgentError>> {
        Box::pin(futures::stream::empty())
    }
}
struct MemoryBridgeTerminal;

#[async_trait::async_trait]
impl hermes_core::TerminalBackend for MemoryBridgeTerminal {
    async fn execute_command(
        &self,
        _command: &str,
        _timeout: Option<u64>,
        _workdir: Option<&str>,
        _background: bool,
        _pty: bool,
    ) -> Result<hermes_core::CommandOutput, AgentError> {
        Ok(hermes_core::CommandOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    async fn read_file(
        &self,
        _path: &str,
        _offset: Option<u64>,
        _limit: Option<u64>,
    ) -> Result<String, AgentError> {
        Ok(String::new())
    }

    async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
        Ok(())
    }

    async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
        Ok(false)
    }

    async fn list_processes(&self) -> Result<serde_json::Value, AgentError> {
        Ok(serde_json::json!([]))
    }
}

struct MemoryBridgeSkills;

#[async_trait::async_trait]
impl hermes_core::SkillProvider for MemoryBridgeSkills {
    async fn create_skill(
        &self,
        name: &str,
        content: &str,
        category: Option<&str>,
    ) -> Result<hermes_core::Skill, AgentError> {
        Ok(hermes_core::Skill {
            name: name.to_string(),
            content: content.to_string(),
            category: category.map(str::to_string),
            description: None,
        })
    }

    async fn get_skill(&self, _name: &str) -> Result<Option<hermes_core::Skill>, AgentError> {
        Ok(None)
    }

    async fn list_skills(&self) -> Result<Vec<hermes_core::SkillMeta>, AgentError> {
        Ok(Vec::new())
    }

    async fn update_skill(
        &self,
        name: &str,
        content: &str,
    ) -> Result<hermes_core::Skill, AgentError> {
        Ok(hermes_core::Skill {
            name: name.to_string(),
            content: content.to_string(),
            category: None,
            description: None,
        })
    }

    async fn delete_skill(&self, _name: &str) -> Result<(), AgentError> {
        Ok(())
    }
}

#[derive(Default)]
struct ProviderStore {
    records: GovernanceMutex<HashMap<MemorySubjectStorageKey, u64>>,
    actors: GovernanceMutex<Vec<MemorySubjectStorageKey>>,
}

struct GovernedStoreProvider {
    provider: &'static str,
    store: Arc<ProviderStore>,
    residual_after_forget: Option<u64>,
}

impl GovernedStoreProvider {
    fn verified_absent(provider: &'static str, store: Arc<ProviderStore>) -> Self {
        Self {
            provider,
            store,
            residual_after_forget: None,
        }
    }

    fn leaves_residue(provider: &'static str, store: Arc<ProviderStore>, residual: u64) -> Self {
        Self {
            provider,
            store,
            residual_after_forget: Some(residual),
        }
    }
}

impl MemoryProviderPlugin for GovernedStoreProvider {
    fn name(&self) -> &str {
        self.provider
    }

    fn export_subject(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        actor: &MemoryAuditActor,
        sink: &mut dyn MemoryExportSink,
    ) -> Result<MemoryExportReceipt, MemoryProviderGovernanceError> {
        self.store
            .actors
            .lock()
            .expect("actor store")
            .push(actor.opaque_id().clone());
        let exported = self
            .store
            .records
            .lock()
            .expect("record store")
            .get(filter)
            .copied()
            .unwrap_or(0);
        let snapshot = MemoryExportSnapshot::new(
            MemoryExportEvidenceDigest::sha256_bytes(
                format!("{}:{}", self.provider, filter.as_str()).as_bytes(),
            ),
            MemoryExportEvidenceDigest::sha256_bytes(
                format!("revision:{}:{}", self.provider, filter.as_str()).as_bytes(),
            ),
            None,
        );
        sink.begin_provider(snapshot.clone()).map_err(|_| {
            MemoryProviderGovernanceError::failed(
                self.provider,
                hermes_core::MemoryGovernanceOperation::Export,
            )
        })?;
        let page_count = u64::from(exported > 0);
        if exported > 0 {
            let record_ids = (0..exported)
                .map(|index| {
                    MemoryExportEvidenceDigest::sha256_bytes(
                        format!("{}:{}:{index}", self.provider, filter.as_str()).as_bytes(),
                    )
                })
                .collect();
            let records = (0..exported)
                .map(|index| serde_json::json!({ "test_record": index }))
                .collect();
            let page = MemoryExportPage::new(None, None, record_ids, records).map_err(|_| {
                MemoryProviderGovernanceError::failed(
                    self.provider,
                    hermes_core::MemoryGovernanceOperation::Export,
                )
            })?;
            sink.append_page(page).map_err(|_| {
                MemoryProviderGovernanceError::failed(
                    self.provider,
                    hermes_core::MemoryGovernanceOperation::Export,
                )
            })?;
        }
        sink.finish_provider(MemoryExportCompletion::exhausted(exported, page_count))
            .map_err(|_| {
                MemoryProviderGovernanceError::failed(
                    self.provider,
                    hermes_core::MemoryGovernanceOperation::Export,
                )
            })?;
        Ok(MemoryExportReceipt::exhausted(
            self.provider,
            filter.clone(),
            snapshot,
            exported,
            page_count,
        ))
    }

    fn forget_subject(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        actor: &MemoryAuditActor,
    ) -> Result<MemoryForgetReceipt, MemoryProviderGovernanceError> {
        self.store
            .actors
            .lock()
            .expect("actor store")
            .push(actor.opaque_id().clone());
        let deleted = {
            let mut records = self.store.records.lock().expect("record store");
            let deleted = records.remove(filter).unwrap_or(0);
            if let Some(residual) = self.residual_after_forget {
                records.insert(filter.clone(), residual);
            }
            deleted
        };
        Ok(MemoryForgetReceipt::new(
            self.provider,
            filter.clone(),
            deleted,
        ))
    }

    fn verify_subject_absent(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<MemoryReadbackReceipt, MemoryProviderGovernanceError> {
        let residual = self
            .store
            .records
            .lock()
            .expect("record store")
            .get(filter)
            .copied()
            .unwrap_or(0);
        Ok(if residual == 0 {
            MemoryReadbackReceipt::absent(self.provider, filter.clone())
        } else {
            MemoryReadbackReceipt::residual(self.provider, filter.clone(), residual)
        })
    }
}

fn test_agent(manager: MemoryManager) -> AgentLoop {
    AgentLoop::new(
        AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        },
        Arc::new(ToolRegistry::new()),
        Arc::new(NoopLlm),
    )
    .with_memory(Arc::new(GovernanceMutex::new(manager)))
}

#[test]
fn bound_memory_governance_is_subject_scoped_readback_verified_and_audited() {
    let home = tempfile::tempdir().expect("memory home");
    let subject_a = MemorySubject::from_authenticated_gateway(
        Some("tenant-a"),
        Some("user-a"),
        None,
        Some("gateway-a"),
        true,
    )
    .expect("direct subject");
    let subject_b = MemorySubject::from_authenticated_gateway(
        Some("tenant-a"),
        Some("user-b"),
        Some("team-b"),
        Some("gateway-b"),
        false,
    )
    .expect("group subject");
    let builtin = Arc::new(ProviderStore::default());
    let remote = Arc::new(ProviderStore::default());

    let mut manager_a = MemoryManager::new(subject_a, home.path()).expect("manager a");
    let mut manager_b = MemoryManager::new(subject_b, home.path()).expect("manager b");
    let key_a = manager_a.subject_filter_key().clone();
    let key_b = manager_b.subject_filter_key().clone();
    let actor_a = manager_a.audit_actor().opaque_id().clone();
    for manager in [&mut manager_a, &mut manager_b] {
        manager.add_provider(Arc::new(GovernedStoreProvider::verified_absent(
            "builtin",
            builtin.clone(),
        )));
        manager.add_provider(Arc::new(GovernedStoreProvider::verified_absent(
            "remote",
            remote.clone(),
        )));
    }
    for store in [&builtin, &remote] {
        let mut records = store.records.lock().expect("record store");
        records.insert(key_a.clone(), 3);
        records.insert(key_b.clone(), 5);
    }

    let agent = test_agent(manager_a);
    let export = agent.export_bound_memory().expect("bound export");
    assert!(export.is_complete());
    let artifact = export.artifact.expect("committed artifact");
    let mut file = agent
        .open_bound_memory_export(&artifact)
        .expect("verified committed export descriptor");
    let mut export_bytes = String::new();
    std::io::Read::read_to_string(&mut file, &mut export_bytes)
        .expect("read verified export descriptor");
    assert!(export_bytes.contains("\"test_record\""));
    let tampered = hermes_core::MemoryExportArtifact::new(
        artifact.artifact_ref.clone(),
        MemoryExportEvidenceDigest::sha256_bytes(b"tampered-manifest"),
        artifact.record_count,
        artifact.page_count,
        artifact.provider_count,
    );
    assert!(matches!(
        agent.open_bound_memory_export(&tampered),
        Err(MemoryManagerError::ExportArtifact)
    ));
    let report = agent.forget_bound_memory().expect("bound forget");
    assert!(report.is_erased());
    for store in [&builtin, &remote] {
        assert!(store
            .records
            .lock()
            .expect("record store")
            .get(&key_a)
            .is_none());
        assert_eq!(
            store
                .records
                .lock()
                .expect("record store")
                .get(&key_b)
                .copied(),
            Some(5)
        );
        assert!(store
            .actors
            .lock()
            .expect("actor store")
            .iter()
            .all(|actor| actor == &actor_a));
    }

    let audit = std::fs::read_to_string(
        home.path()
            .join("memory-governance")
            .join("memory-audit-v1.jsonl"),
    )
    .expect("terminal audit");
    for provider in ["builtin", "remote"] {
        assert!(audit.lines().any(|line| {
            let event: serde_json::Value = serde_json::from_str(line).expect("audit json");
            event.get("kind").and_then(serde_json::Value::as_str)
                == Some("governance_terminal")
                && event.get("action").and_then(serde_json::Value::as_str) == Some("export")
                && event.get("provider").and_then(serde_json::Value::as_str) == Some(provider)
                && event.get("outcome").and_then(serde_json::Value::as_str)
                    == Some("staged_complete")
                && event
                    .get("manifest_digest")
                    .and_then(serde_json::Value::as_str)
                    == Some(artifact.manifest_digest.as_str())
        }));
    }
    for provider in ["builtin", "remote"] {
        assert!(audit.lines().any(|line| {
            let event: serde_json::Value = serde_json::from_str(line).expect("audit json");
            event.get("kind").and_then(serde_json::Value::as_str)
                == Some("governance_terminal")
                && event.get("action").and_then(serde_json::Value::as_str) == Some("forget")
                && event.get("provider").and_then(serde_json::Value::as_str) == Some(provider)
                && event.get("outcome").and_then(serde_json::Value::as_str)
                    == Some("verified_absent")
        }));
    }
}

#[test]
fn bound_memory_forget_preserves_incomplete_readback_report() {
    let home = tempfile::tempdir().expect("memory home");
    let subject =
        MemorySubject::local_owner(hermes_core::MemorySurface::Gateway, "gateway-residue")
        .expect("subject");
    let mut manager = MemoryManager::new(subject, home.path()).expect("manager");
    let builtin = Arc::new(ProviderStore::default());
    let residual = Arc::new(ProviderStore::default());
    manager.add_provider(Arc::new(GovernedStoreProvider::verified_absent(
        "builtin",
        builtin,
    )));
    manager.add_provider(Arc::new(GovernedStoreProvider::leaves_residue(
        "remote",
        residual,
        1,
    )));

    let agent = test_agent(manager);
    let Err(MemoryManagerError::ForgetIncomplete { report }) = agent.forget_bound_memory() else {
        panic!("residual readback must not report completion");
    };
    assert!(!report.is_erased());
    assert_eq!(
        report
            .providers
            .iter()
            .find(|provider| provider.provider == "remote")
            .and_then(|provider| provider.readback.as_ref())
            .map(|readback| readback.residual_records),
        Some(1)
    );
}

#[test]
fn bound_memory_forget_fails_closed_when_a_previously_used_provider_is_missing() {
    let home = tempfile::tempdir().expect("memory home");
    let subject = MemorySubject::from_authenticated_gateway(
        Some("tenant-a"),
        Some("user-a"),
        None,
        Some("gateway-restart"),
        true,
    )
    .expect("subject");
    {
        let mut initial = MemoryManager::new(subject.clone(), home.path()).expect("initial manager");
        initial.add_provider(Arc::new(GovernedStoreProvider::verified_absent(
            "builtin",
            Arc::new(ProviderStore::default()),
        )));
        initial.add_provider(Arc::new(GovernedStoreProvider::verified_absent(
            "remote",
            Arc::new(ProviderStore::default()),
        )));
    }

    let mut restarted = MemoryManager::new(subject, home.path()).expect("restarted manager");
    restarted.add_provider(Arc::new(GovernedStoreProvider::verified_absent(
        "builtin",
        Arc::new(ProviderStore::default()),
    )));
    let agent = test_agent(restarted);
    let Err(MemoryManagerError::ForgetIncomplete { report }) = agent.forget_bound_memory() else {
        panic!("missing expected provider must not report completion");
    };
    let missing = report
        .providers
        .iter()
        .find(|provider| provider.provider == "remote")
        .expect("remote provider status");
    assert!(missing.forget.is_none());
    assert!(missing.readback.is_none());
    assert!(matches!(
        &missing.error,
        Some(MemoryProviderGovernanceError::Failed { .. })
    ));
}

#[test]
fn bound_memory_forget_fails_closed_when_terminal_audit_persistence_fails() {
    struct AuditFailureProvider {
        audit_path: std::path::PathBuf,
        moved_audit_path: std::path::PathBuf,
    }

    impl MemoryProviderPlugin for AuditFailureProvider {
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
                    hermes_core::MemoryGovernanceOperation::Forget,
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

    let home = tempfile::tempdir().expect("memory home");
    let subject = MemorySubject::from_authenticated_gateway(
        Some("tenant-a"),
        Some("user-a"),
        None,
        Some("gateway-audit"),
        true,
    )
    .expect("subject");
    let audit_path = home
        .path()
        .join("memory-governance")
        .join("memory-audit-v1.jsonl");
    let moved_audit_path = home.path().join("audit-request-before-terminal.jsonl");
    let mut manager = MemoryManager::new(subject, home.path()).expect("manager");
    manager.add_provider(Arc::new(AuditFailureProvider {
        audit_path,
        moved_audit_path,
    }));

    let agent = test_agent(manager);
    let Err(MemoryManagerError::ForgetIncomplete { report }) = agent.forget_bound_memory() else {
        panic!("audit persistence failure must not report completion");
    };
    assert!(!report.is_erased());
    assert!(report
        .providers
        .iter()
        .all(|provider| provider.error.is_some()));
}

#[test]
fn bound_memory_forget_requires_an_attached_manager() {
    let agent = AgentLoop::new(
        AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        },
        Arc::new(ToolRegistry::new()),
        Arc::new(NoopLlm),
    );
    assert!(matches!(
        agent.forget_bound_memory(),
        Err(MemoryManagerError::SubjectState)
    ));
}

#[test]
fn bound_memory_governance_rejects_rebound_manager() {
    let home = tempfile::tempdir().expect("memory home");
    let attached_subject =
        MemorySubject::local_owner(hermes_core::MemorySurface::Gateway, "attached-subject")
            .expect("attached subject");
    let rebound_subject =
        MemorySubject::local_owner(hermes_core::MemorySurface::Gateway, "rebound-subject")
            .expect("rebound subject");
    let manager = MemoryManager::new(attached_subject, home.path()).expect("attached manager");
    let rebound = MemoryManager::new(rebound_subject, home.path()).expect("rebound manager");
    let mut agent = test_agent(manager);
    agent.memory_manager = Some(Arc::new(GovernanceMutex::new(rebound)));

    assert!(matches!(
        agent.forget_bound_memory(),
        Err(MemoryManagerError::SubjectMismatch)
    ));
}

#[tokio::test]
async fn governed_builtin_memory_manager_allows_a_normal_agent_run() {
    struct ReplyingLlm;

    #[async_trait::async_trait]
    impl LlmProvider for ReplyingLlm {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Ok(LlmResponse {
                message: Message::assistant("done"),
                usage: None,
                model: "test".to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> GovernanceBoxStream<'static, Result<StreamChunk, AgentError>> {
            Box::pin(futures::stream::empty())
        }
    }

    let home = tempfile::tempdir().expect("memory home");
    let subject =
        MemorySubject::local_owner(hermes_core::MemorySurface::Cli, "builtin-normal-run")
            .expect("local memory subject");
    let mut manager = MemoryManager::new(subject, home.path()).expect("memory manager");
    manager.add_builtin_provider(Arc::new(
        crate::memory_plugins::builtin::BuiltinMemoryPlugin::with_home(home.path().to_path_buf())
            .expect("builtin provider"),
    ));
    manager.initialize_all().expect("initialize builtin provider");

    let (agent, _run_state) = attach_test_run_runtime(
        AgentLoop::new(
            AgentConfig {
                provider: Some("test-provider".to_string()),
                skip_memory: false,
                skip_context_files: true,
                hermes_home: Some(home.path().to_string_lossy().to_string()),
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(ReplyingLlm),
        )
        .with_memory(Arc::new(GovernanceMutex::new(manager))),
    );

    let result = agent
        .run(vec![Message::user("hello")], None)
        .await
        .expect("governed builtin memory must not block a normal run");
    assert!(result.finished_naturally);
}

#[tokio::test(flavor = "multi_thread")]
async fn canonical_memory_bridge_binds_registry_manager_file_and_audit_end_to_end() {
    let home = tempfile::tempdir().expect("memory home");
    let runtime_registry = Arc::new(hermes_tools::ToolRegistry::new());
    runtime_registry
        .set_policy(hermes_tools::ToolPolicyEngine::new(hermes_tools::ToolPolicyMode::Off));
    hermes_tools::register_builtin_tools_with_home(
        runtime_registry.as_ref(),
        Arc::new(MemoryBridgeTerminal),
        Arc::new(MemoryBridgeSkills),
        home.path().to_path_buf(),
    );
    let memory_definition = runtime_registry
        .runtime_definitions()
        .into_iter()
        .find(|definition| definition.schema().name == "memory")
        .expect("canonical runtime memory definition");
    let mut agent_registry = ToolRegistry::new();
    agent_registry.register_runtime_tool(Arc::clone(&runtime_registry), memory_definition);

    let subject =
        MemorySubject::local_owner(hermes_core::MemorySurface::Cli, "bridge-subject")
            .expect("authenticated memory subject");
    let mut manager = MemoryManager::new(subject, home.path()).expect("memory manager");
    manager.add_builtin_provider(Arc::new(
        crate::memory_plugins::builtin::BuiltinMemoryPlugin::with_home(home.path().to_path_buf())
            .expect("builtin provider"),
    ));
    manager.initialize_all().expect("initialize builtin provider");
    let subject_key = manager.subject_filter_key().clone();
    let manager = Arc::new(GovernanceMutex::new(manager));

    let agent = AgentLoop::new(
        AgentConfig {
            skip_memory: false,
            skip_context_files: true,
            hermes_home: Some(home.path().to_string_lossy().to_string()),
            ..AgentConfig::default()
        },
        Arc::new(agent_registry),
        Arc::new(NoopLlm),
    )
    .with_memory(Arc::clone(&manager));
    assert!(agent.tool_registry.canonical_memory_bound());

    let explicit_without_memory = agent
        .prepare_run_input(vec![Message::user("no memory tools")], Some(Vec::new()))
        .await
        .expect("explicit empty tool filter");
    assert!(explicit_without_memory.advertised_tools.is_empty());
    let canonical_schema = agent
        .tool_registry
        .get("memory")
        .expect("bound memory entry")
        .schema
        .clone();
    let explicit_with_memory = agent
        .prepare_run_input(
            vec![Message::user("memory enabled")],
            Some(vec![canonical_schema.clone()]),
        )
        .await
        .expect("explicit canonical memory schema");
    assert_eq!(
        explicit_with_memory.advertised_tools,
        vec![canonical_schema.clone()]
    );

    let mut spoofed_schema = canonical_schema.clone();
    spoofed_schema.description = "spoofed memory capability".to_string();
    assert!(matches!(
        agent
            .prepare_run_input(
                vec![Message::user("spoofed memory")],
                Some(vec![spoofed_schema]),
            )
            .await,
        Err(AgentError::Config(message))
            if message.contains("does not match the subject-bound canonical tool")
    ));

    let entry = agent
        .tool_registry
        .get("memory")
        .expect("subject-bound memory entry")
        .clone();
    let output = (entry.handler)(serde_json::json!({
        "action": "add",
        "target": "memory",
        "content": "bridge-subject-marker",
    }))
    .expect("canonical bound memory dispatch");
    assert!(!output.contains("\"error\""), "unexpected tool output: {output}");

    let memory_file = home
        .path()
        .join("memories/subjects")
        .join(subject_key.as_str())
        .join("MEMORY.md");
    assert!(std::fs::read_to_string(&memory_file)
        .expect("subject memory file")
        .contains("bridge-subject-marker"));
    let audit = std::fs::read_to_string(
        home.path()
            .join("memory-governance")
            .join("memory-audit-v1.jsonl"),
    )
    .expect("memory audit");
    assert!(audit.lines().any(|line| {
        let event: serde_json::Value = serde_json::from_str(line).expect("audit JSON");
        event.get("kind").and_then(serde_json::Value::as_str) == Some("memory_access")
            && event.get("action").and_then(serde_json::Value::as_str) == Some("write")
            && event.get("provider").and_then(serde_json::Value::as_str) == Some("builtin")
            && event.get("outcome").and_then(serde_json::Value::as_str) == Some("requested")
    }));
    assert!(!audit.contains("bridge-subject-marker"));

    let replacement_subject =
        MemorySubject::local_owner(hermes_core::MemorySurface::Cli, "replacement-subject")
            .expect("replacement subject");
    let mut replacement =
        MemoryManager::new(replacement_subject, home.path()).expect("replacement manager");
    replacement.add_builtin_provider(Arc::new(
        crate::memory_plugins::builtin::BuiltinMemoryPlugin::with_home(home.path().to_path_buf())
            .expect("replacement builtin"),
    ));
    replacement
        .initialize_all()
        .expect("initialize replacement builtin");
    let replacement_key = replacement.subject_filter_key().clone();
    *manager.lock().expect("manager lock") = replacement;
    let error = (entry.handler)(serde_json::json!({
        "action": "add",
        "target": "memory",
        "content": "replacement-must-not-write",
    }))
    .expect_err("manager replacement must invalidate the bound handler");
    assert!(error.to_string().contains("binding changed"));
    assert!(!home
        .path()
        .join("memories/subjects")
        .join(replacement_key.as_str())
        .join("MEMORY.md")
        .exists());

    let mut child_config = AgentConfig {
        skip_memory: true,
        skip_context_files: true,
        hermes_home: Some(home.path().to_string_lossy().to_string()),
        ..AgentConfig::default()
    };
    child_config.execution_mode = agent.config.execution_mode;
    let child = AgentLoop::new(
        child_config,
        Arc::clone(&agent.tool_registry),
        Arc::new(NoopLlm),
    );
    let child_input = child
        .prepare_run_input(vec![Message::user("delegated child")], None)
        .await
        .expect("skip-memory child input");
    assert!(child_input
        .advertised_tools
        .iter()
        .all(|schema| schema.name != "memory"));
    let skipped_call = ToolCall {
        id: "skip-memory-call".to_string(),
        function: hermes_core::FunctionCall {
            name: "memory".to_string(),
            arguments: serde_json::json!({
                "action": "add",
                "target": "memory",
                "content": "skip-guard-must-not-write",
            })
            .to_string(),
        },
        extra_content: None,
    };
    let mut child_errors = Vec::new();
    let child_results = child
        .execute_tool_calls(
            &[skipped_call],
            1,
            1,
            &[],
            false,
            None,
            None,
            &mut child_errors,
        )
        .await
        .expect("skip-memory execution result");
    assert_eq!(child_results.len(), 1);
    assert!(child_results[0].is_error);
    assert_eq!(
        child_results[0].content,
        "Memory tools are disabled for this agent invocation."
    );
    assert_eq!(child_errors.len(), 1);
    assert!(!home
        .path()
        .join("memories/subjects")
        .join(replacement_key.as_str())
        .join("MEMORY.md")
        .exists());
}
