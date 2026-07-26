// ---------------------------------------------------------------------------
// Plugin diagnostic discovery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginSurfaceStatus {
    VerifiedDiagnostic,
    Quarantined,
    AuthorityUnavailable,
}

impl PluginSurfaceStatus {
    fn label(self) -> &'static str {
        match self {
            Self::VerifiedDiagnostic => "verified-diagnostic",
            Self::Quarantined => "quarantined",
            Self::AuthorityUnavailable => "authority-unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginSurfaceSource {
    User,
    Project,
    Runtime,
}

impl PluginSurfaceSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Runtime => "runtime",
        }
    }
}

#[derive(Debug, Clone)]
struct PluginRemovalTarget {
    path: PathBuf,
    source: PluginSurfaceSource,
}

#[derive(Debug, Clone)]
struct PluginSurfaceEntry {
    name: String,
    version: String,
    description: String,
    source: PluginSurfaceSource,
    path: Option<PathBuf>,
    status: PluginSurfaceStatus,
    reason: Option<String>,
}

fn resolved_hermes_home() -> Result<PathBuf, String> {
    let home = hermes_config::hermes_home();
    if home.is_absolute() {
        Ok(home)
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(home))
            .map_err(|error| format!("cannot resolve Hermes home: {error}"))
    }
}

fn authority_unavailable_entry(reason: impl Into<String>) -> PluginSurfaceEntry {
    PluginSurfaceEntry {
        name: "<plugin trust authority>".to_string(),
        version: "-".to_string(),
        description: "Plugin bundle verification is unavailable; no third-party plugin can execute."
            .to_string(),
        source: PluginSurfaceSource::Runtime,
        path: None,
        status: PluginSurfaceStatus::AuthorityUnavailable,
        reason: Some(reason.into()),
    }
}

/// Discover only bundles that core trust verification has classified. This
/// never parses raw `plugin.yaml` as trusted metadata and never loads code.
fn discover_plugin_surface() -> Vec<PluginSurfaceEntry> {
    let home = match resolved_hermes_home() {
        Ok(home) => home,
        Err(error) => return vec![authority_unavailable_entry(error)],
    };
    let layout = match hermes_config::SkillTrustAuthorityLayout::from_resolved_home(&home) {
        Ok(layout) => layout,
        Err(error) => return vec![authority_unavailable_entry(error.to_string())],
    };
    let authority = match hermes_skills::SkillTrustAuthorityPaths::new(
        home.join("plugins"),
        layout.remote_root_file().to_path_buf(),
        layout.remote_audit_file().to_path_buf(),
        layout.local_owner_keyring_file().to_path_buf(),
    ) {
        Ok(authority) => authority,
        Err(error) => return vec![authority_unavailable_entry(error.to_string())],
    };
    let report = hermes_agent::PluginManager::discover_plugins_runtime_default(
        &home,
        &authority,
        hermes_skills::SkillTrustPolicy::default(),
        std::collections::BTreeMap::new(),
    );
    let mut rows = Vec::with_capacity(report.admitted.len() + report.quarantined.len());
    rows.extend(report.admitted.into_iter().map(|bundle| PluginSurfaceEntry {
        name: bundle.identity().name.clone(),
        version: bundle.identity().version.clone(),
        description: bundle.manifest.description,
        source: plugin_surface_source(bundle.source),
        path: Some(bundle.path),
        status: PluginSurfaceStatus::VerifiedDiagnostic,
        reason: Some(
            "verified package metadata; third-party execution is unavailable until a loader is policy- and sandbox-bound"
                .to_string(),
        ),
    }));
    rows.extend(report.quarantined.into_iter().map(|quarantine| {
        let (name, version) = quarantine
            .identity
            .as_ref()
            .map(|identity| (identity.name.clone(), identity.version.clone()))
            .unwrap_or_else(|| ("<unidentified plugin bundle>".to_string(), "-".to_string()));
        PluginSurfaceEntry {
            name,
            version,
            description: quarantine.detail,
            source: plugin_surface_source(quarantine.source),
            path: Some(quarantine.path),
            status: PluginSurfaceStatus::Quarantined,
            reason: Some(format!("{:?}", quarantine.reason)),
        }
    }));
    rows.sort_by(|left, right| {
        left
            .source
            .as_str()
            .cmp(right.source.as_str())
            .then_with(|| left.name.to_ascii_lowercase().cmp(&right.name.to_ascii_lowercase()))
    });
    rows
}

fn plugin_surface_source(
    source: hermes_agent::plugins::PluginDiscoverySource,
) -> PluginSurfaceSource {
    match source {
        hermes_agent::plugins::PluginDiscoverySource::User => PluginSurfaceSource::User,
        hermes_agent::plugins::PluginDiscoverySource::Project => PluginSurfaceSource::Project,
    }
}

/// Select exactly one core-discovered plugin row for an operator-requested
/// cleanup. Discovery names are not sufficient authority: the selected path is
/// revalidated against its allowed root immediately before deletion.
fn resolve_local_plugin_path_by_name(name: &str) -> Result<PluginRemovalTarget, String> {
    select_plugin_removal_target(name, &discover_plugin_surface())
}

