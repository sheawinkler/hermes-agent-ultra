include!("command_cli_plugins_memory_mcp/plugins.rs");

pub async fn handle_cli_plugins(
    action: Option<String>,
    name: Option<String>,
) -> Result<(), hermes_core::AgentError> {

    match action.as_deref() {
        None | Some("list") => {
            let rows = discover_plugin_surface();
            println!("Plugin diagnostic surface ({} entries):", rows.len());
            println!("{}", render_plugin_surface_table(&rows));
        }
        Some("enable") | Some("disable") => {
            return Err(hermes_core::AgentError::PolicyBlocked(
                "third-party plugin activation is unavailable until Hermes provides a loader bound to verified package identity and the policy sandbox".into(),
            ));
        }
        Some("install") => {
            return Err(hermes_core::AgentError::PolicyBlocked(
                "raw Git and registry plugin installation is disabled; install only a v2 package through a future signed package installer".into(),
            ));
        }
        Some("remove") | Some("uninstall") => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins remove <name>".into(),
                )
            })?;
            let selected = resolve_local_plugin_path_by_name(&plugin_name)
                .map_err(hermes_core::AgentError::PolicyBlocked)?;
            let hermes_home =
                resolved_hermes_home().map_err(hermes_core::AgentError::PolicyBlocked)?;
            remove_plugin_bundle(&selected, &hermes_home)?;
            println!("Plugin '{}' removed.", plugin_name);
        }
        Some("update") => {
            return Err(hermes_core::AgentError::PolicyBlocked(
                "raw Git plugin updates are disabled; a future signed package installer must verify a new immutable package before replacing a bundle".into(),
            ));
        }
        Some("inspect") | Some("info") => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins inspect <name>".into(),
                )
            })?;
            let rows = discover_plugin_surface();
            let Some(row) = rows
                .iter()
                .find(|row| row.name.eq_ignore_ascii_case(&plugin_name))
            else {
                println!("Plugin '{}' not found in core-verified discovery.", plugin_name);
                return Ok(());
            };
            println!("Plugin: {}", row.name);
            println!("Version: {}", row.version);
            println!("Source: {}", row.source.as_str());
            println!("Status: {}", row.status.label());
            if let Some(path) = row.path.as_deref() {
                println!("Path: {}", path.display());
            }
            if !row.description.trim().is_empty() {
                println!("Detail: {}", row.description.trim());
            }
            if let Some(reason) = row.reason.as_deref() {
                println!("Reason: {}", reason);
            }
        }
        Some(other) => {
            println!("Plugins action '{}' is not recognized.", other);
            println!("Available: list, inspect, remove");
        }
    }
    Ok(())
}

include!("command_cli_plugins_memory_mcp/memory_setup.rs");

/// Command-only options for memory setup and subject-governed operations.
///
/// Setup remains provider-specific. Governance carries an explicit runtime
/// session and never turns a positional CLI argument into a principal.
#[derive(Debug, Clone, Default)]
pub struct MemoryCliOptions {
    pub setup: MemorySetupCliOptions,
    pub session_id: Option<String>,
    pub retention_days: Option<u32>,
    pub retention_max_records: Option<u64>,
}

