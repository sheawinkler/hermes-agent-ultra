//! Subject-bound adapter for Hermes' local MEMORY.md / USER.md backend.
//!
//! The file backend receives an explicit Hermes home during manager
//! initialization. It never discovers a home from the environment and never
//! accepts a caller-selected subject partition.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hermes_core::{
    MemoryAuditActor, MemoryExportReceipt, MemoryExportSink, MemoryForgetReceipt,
    MemoryGovernanceOperation, MemoryProviderGovernanceError, MemoryReadbackReceipt,
    MemoryRetentionPolicy, MemoryRetentionReceipt, MemorySubject, MemorySubjectStorageKey,
    ToolHandler,
};
use hermes_tools::{FileMemoryBackend, MemoryHandler};
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

struct BoundBuiltinMemoryBackend {
    subject: MemorySubject,
    filter: MemorySubjectStorageKey,
}

/// Built-in subject-scoped file-memory governance provider.
///
/// A manager creates one instance and binds it exactly once per initialization.
/// Every governed operation checks that same immutable binding before reaching
/// the underlying filesystem backend.
pub struct BuiltinMemoryPlugin {
    hermes_home: PathBuf,
    backend: Arc<FileMemoryBackend>,
    bound: Mutex<Option<BoundBuiltinMemoryBackend>>,
}

impl BuiltinMemoryPlugin {
    /// Construct an adapter for exactly one caller-resolved Hermes home.
    pub fn with_home(hermes_home: PathBuf) -> Result<Self, String> {
        if hermes_home.as_os_str().is_empty() {
            return Err("builtin memory provider requires an explicit Hermes home".into());
        }
        Ok(Self {
            backend: Arc::new(FileMemoryBackend::with_home(hermes_home.clone())),
            hermes_home,
            bound: Mutex::new(None),
        })
    }

    fn bound_backend(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        operation: MemoryGovernanceOperation,
    ) -> Result<Arc<FileMemoryBackend>, MemoryProviderGovernanceError> {
        let bound = self.bound.lock().map_err(|_| {
            MemoryProviderGovernanceError::failed(Self::NAME, operation)
        })?;
        let Some(bound) = bound.as_ref() else {
            return Err(MemoryProviderGovernanceError::failed(Self::NAME, operation));
        };
        if &bound.subject != subject || &bound.filter != filter {
            return Err(MemoryProviderGovernanceError::failed(Self::NAME, operation));
        }
        Ok(Arc::clone(&self.backend))
    }

    const NAME: &'static str = "builtin";
}

impl MemoryProviderPlugin for BuiltinMemoryPlugin {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn initialize(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        hermes_home: &str,
    ) -> Result<(), String> {
        if PathBuf::from(hermes_home) != self.hermes_home {
            return Err("builtin memory provider home does not match its manager binding".into());
        }
        let mut bound = self
            .bound
            .lock()
            .map_err(|_| "builtin memory provider binding lock is unavailable".to_string())?;
        match bound.as_ref() {
            Some(existing) if existing.subject == *subject && existing.filter == *filter => Ok(()),
            Some(_) => Err(
                "builtin memory provider cannot be rebound to a different authenticated subject"
                    .to_string(),
            ),
            None => {
                *bound = Some(BoundBuiltinMemoryBackend {
                    subject: subject.clone(),
                    filter: filter.clone(),
                });
                Ok(())
            }
        }
    }