fn select_plugin_removal_target(
    name: &str,
    rows: &[PluginSurfaceEntry],
) -> Result<PluginRemovalTarget, String> {
    let matches = rows
        .iter()
        .filter(|row| {
            row.status != PluginSurfaceStatus::AuthorityUnavailable
                && row.name.eq_ignore_ascii_case(name)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(
            "plugin removal requires a bundle identity surfaced by core verification; unidentified bundles must be removed manually by the local owner"
                .to_string(),
        ),
        [row] => {
            let path = row.path.clone().ok_or_else(|| {
                "plugin removal requires a concrete core-discovered bundle path".to_string()
            })?;
            if row.source == PluginSurfaceSource::Runtime {
                return Err("runtime plugin diagnostics are not removable".to_string());
            }
            Ok(PluginRemovalTarget {
                path,
                source: row.source,
            })
        }
        matches => {
            let diagnostics = matches
                .iter()
                .map(|row| {
                    format!(
                        "source={}; path={}",
                        row.source.as_str(),
                        row.path
                            .as_deref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "-".to_string())
                    )
                })
                .collect::<Vec<_>>()
                .join("\n  - ");
            Err(format!(
                "refusing ambiguous plugin removal for '{}'; multiple core-discovered bundles match:\n  - {}",
                name, diagnostics
            ))
        }
    }
}

fn revalidate_plugin_removal_target(
    target: &PluginRemovalTarget,
    hermes_home: &Path,
) -> Result<PathBuf, String> {
    let root = match target.source {
        PluginSurfaceSource::User => hermes_home.join("plugins"),
        PluginSurfaceSource::Project => {
            if !hermes_config::env_var_enabled("HERMES_ENABLE_PROJECT_PLUGINS") {
                return Err(
                    "project plugin cleanup requires explicit HERMES_ENABLE_PROJECT_PLUGINS selection"
                        .to_string(),
                );
            }
            std::env::current_dir()
                .map_err(|error| format!("cannot resolve project plugin root: {error}"))?
                .join(".hermes")
                .join("plugins")
        }
        PluginSurfaceSource::Runtime => {
            return Err("runtime plugin diagnostics are not removable".to_string())
        }
    };
    let root_metadata = std::fs::symlink_metadata(&root)
        .map_err(|error| format!("cannot inspect plugin root {}: {error}", root.display()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(format!(
            "refusing plugin removal: allowed root {} must be a non-symlink directory",
            root.display()
        ));
    }
    let canonical_root = std::fs::canonicalize(&root)
        .map_err(|error| format!("cannot canonicalize plugin root {}: {error}", root.display()))?;
    let target_metadata = std::fs::symlink_metadata(&target.path).map_err(|error| {
        format!(
            "cannot inspect selected plugin path {} immediately before removal: {error}",
            target.path.display()
        )
    })?;
    if target_metadata.file_type().is_symlink() || !target_metadata.is_dir() {
        return Err(format!(
            "refusing plugin removal: selected path {} must be a non-symlink directory",
            target.path.display()
        ));
    }
    let canonical_target = std::fs::canonicalize(&target.path).map_err(|error| {
        format!(
            "cannot canonicalize selected plugin path {} immediately before removal: {error}",
            target.path.display()
        )
    })?;
    if canonical_target.parent() != Some(canonical_root.as_path()) {
        return Err(format!(
            "refusing plugin removal: selected path {} is not a direct child of allowed root {}",
            canonical_target.display(),
            canonical_root.display()
        ));
    }
    Ok(canonical_target)
}

/// Delete only after the selected path has been revalidated against the
/// source-specific root in the same synchronous operation.
fn remove_plugin_bundle(
    target: &PluginRemovalTarget,
    hermes_home: &Path,
) -> Result<(), AgentError> {
    let target = revalidate_plugin_removal_target(target, hermes_home)
        .map_err(AgentError::PolicyBlocked)?;
    std::fs::remove_dir_all(&target)
        .map_err(|error| AgentError::Io(format!("Failed to remove plugin: {error}")))
}

fn render_plugin_surface_table(rows: &[PluginSurfaceEntry]) -> String {
    if rows.is_empty() {
        return "  (no plugin bundles discovered)".to_string();
    }
    let mut out = String::new();
    for row in rows {
        let path = row
            .path
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        let _ = writeln!(
            out,
            "  • {} v{} [{}; source={}; path={}]",
            row.name,
            row.version,
            row.status.label(),
            row.source.as_str(),
            path
        );
        if !row.description.trim().is_empty() {
            let _ = writeln!(out, "    {}", row.description.trim());
        }
        if let Some(reason) = row.reason.as_deref() {
            let _ = writeln!(out, "    reason={reason}");
        }
    }
    out.trim_end().to_string()
}

pub async fn handle_cli_external_plugin_subcommand(raw: Vec<String>) -> Result<(), AgentError> {
    if raw.is_empty() {
        return Err(AgentError::Config(
            "Unknown command. Run `hermes --help` for available commands.".to_string(),
        ));
    }
    let command_name = raw[0].trim().to_string();
    Err(AgentError::Config(format!(
        "Unknown command '{}'. Run `hermes --help` for Rust-native commands. Python plugin command dispatch is disabled in Hermes Agent Ultra's Rust-only runtime.",
        command_name
    )))
}