impl MemoryCliOptions {
    pub fn setup_only(yes: bool) -> Self {
        Self {
            setup: MemorySetupCliOptions::yes_only(yes),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryRetentionExecution {
    DryRun,
    Apply,
}

fn memory_governance_subject(
    options: &MemoryCliOptions,
) -> Result<hermes_core::MemorySubject, hermes_core::AgentError> {
    let session_id = options
        .session_id
        .as_deref()
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| {
            hermes_core::AgentError::Config(
                "memory governance requires an explicit --session-id; no default subject is used."
                    .into(),
            )
        })?;
    hermes_core::MemorySubject::local_owner(hermes_core::MemorySurface::Cli, session_id)
        .map_err(|error| {
            hermes_core::AgentError::Config(format!(
                "invalid explicit memory session subject: {error}"
            ))
        })
}

fn reject_memory_governance_target(
    target: Option<&str>,
) -> Result<(), hermes_core::AgentError> {
    if target.is_some_and(|value| !value.trim().is_empty()) {
        return Err(hermes_core::AgentError::PolicyBlocked(
            "memory governance subject is authenticated at construction; positional targets cannot select another principal."
                .into(),
        ));
    }
    Ok(())
}

fn memory_retention_execution(options: &MemoryCliOptions) -> MemoryRetentionExecution {
    if options.setup.dry_run || !options.setup.yes {
        MemoryRetentionExecution::DryRun
    } else {
        MemoryRetentionExecution::Apply
    }
}

fn memory_retention_policy(
    options: &MemoryCliOptions,
) -> Result<hermes_core::MemoryRetentionPolicy, hermes_core::AgentError> {
    hermes_core::MemoryRetentionPolicy::new(
        options.retention_days,
        options.retention_max_records,
    )
    .map_err(|error| {
        hermes_core::AgentError::Config(format!(
            "memory retention requires a valid --retention-days or --retention-max-records: {error}"
        ))
    })
}

fn build_memory_governance_manager(
    subject: hermes_core::MemorySubject,
    hermes_home: &Path,
) -> Result<hermes_agent::MemoryManager, hermes_core::AgentError> {
    let mut manager = hermes_agent::MemoryManager::new(subject, hermes_home).map_err(|_| {
        hermes_core::AgentError::Config(
            "memory governance state could not be provisioned safely.".into(),
        )
    })?;
    let hermes_home = hermes_home.to_str().ok_or_else(|| {
        hermes_core::AgentError::Config(
            "memory governance requires a valid explicit Hermes home.".into(),
        )
    })?;
    hermes_agent::memory_plugins::register_builtin_then_externals(&mut manager, hermes_home)
        .map_err(|_| {
            hermes_core::AgentError::Config(
                "memory governance providers could not be registered safely.".into(),
            )
        })?;
    if manager.initialize_all().is_err() {
        manager.shutdown_all();
        return Err(hermes_core::AgentError::Config(
            "memory provider initialization failed; no governance operation was attempted.".into(),
        ));
    }
    Ok(manager)
}

fn memory_governance_error_kind(
    error: Option<&hermes_core::MemoryProviderGovernanceError>,
) -> Option<&'static str> {
    match error {
        None => None,
        Some(hermes_core::MemoryProviderGovernanceError::Unsupported { .. }) => {
            Some("unsupported")
        }
        Some(hermes_core::MemoryProviderGovernanceError::Incomplete { .. }) => {
            Some("incomplete")
        }
        Some(hermes_core::MemoryProviderGovernanceError::Failed { .. }) => Some("failed"),
    }
}

fn pretty_memory_governance_json(value: serde_json::Value) -> String {
    serde_json::to_string_pretty(&value)
        .expect("memory governance receipt values are always serializable")
}

fn render_memory_export_report(
    report: &hermes_agent::memory_manager::MemoryExportReport,
) -> String {
    let providers = report
        .providers
        .iter()
        .map(|entry| {
            serde_json::json!({
                "provider": entry.provider,
                "exported_records": entry.receipt.as_ref().map(|receipt| receipt.exported_records),
                "page_count": entry.receipt.as_ref().map(|receipt| receipt.page_count),
                "exhausted": entry.receipt.as_ref().map(|receipt| receipt.exhausted),
                "error": memory_governance_error_kind(entry.error.as_ref()),
            })
        })
        .collect::<Vec<_>>();
    let artifact = report.artifact.as_ref().map(|artifact| {
        serde_json::json!({
            "artifact_ref": artifact.artifact_ref.as_str(),
            "manifest_digest": artifact.manifest_digest.as_str(),
            "record_count": artifact.record_count,
            "page_count": artifact.page_count,
            "provider_count": artifact.provider_count,
        })
    });
    pretty_memory_governance_json(serde_json::json!({
        "operation": "export",
        "authorized_subject_key": report.subject_key.as_str(),
        "redacted": true,
        "complete": report.is_complete(),
        "artifact": artifact,
        "providers": providers,
    }))
}

fn render_memory_forget_report(
    report: &hermes_agent::memory_manager::MemoryForgetReport,
    reset_alias: bool,
) -> String {
    let providers = report
        .providers
        .iter()
        .map(|entry| {
            serde_json::json!({
                "provider": entry.provider,
                "deleted_records": entry.forget.as_ref().map(|receipt| receipt.deleted_records),
                "residual_records": entry.readback.as_ref().map(|receipt| receipt.residual_records),
                "error": memory_governance_error_kind(entry.error.as_ref()),
            })
        })
        .collect::<Vec<_>>();
    pretty_memory_governance_json(serde_json::json!({
        "operation": "forget",
        "reset_alias": reset_alias,
        "authorized_subject_key": report.subject_key.as_str(),
        "erased": report.is_erased(),
        "providers": providers,
    }))
}

fn render_memory_retention_report(
    report: &hermes_agent::memory_manager::MemoryRetentionReport,
) -> String {
    let providers = report
        .providers
        .iter()
        .map(|entry| {
            serde_json::json!({
                "provider": entry.provider,
                "deleted_records": entry
                    .retention
                    .as_ref()
                    .map(|receipt| receipt.deleted_records),
                "residual_records": entry.readback.as_ref().map(|receipt| receipt.residual_records),
                "error": memory_governance_error_kind(entry.error.as_ref()),
            })
        })
        .collect::<Vec<_>>();
    pretty_memory_governance_json(serde_json::json!({
        "operation": "retention",
        "authorized_subject_key": report.subject_key.as_str(),
        "dry_run": false,
        "complete": report.is_complete(),
        "providers": providers,
    }))
}

fn render_memory_retention_preview(
    manager: &hermes_agent::MemoryManager,
    policy: &hermes_core::MemoryRetentionPolicy,
) -> String {
    let providers = manager
        .providers()
        .iter()
        .map(|provider| serde_json::json!({ "provider": provider.name(), "status": "planned" }))
        .collect::<Vec<_>>();
    pretty_memory_governance_json(serde_json::json!({
        "operation": "retention",
        "authorized_subject_key": manager.subject_filter_key().as_str(),
        "dry_run": true,
        "policy": {
            "max_age_days": policy.max_age_days,
            "max_records": policy.max_records,
        },
        "providers": providers,
    }))
}

fn governed_memory_export(
    subject: &hermes_core::MemorySubject,
    hermes_home: &Path,
) -> Result<(), hermes_core::AgentError> {
    let manager = build_memory_governance_manager(subject.clone(), hermes_home)?;
    let result = manager.export_subject(subject, manager.audit_actor());
    manager.shutdown_all();
    match result {
        Ok(report) => {
            println!("{}", render_memory_export_report(&report));
            Ok(())
        }
        Err(hermes_agent::memory_manager::MemoryManagerError::ExportIncomplete { report }) => {
            println!("{}", render_memory_export_report(&report));
            Err(hermes_core::AgentError::PolicyBlocked(
                "memory export is incomplete across providers; provider receipts were emitted and no complete export claim was made."
                    .into(),
            ))
        }
        Err(_) => Err(hermes_core::AgentError::PolicyBlocked(
            "memory export could not be verified for the authenticated subject.".into(),
        )),
    }
}

fn governed_memory_forget(
    subject: &hermes_core::MemorySubject,
    hermes_home: &Path,
    reset_alias: bool,
) -> Result<(), hermes_core::AgentError> {
    let manager = build_memory_governance_manager(subject.clone(), hermes_home)?;
    let result = manager.forget_subject(subject, manager.audit_actor());
    manager.shutdown_all();
    match result {
        Ok(report) => {
            println!("{}", render_memory_forget_report(&report, reset_alias));
            Ok(())
        }
        Err(hermes_agent::memory_manager::MemoryManagerError::ForgetIncomplete { report }) => {
            println!("{}", render_memory_forget_report(&report, reset_alias));
            Err(hermes_core::AgentError::PolicyBlocked(
                "memory forget is incomplete across providers; residual receipts were emitted and no erasure claim was made."
                    .into(),
            ))
        }
        Err(_) => Err(hermes_core::AgentError::PolicyBlocked(
            "memory forget could not be verified for the authenticated subject; no erasure claim was made."
                .into(),
        )),
    }
}

fn governed_memory_retention(
    subject: &hermes_core::MemorySubject,
    policy: &hermes_core::MemoryRetentionPolicy,
    execution: MemoryRetentionExecution,
    hermes_home: &Path,
) -> Result<(), hermes_core::AgentError> {
    let manager = build_memory_governance_manager(subject.clone(), hermes_home)?;
    if execution == MemoryRetentionExecution::DryRun {
        println!("{}", render_memory_retention_preview(&manager, policy));
        manager.shutdown_all();
        return Ok(());
    }
    let result = manager.retention_sweep(subject, policy, manager.audit_actor());
    manager.shutdown_all();
    match result {
        Ok(report) => {
            println!("{}", render_memory_retention_report(&report));
            Ok(())
        }
        Err(hermes_agent::memory_manager::MemoryManagerError::RetentionIncomplete {
            report,
        }) => {
            println!("{}", render_memory_retention_report(&report));
            Err(hermes_core::AgentError::PolicyBlocked(
                "memory retention is incomplete across providers; provider receipts were emitted."
                    .into(),
            ))
        }
        Err(_) => Err(hermes_core::AgentError::PolicyBlocked(
            "memory retention could not be verified for the authenticated subject.".into(),
        )),
    }
}

/// Handle `hermes memory [action]`.
pub async fn handle_cli_memory(
    action: Option<String>,
    target: Option<String>,
    options: MemoryCliOptions,
) -> Result<(), hermes_core::AgentError> {
    let hermes_home = hermes_config::hermes_home();
    let memories_dir = hermes_home.join("memories");
    let memory_md = memories_dir.join("MEMORY.md");
    let user_md = memories_dir.join("USER.md");
    let disabled_marker = hermes_home.join(".memory_disabled");

    match action.as_deref().unwrap_or("status") {
        "status" => {
            println!("{}", render_memory_backend_status(&hermes_home));
        }
        "setup" => {
            let provider = target
                .as_deref()
                .map(str::trim)
                .filter(|provider| !provider.is_empty())
                .or_else(|| options.setup.has_mem0_specific_options().then_some("mem0"));
            if let Some(provider) = provider
            {
                let result = setup_memory_provider_target(provider, &options.setup)?;
                if result.dry_run {
                    println!("Dry run complete; memory provider '{}' was not written.", provider);
                    println!("  Config: {}", result.config_path.display());
                } else {
                    println!("Configured memory provider '{}'.", provider);
                    println!("  Config: {}", result.config_path.display());
                    if let Some(env_path) = result.env_path {
                        println!("  Secret env: {}", env_path.display());
                    }
                    println!(
                        "Memory provider config is owner-only and activates on subsequent sessions."
                    );
                }
                return Ok(());
            }
            println!("Memory Provider Setup");
            println!("---------------------");
            std::fs::create_dir_all(&memories_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            if !memory_md.exists() {
                std::fs::write(
                    &memory_md,
                    "# Hermes MEMORY\n\nStore durable assistant memory entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            if !user_md.exists() {
                std::fs::write(
                    &user_md,
                    "# Hermes USER\n\nStore durable user profile entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            if disabled_marker.exists() {
                let _ = std::fs::remove_file(&disabled_marker);
            }
            println!("Initialized file memory backend.");
            println!("  MEMORY.md: {}", memory_md.display());
            println!("  USER.md:   {}", user_md.display());
            println!("Memory is enabled for subsequent sessions.");
        }
        "off" => {
            std::fs::create_dir_all(&hermes_home)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            std::fs::write(
                &disabled_marker,
                format!(
                    "disabled_at={}\nreason=hermes memory off\n",
                    chrono::Utc::now().to_rfc3339()
                ),
            )
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!("Memory provider disabled.");
            println!("  Marker: {}", disabled_marker.display());
            println!("Run `hermes memory setup` to re-enable.");
        }
        "export" => {
            reject_memory_governance_target(target.as_deref())?;
            let subject = memory_governance_subject(&options)?;
            governed_memory_export(&subject, &hermes_home)?;
        }
        "forget" => {
            reject_memory_governance_target(target.as_deref())?;
            if !options.setup.yes {
                return Err(hermes_core::AgentError::Config(
                    "memory forget requires -y after an explicit --session-id.".into(),
                ));
            }
            let subject = memory_governance_subject(&options)?;
            governed_memory_forget(&subject, &hermes_home, false)?;
        }
        "retention" => {
            reject_memory_governance_target(target.as_deref())?;
            let subject = memory_governance_subject(&options)?;
            let policy = memory_retention_policy(&options)?;
            governed_memory_retention(
                &subject,
                &policy,
                memory_retention_execution(&options),
                &hermes_home,
            )?;
        }
        "reset" => {
            reject_memory_governance_target(target.as_deref())?;
            if !options.setup.yes {
                return Err(hermes_core::AgentError::Config(
                    "memory reset is an alias for verified forget and requires -y after an explicit --session-id."
                        .into(),
                ));
            }
            let subject = memory_governance_subject(&options)?;
            governed_memory_forget(&subject, &hermes_home, true)?;
        }
        other => {
            println!("Unknown memory action '{}'.", other);
            println!("Available actions: status, setup, off, export, forget, retention, reset");
        }
    }
    Ok(())
}

#[cfg(test)]
mod memory_governance_tests {
    use super::*;
    use hermes_agent::MemoryProviderPlugin;
    use hermes_core::{
        MemoryExportCompletion, MemoryExportEvidenceDigest, MemoryExportPage, MemoryExportReceipt,
        MemoryExportSink, MemoryExportSnapshot, MemoryForgetReceipt, MemoryGovernanceOperation,
        MemoryProviderGovernanceError, MemoryReadbackReceipt, MemoryRetentionReceipt,
        MemorySubject, MemorySubjectStorageKey,
    };
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    #[derive(Clone, Copy)]
    enum ProbeBehavior {
        Succeeds,
        UnsupportedForget,
        RemoteForgetFailure,
        UnsupportedExport,
    }

    #[derive(Clone)]
    struct GovernanceProbe {
        name: &'static str,
        behavior: ProbeBehavior,
        records: Arc<Mutex<u64>>,
        retention_calls: Arc<AtomicUsize>,
    }

    impl GovernanceProbe {
        fn new(name: &'static str, behavior: ProbeBehavior, records: u64) -> Self {
            Self {
                name,
                behavior,
                records: Arc::new(Mutex::new(records)),
                retention_calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn records(&self) -> u64 {
            *self.records.lock().expect("probe records lock")
        }
    }

    impl MemoryProviderPlugin for GovernanceProbe {
        fn name(&self) -> &str {
            self.name
        }

        fn export_subject(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            _actor: &hermes_core::MemoryAuditActor,
            sink: &mut dyn MemoryExportSink,
        ) -> Result<MemoryExportReceipt, MemoryProviderGovernanceError> {
            if matches!(self.behavior, ProbeBehavior::UnsupportedExport) {
                return Err(MemoryProviderGovernanceError::unsupported(
                    self.name,
                    MemoryGovernanceOperation::Export,
                ));
            }
            let record_count = self.records();
            let snapshot = MemoryExportSnapshot::new(
                MemoryExportEvidenceDigest::sha256_bytes(
                    format!("{}-snapshot", self.name).as_bytes(),
                ),
                MemoryExportEvidenceDigest::sha256_bytes(
                    format!("{}-revision", self.name).as_bytes(),
                ),
                None,
            );
            sink.begin_provider(snapshot.clone()).map_err(|_| {
                MemoryProviderGovernanceError::failed(self.name, MemoryGovernanceOperation::Export)
            })?;
            if record_count == 0 {
                sink.finish_provider(MemoryExportCompletion::exhausted(0, 0))
                    .map_err(|_| {
                        MemoryProviderGovernanceError::failed(
                            self.name,
                            MemoryGovernanceOperation::Export,
                        )
                    })?;
                return Ok(MemoryExportReceipt::exhausted(
                    self.name,
                    filter.clone(),
                    snapshot,
                    0,
                    0,
                ));
            }
            let record_ids = (0..record_count)
                .map(|index| {
                    MemoryExportEvidenceDigest::sha256_bytes(
                        format!("{}-record-{index}", self.name).as_bytes(),
                    )
                })
                .collect::<Vec<_>>();
            let records = (0..record_count)
                .map(|index| serde_json::json!({ "fixture_record": index }))
                .collect::<Vec<_>>();
            let page = MemoryExportPage::new(None, None, record_ids, records).map_err(|_| {
                MemoryProviderGovernanceError::failed(self.name, MemoryGovernanceOperation::Export)
            })?;
            sink.append_page(page).map_err(|_| {
                MemoryProviderGovernanceError::failed(self.name, MemoryGovernanceOperation::Export)
            })?;
            sink.finish_provider(MemoryExportCompletion::exhausted(record_count, 1))
                .map_err(|_| {
                    MemoryProviderGovernanceError::failed(
                        self.name,
                        MemoryGovernanceOperation::Export,
                    )
                })?;
            Ok(MemoryExportReceipt::exhausted(
                self.name,
                filter.clone(),
                snapshot,
                record_count,
                1,
            ))
        }

        fn forget_subject(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            _actor: &hermes_core::MemoryAuditActor,
        ) -> Result<MemoryForgetReceipt, MemoryProviderGovernanceError> {
            if matches!(self.behavior, ProbeBehavior::UnsupportedForget) {
                return Err(MemoryProviderGovernanceError::unsupported(
                    self.name,
                    MemoryGovernanceOperation::Forget,
                ));
            }
            if matches!(self.behavior, ProbeBehavior::RemoteForgetFailure) {
                return Err(MemoryProviderGovernanceError::failed(
                    self.name,
                    MemoryGovernanceOperation::Forget,
                ));
            }
            let deleted_records = {
                let mut records = self.records.lock().expect("probe records lock");
                let deleted_records = *records;
                *records = 0;
                deleted_records
            };
            Ok(MemoryForgetReceipt::new(
                self.name,
                filter.clone(),
                deleted_records,
            ))
        }

        fn retention_sweep(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
            _policy: &hermes_core::MemoryRetentionPolicy,
            _actor: &hermes_core::MemoryAuditActor,
        ) -> Result<MemoryRetentionReceipt, MemoryProviderGovernanceError> {
            self.retention_calls.fetch_add(1, Ordering::SeqCst);
            Ok(MemoryRetentionReceipt::new(self.name, filter.clone(), 0))
        }

        fn verify_subject_absent(
            &self,
            _subject: &MemorySubject,
            filter: &MemorySubjectStorageKey,
        ) -> Result<MemoryReadbackReceipt, MemoryProviderGovernanceError> {
            let records = self.records();
            Ok(if records == 0 {
                MemoryReadbackReceipt::absent(self.name, filter.clone())
            } else {
                MemoryReadbackReceipt::residual(self.name, filter.clone(), records)
            })
        }
    }

    fn local_subject(session_id: &str) -> MemorySubject {
        hermes_core::MemorySubject::local_owner(hermes_core::MemorySurface::Cli, session_id)
            .expect("valid local subject")
    }

    fn manager_with_probes(
        home: &Path,
        subject: MemorySubject,
        probes: &[Arc<GovernanceProbe>],
    ) -> hermes_agent::MemoryManager {
        let mut manager =
            hermes_agent::MemoryManager::new(subject, home).expect("provision manager");
        manager.add_provider(Arc::new(GovernanceProbe::new(
            "builtin",
            ProbeBehavior::Succeeds,
            0,
        )));
        for probe in probes {
            manager.add_provider(probe.clone());
        }
        manager.initialize_all().expect("initialize probes");
        manager
    }

    #[test]
    fn full_provider_forget_success_is_the_only_erased_claim() {
        let home = tempdir().expect("temp home");
        let subject = local_subject("forget-success-session");
        let first = Arc::new(GovernanceProbe::new("first", ProbeBehavior::Succeeds, 3));
        let second = Arc::new(GovernanceProbe::new("second", ProbeBehavior::Succeeds, 7));
        let manager = manager_with_probes(
            home.path(),
            subject.clone(),
            &[Arc::clone(&first), Arc::clone(&second)],
        );

        let report = manager
            .forget_subject(&subject, manager.audit_actor())
            .expect("all providers verify absent");

        assert!(report.is_erased());
        assert_eq!(first.records(), 0);
        assert_eq!(second.records(), 0);
        assert!(render_memory_forget_report(&report, false).contains("\"erased\": true"));
        assert!(render_memory_forget_report(&report, true).contains("\"reset_alias\": true"));
        manager.shutdown_all();
    }

    #[test]
    fn unsupported_or_failed_provider_prevents_any_erased_claim() {
        let home = tempdir().expect("temp home");
        let subject = local_subject("forget-incomplete-session");
        let first = Arc::new(GovernanceProbe::new("first", ProbeBehavior::Succeeds, 3));
        let unsupported = Arc::new(GovernanceProbe::new(
            "unsupported-remote",
            ProbeBehavior::UnsupportedForget,
            7,
        ));
        let remote_failure = Arc::new(GovernanceProbe::new(
            "failed-remote",
            ProbeBehavior::RemoteForgetFailure,
            11,
        ));
        let manager = manager_with_probes(
            home.path(),
            subject.clone(),
            &[
                Arc::clone(&first),
                Arc::clone(&unsupported),
                Arc::clone(&remote_failure),
            ],
        );

        let error = manager
            .forget_subject(&subject, manager.audit_actor())
            .expect_err("unsupported provider must fail closed");
        let hermes_agent::memory_manager::MemoryManagerError::ForgetIncomplete { report } = error
        else {
            panic!("expected typed forget residual report");
        };
        let rendered = render_memory_forget_report(&report, false);

        assert!(!report.is_erased());
        assert!(rendered.contains("\"erased\": false"));
        assert!(rendered.contains("\"error\": \"unsupported\""));
        assert!(rendered.contains("\"error\": \"failed\""));
        assert_eq!(first.records(), 0, "fan-out still reaches healthy providers");
        assert_eq!(unsupported.records(), 7);
        assert_eq!(remote_failure.records(), 11);
        manager.shutdown_all();
    }

    #[test]
    fn unsupported_export_is_artifactless_and_never_claims_completion() {
        let home = tempdir().expect("temp home");
        let subject = local_subject("export-incomplete-session");
        let first = Arc::new(GovernanceProbe::new(
            "first-export",
            ProbeBehavior::Succeeds,
            1,
        ));
        let unsupported = Arc::new(GovernanceProbe::new(
            "unsupported-export",
            ProbeBehavior::UnsupportedExport,
            1,
        ));
        let manager = manager_with_probes(
            home.path(),
            subject.clone(),
            &[Arc::clone(&first), Arc::clone(&unsupported)],
        );

        let error = manager
            .export_subject(&subject, manager.audit_actor())
            .expect_err("unsupported provider must abort the export transaction");
        let hermes_agent::memory_manager::MemoryManagerError::ExportIncomplete { report } = error
        else {
            panic!("expected typed incomplete export report");
        };
        let rendered = render_memory_export_report(&report);

        assert!(!report.is_complete());
        assert!(report.artifact.is_none());
        assert!(rendered.contains("\"complete\": false"));
        assert!(rendered.contains("\"artifact\": null"));
        assert!(rendered.contains("\"error\": \"unsupported\""));
        assert!(!rendered.contains("artifact_ref"));
        manager.shutdown_all();
    }

    #[test]
    fn governance_target_cannot_select_another_principal() {
        assert!(matches!(
            reject_memory_governance_target(Some("another-principal")),
            Err(hermes_core::AgentError::PolicyBlocked(_))
        ));
    }

    #[test]
    fn governance_never_invents_a_default_subject() {
        assert!(matches!(
            memory_governance_subject(&MemoryCliOptions::default()),
            Err(hermes_core::AgentError::Config(_))
        ));
    }

    #[test]
    fn export_receipt_is_redacted_and_does_not_expose_subject_or_content() {
        let home = tempdir().expect("temp home");
        let subject = local_subject("export-private-session");
        let provider = Arc::new(GovernanceProbe::new(
            "export-provider",
            ProbeBehavior::Succeeds,
            1,
        ));
        let manager =
            manager_with_probes(home.path(), subject.clone(), &[Arc::clone(&provider)]);

        let report = manager
            .export_subject(&subject, manager.audit_actor())
            .expect("export receipt");
        let rendered = render_memory_export_report(&report);

        let artifact = report.artifact.as_ref().expect("committed export artifact");
        assert!(report.is_complete());
        assert_eq!(artifact.record_count, 1);
        assert_eq!(artifact.page_count, 1);
        assert_eq!(artifact.provider_count, 2);
        assert!(rendered.contains("\"redacted\": true"));
        assert!(rendered.contains("\"artifact_ref\": \"mea1_"));
        assert!(rendered.contains("\"manifest_digest\": \"sha256:"));
        assert!(!rendered.contains("export-private-session"));
        assert!(!rendered.contains("fixture_record"));
        assert!(!rendered.contains("private-content"));
        manager.shutdown_all();
    }

    #[test]
    fn retention_defaults_to_dry_run_until_confirmed_apply() {
        let home = tempdir().expect("temp home");
        let subject = local_subject("retention-session");
        let provider = Arc::new(GovernanceProbe::new(
            "retention-provider",
            ProbeBehavior::Succeeds,
            2,
        ));
        let manager =
            manager_with_probes(home.path(), subject.clone(), &[Arc::clone(&provider)]);
        let policy = hermes_core::MemoryRetentionPolicy::new(Some(30), None)
            .expect("retention policy");
        let dry_run = MemoryCliOptions {
            setup: MemorySetupCliOptions::yes_only(false),
            retention_days: Some(30),
            ..MemoryCliOptions::default()
        };
        let apply = MemoryCliOptions {
            setup: MemorySetupCliOptions::yes_only(true),
            retention_days: Some(30),
            ..MemoryCliOptions::default()
        };
        let forced_dry_run = MemoryCliOptions {
            setup: MemorySetupCliOptions {
                dry_run: true,
                ..MemorySetupCliOptions::yes_only(true)
            },
            retention_days: Some(30),
            ..MemoryCliOptions::default()
        };

        assert_eq!(
            memory_retention_execution(&dry_run),
            MemoryRetentionExecution::DryRun
        );
        assert_eq!(
            memory_retention_execution(&forced_dry_run),
            MemoryRetentionExecution::DryRun
        );
        assert_eq!(provider.retention_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            memory_retention_execution(&apply),
            MemoryRetentionExecution::Apply
        );
        manager
            .retention_sweep(&subject, &policy, manager.audit_actor())
            .expect("confirmed retention applies");
        assert_eq!(provider.retention_calls.load(Ordering::SeqCst), 1);
        manager.shutdown_all();
    }

    #[test]
    fn forget_audit_receipt_is_content_free_and_subject_opaque() {
        let home = tempdir().expect("temp home");
        let subject = local_subject("audit-private-session");
        let provider = Arc::new(GovernanceProbe::new(
            "audit-provider",
            ProbeBehavior::Succeeds,
            1,
        ));
        let manager =
            manager_with_probes(home.path(), subject.clone(), &[Arc::clone(&provider)]);

        manager
            .forget_subject(&subject, manager.audit_actor())
            .expect("forget succeeds");
        manager.shutdown_all();
        let audit = std::fs::read_to_string(
            home.path()
                .join("memory-governance")
                .join("memory-audit-v1.jsonl"),
        )
        .expect("audit receipt");

        assert!(audit.contains("\"action\":\"forget\""));
        assert!(audit.contains("\"subject_key\":\"msp1_"));
        assert!(audit.contains("\"actor\":\"maa1_"));
        assert!(!audit.contains("audit-private-session"));
        assert!(!audit.contains("private-content"));
    }
}

include!("command_cli_plugins_memory_mcp/mcp_commands.rs");