    /// Expose the one canonical `memory` schema. The handler is deliberately
    /// unbound here: binding happens only after the manager verifies this
    /// provider's immutable authenticated subject at dispatch time.
    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![
            serde_json::to_value(MemoryHandler::new(self.backend.clone()).schema())
                .expect("canonical memory tool schema must serialize"),
        ]
    }

    /// Execute only the canonical memory tool through `MemoryHandler`, after
    /// proving that the caller is this provider's original subject/filter
    /// binding. Tool JSON cannot supply or override that binding.
    fn handle_tool_call(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        tool_name: &str,
        args: &Value,
    ) -> String {
        let backend = match self.bound_backend(subject, filter, MemoryGovernanceOperation::Write)
        {
            Ok(backend) => backend,
            Err(_) => return json!({"error": "memory_subject_binding_rejected"}).to_string(),
        };
        if tool_name != "memory" {
            return json!({"error": "unsupported_memory_tool"}).to_string();
        }
        let handler = MemoryHandler::with_subject_filter(backend, filter.clone());
        match futures::executor::block_on(handler.execute(args.clone())) {
            Ok(result) => result,
            Err(_) => json!({"error": "memory_operation_failed"}).to_string(),
        }
    }

    fn export_subject(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _actor: &MemoryAuditActor,
        sink: &mut dyn MemoryExportSink,
    ) -> Result<MemoryExportReceipt, MemoryProviderGovernanceError> {
        let backend = match self.bound_backend(subject, filter, MemoryGovernanceOperation::Export) {
            Ok(backend) => backend,
            Err(error) => {
                sink.abort();
                return Err(error);
            }
        };
        backend.export_subject(filter, sink)
    }

    fn forget_subject(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryForgetReceipt, MemoryProviderGovernanceError> {
        self.bound_backend(subject, filter, MemoryGovernanceOperation::Forget)?
            .forget_subject(filter)
    }

    fn retention_sweep(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        policy: &MemoryRetentionPolicy,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryRetentionReceipt, MemoryProviderGovernanceError> {
        self.bound_backend(subject, filter, MemoryGovernanceOperation::RetentionSweep)?
            .retention_sweep(filter, policy)
    }

    fn verify_subject_absent(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<MemoryReadbackReceipt, MemoryProviderGovernanceError> {
        self.bound_backend(subject, filter, MemoryGovernanceOperation::Readback)?
            .verify_subject_absent(filter)
    }

    fn shutdown(&self) {
        if let Ok(mut bound) = self.bound.lock() {
            *bound = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(label: &str) -> (MemorySubject, MemorySubjectStorageKey) {
        let subject = MemorySubject::local_owner(
            hermes_core::MemorySurface::Local,
            format!("builtin-{label}"),
        )
        .expect("subject");
        let key = subject.storage_key(&hermes_core::MemorySubjectDerivationKey::from_bytes(
            [61_u8; 32],
        ));
        (subject, key)
    }

    #[test]
    fn initialize_is_idempotent_but_rejects_a_different_authenticated_binding() {
        let home = tempfile::tempdir().expect("home");
        let provider =
            BuiltinMemoryPlugin::with_home(home.path().to_path_buf()).expect("explicit home");
        let (alice, alice_key) = subject("alice");
        let (bob, bob_key) = subject("bob");

        MemoryProviderPlugin::initialize(
            &provider,
            &alice,
            &alice_key,
            home.path().to_str().expect("UTF-8 home"),
        )
        .expect("first binding");
        MemoryProviderPlugin::initialize(
            &provider,
            &alice,
            &alice_key,
            home.path().to_str().expect("UTF-8 home"),
        )
        .expect("same binding remains idempotent");
        let error = MemoryProviderPlugin::initialize(
            &provider,
            &bob,
            &bob_key,
            home.path().to_str().expect("UTF-8 home"),
        )
        .expect_err("different authenticated subject must not rebind provider");
        assert!(error.contains("cannot be rebound"));
        assert!(matches!(
            MemoryProviderPlugin::verify_subject_absent(&provider, &bob, &bob_key),
            Err(MemoryProviderGovernanceError::Failed {
                operation: MemoryGovernanceOperation::Readback,
                ..
            })
        ));
        assert!(
            MemoryProviderPlugin::verify_subject_absent(&provider, &alice, &alice_key)
                .expect("original binding retained")
                .is_absent()
        );
    }
}
