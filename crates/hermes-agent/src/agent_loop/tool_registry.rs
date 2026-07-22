#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolBatchInterrupt {
    None,
    Stop,
    Steer(String),
}

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolExecutionTrust {
    Untrusted,
    Observation,
    VettedLocalOneShot,
}

impl ToolExecutionTrust {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::Observation => "observation",
            Self::VettedLocalOneShot => "vetted_local_one_shot",
        }
    }
}

/// A single tool entry in the registry.
#[derive(Clone)]
pub struct ToolEntry {
    /// The tool's JSON Schema descriptor.
    pub schema: ToolSchema,
    /// A handler function: takes a JSON Value and returns the tool output string.
    pub handler: Arc<dyn Fn(Value) -> Result<String, ToolError> + Send + Sync>,
    pub(crate) execution_trust: ToolExecutionTrust,
}

impl std::fmt::Debug for ToolEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolEntry")
            .field("schema", &self.schema)
            .field("handler", &"<function>")
            .field("execution_trust", &self.execution_trust)
            .finish()
    }
}

#[derive(Clone)]
struct RuntimeToolBindingSource {
    registry: Arc<hermes_tools::ToolRegistry>,
    definition: hermes_tools::RuntimeToolDefinition,
}

impl std::fmt::Debug for RuntimeToolBindingSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeToolBindingSource")
            .field("schema", self.definition.schema())
            .finish_non_exhaustive()
    }
}

fn runtime_dispatch_result(
    outcome: hermes_tools::RuntimeDispatchOutcome,
) -> Result<String, ToolError> {
    match outcome {
        hermes_tools::RuntimeDispatchOutcome::Returned(output) => Ok(output),
        hermes_tools::RuntimeDispatchOutcome::RejectedBeforeInvoke(error) => {
            Err(ToolError::ExecutionFailed(error))
        }
        hermes_tools::RuntimeDispatchOutcome::FailedAfterInvoke(error) => {
            Err(ToolError::InvalidParams(error))
        }
        hermes_tools::RuntimeDispatchOutcome::UnknownAfterInvoke(error) => {
            Err(ToolError::OutcomeUnknown(error))
        }
    }
}

/// A simple registry mapping tool names to their schemas and handlers.
///
/// The full-featured implementation lives in `hermes-tools`; this minimal
/// version exists so the agent loop can be tested and used independently.
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
    /// Exact containment binding shared with the owning `AgentLoop`.
    execution_containment: Arc<Mutex<Arc<ExecutionContainmentContext>>>,
    /// Opaque live registrations retained only so the canonical memory tool
    /// can receive a per-agent subject binding without detaching provenance.
    runtime_binding_sources: HashMap<String, RuntimeToolBindingSource>,
    canonical_memory_bound: bool,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        let context = Arc::new(
            ExecutionContainmentContext::manual(hermes_core::ResolvedExecutionMode::default())
                .expect("the default execution mode is manual"),
        );
        Self {
            tools: HashMap::new(),
            execution_containment: Arc::new(Mutex::new(context)),
            runtime_binding_sources: HashMap::new(),
            canonical_memory_bound: false,
        }
    }

    /// Set the exact containment context shared by policy/handler dispatch
    /// and the owning loop.
    pub fn set_execution_containment_context(
        &self,
        context: Arc<ExecutionContainmentContext>,
    ) {
        *self
            .execution_containment
            .lock()
            .expect("agent tool registry containment lock poisoned") = context;
    }

    /// Snapshot the exact containment context used by this registry.
    pub fn execution_containment_context(&self) -> Arc<ExecutionContainmentContext> {
        Arc::clone(
            &self
                .execution_containment
                .lock()
                .expect("agent tool registry containment lock poisoned"),
        )
    }

    /// Register a tool.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn Fn(Value) -> Result<String, ToolError> + Send + Sync>,
    ) {
        let name = name.into();
        self.runtime_binding_sources.remove(&name);
        if name == "memory" {
            self.canonical_memory_bound = false;
        }
        self.tools.insert(
            name,
            ToolEntry {
                schema,
                handler,
                execution_trust: ToolExecutionTrust::Untrusted,
            },
        );
    }

    #[cfg(test)]
    pub(crate) fn register_test_observation(
        &mut self,
        name: impl Into<String>,
        schema: ToolSchema,
        handler: Arc<dyn Fn(Value) -> Result<String, ToolError> + Send + Sync>,
    ) {
        let name = name.into();
        self.runtime_binding_sources.remove(&name);
        if name == "memory" {
            self.canonical_memory_bound = false;
        }
        self.tools.insert(
            name,
            ToolEntry {
                schema,
                handler,
                execution_trust: ToolExecutionTrust::Observation,
            },
        );
    }

    /// Register an opaque `hermes-tools` definition without detaching its
    /// handler from the registration identity that authorized it.
    pub fn register_runtime_tool(
        &mut self,
        registry: Arc<hermes_tools::ToolRegistry>,
        definition: hermes_tools::RuntimeToolDefinition,
    ) {
        let schema = definition.schema().clone();
        let name = schema.name.clone();
        let execution_trust = match definition.vetted_effect(&registry) {
            Some(hermes_tools::VettedToolEffect::Observation) => ToolExecutionTrust::Observation,
            Some(hermes_tools::VettedToolEffect::LocalOneShot) => {
                ToolExecutionTrust::VettedLocalOneShot
            }
            None => ToolExecutionTrust::Untrusted,
        };
        let binding_source = RuntimeToolBindingSource {
            registry: Arc::clone(&registry),
            definition: definition.clone(),
        };
        let handler = Arc::new(move |params: Value| {
            runtime_dispatch_result(registry.dispatch_runtime(&definition, params))
        });
        self.tools.insert(
            name.clone(),
            ToolEntry {
                schema,
                handler,
                execution_trust,
            },
        );
        self.runtime_binding_sources.insert(name.clone(), binding_source);
        if name == "memory" {
            self.canonical_memory_bound = false;
        }
    }
    /// Replace only the canonical built-in memory dispatch with a per-agent
    /// subject binding. The live Hermes Tools registry still owns policy,
    /// capability provenance, containment, filtering, and output handling.
    pub(crate) fn bind_canonical_memory_manager(
        &mut self,
        manager: Arc<std::sync::Mutex<MemoryManager>>,
    ) -> Result<(), ToolError> {
        let source = self
            .runtime_binding_sources
            .get("memory")
            .cloned()
            .ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "Canonical memory runtime definition is unavailable".to_string(),
                )
            })?;
        let (hermes_home, subject_filter) = {
            let manager = manager.lock().map_err(|_| {
                ToolError::ExecutionFailed("Memory manager lock is unavailable".to_string())
            })?;
            (
                manager.hermes_home().to_path_buf(),
                manager.subject_filter_key().clone(),
            )
        };
        let expected_subject_filter = subject_filter.clone();
        let bound = source.registry.bind_canonical_memory_subject(
            &source.definition,
            &hermes_home,
            subject_filter,
        )?;
        if bound.vetted_effect() != Some(hermes_tools::VettedToolEffect::LocalOneShot) {
            return Err(ToolError::ExecutionFailed(
                "Canonical memory provenance changed before subject binding".to_string(),
            ));
        }
        let schema = bound.schema().clone();
        let entry = self.tools.get_mut("memory").ok_or_else(|| {
            ToolError::ExecutionFailed("Canonical memory tool is unavailable".to_string())
        })?;
        if entry.schema != schema
            || entry.execution_trust != ToolExecutionTrust::VettedLocalOneShot
        {
            return Err(ToolError::ExecutionFailed(
                "Canonical memory tool metadata does not match its runtime definition".to_string(),
            ));
        }
        entry.handler = Arc::new(move |params: Value| {
            let prepared = {
                let manager = manager.lock().map_err(|_| {
                    ToolError::ExecutionFailed("Memory manager lock is unavailable".to_string())
                })?;
                if manager.hermes_home() != hermes_home
                    || manager.subject_filter_key() != &expected_subject_filter
                {
                    return Err(ToolError::ExecutionFailed(
                        "Memory manager binding changed after canonical tool admission".to_string(),
                    ));
                }
                manager
                    .prepare_builtin_memory_tool_call(&params)
                    .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?
            };
            runtime_dispatch_result(bound.dispatch(prepared))
        });
        self.canonical_memory_bound = true;
        Ok(())
    }

    pub(crate) fn canonical_memory_bound(&self) -> bool {
        self.canonical_memory_bound
    }

    pub(crate) fn disable_canonical_memory(&mut self) {
        self.tools.remove("memory");
        self.runtime_binding_sources.remove("memory");
        self.canonical_memory_bound = false;
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&ToolEntry> {
        self.tools.get(name)
    }

    /// Return all registered tool schemas.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|e| e.schema.clone()).collect()
    }

    /// Return all registered tool names.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub(crate) fn provenance_entries(&self) -> Vec<(String, &'static str)> {
        let mut entries = self
            .tools
            .iter()
            .map(|(name, entry)| (name.clone(), entry.execution_trust.as_str()))
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    pub(crate) fn execution_trust_for(&self, name: &str) -> Option<&'static str> {
        self.tools
            .get(name)
            .map(|entry| entry.execution_trust.as_str())
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
