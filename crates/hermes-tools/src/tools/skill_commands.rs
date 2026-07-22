//! Skill slash-command support.
//!
//! Skills can declare `/command` handlers in their frontmatter. This module
//! provides a registry that maps slash commands to skills, enabling users to
//! invoke skill-provided functionality directly from the REPL.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::skill_utils::{
    discover_verified_skills, load_verified_skill_by_selector, match_platform,
    reload_verified_skill, render_skill_provenance_tag, BlockedSkill, SkillInfo,
    SkillTrustIdentity, SkillTrustRuntime,
};
// ---------------------------------------------------------------------------
// SkillCommand
// ---------------------------------------------------------------------------

/// A slash command defined by a skill.
#[derive(Debug, Clone)]
pub struct SkillCommand {
    /// The command name without the `/` prefix (e.g. "plan", "review").
    pub name: String,
    /// Description shown in help output.
    pub description: String,
    /// The skill that owns this command.
    pub skill_name: String,
    /// Immutable package evidence retained with this registered command.
    pub provenance: SkillTrustIdentity,
    /// Package locator retained solely so every invocation re-loads the
    /// verified command declaration rather than using a registry snapshot.
    verified_skill: SkillInfo,
}

// ---------------------------------------------------------------------------
// SkillCommandRegistry
// ---------------------------------------------------------------------------

/// Registry of slash commands contributed by skills.
pub struct SkillCommandRegistry {
    commands: HashMap<String, SkillCommand>,
}

/// Runtime configuration for resolving installed skills as slash commands.
#[derive(Debug, Clone)]
pub struct SkillCommandResolverConfig {
    /// Actual skill roots containing a single v2 `.hub/lock.json` each.
    pub roots: Vec<PathBuf>,
    /// Optional allow-list of skill names or command slugs.
    pub enabled: Vec<String>,
    /// Optional deny-list of skill names or command slugs.
    pub disabled: Vec<String>,
    /// Platform label used for skill frontmatter filtering.
    pub platform: Option<String>,
    /// Core verifier context. A missing context fails closed.
    pub trust_runtime: Option<SkillTrustRuntime>,
}

impl Default for SkillCommandResolverConfig {
    fn default() -> Self {
        Self {
            roots: default_skill_roots(),
            enabled: Vec::new(),
            disabled: Vec::new(),
            platform: Some(std::env::consts::OS.to_string()),
            trust_runtime: SkillTrustRuntime::fail_closed().ok(),
        }
    }
}

/// Resolved installed-skill slash invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSlashInvocation {
    /// Canonical command key, including leading slash.
    pub command: String,
    /// Canonical package name, not a frontmatter-only label.
    pub skill_name: String,
    /// Short description from verified package frontmatter, if present.
    pub description: String,
    /// Typed identity that must accompany the selected prompt content.
    pub provenance: SkillTrustIdentity,
    /// User message content to send into the agent loop.
    pub message: String,
}

/// One installed skill command discovered during a fresh verified-store pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSlashCommandEntry {
    /// Canonical command key, including leading slash.
    pub command: String,
    /// Canonical package name.
    pub skill_name: String,
    /// Short description from verified package frontmatter, if present.
    pub description: String,
    /// Typed provenance for an admitted package; absent for a rejected lock
    /// record that was never allowed into prompt material.
    pub provenance: Option<SkillTrustIdentity>,
    /// Typed trust rejection text when the package was blocked.
    pub blocked_reason: Option<String>,
}

/// Snapshot of installed skill commands visible to the current runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSlashCommandSnapshot {
    /// Skill roots consulted through their v2 stores.
    pub roots: Vec<PathBuf>,
    /// Verified commands available for slash invocation.
    pub available: Vec<SkillSlashCommandEntry>,
    /// Lock records rejected by the trust verifier.
    pub blocked: Vec<SkillSlashCommandEntry>,
    /// Skills skipped by platform, enable/disable filters, empty slugs, or duplicate commands.
    pub skipped: usize,
}

impl SkillCommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
        }
    }

    /// Register a command sourced from an already verified package.
    fn register(&mut self, cmd: SkillCommand) {
        self.commands.insert(cmd.name.clone(), cmd);
    }

    /// Look up a command by name.
    pub fn get(&self, name: &str) -> Option<&SkillCommand> {
        self.commands.get(name)
    }

    /// List all registered skill commands.
    pub fn list(&self) -> Vec<&SkillCommand> {
        self.commands.values().collect()
    }

    /// Return command names for auto-complete.
    pub fn command_names(&self) -> Vec<String> {
        self.commands.keys().map(|k| format!("/{}", k)).collect()
    }

    /// Number of registered commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

impl Default for SkillCommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Default installed skill root shared by CLI and gateway surfaces.
///
/// `hermes_config::skills_dir` owns the legacy-home decision; scanning an
/// additional home directory would create a second independent lock authority.
pub fn default_skill_roots() -> Vec<PathBuf> {
    vec![hermes_config::skills_dir()]
}

fn normalize_selector(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('/')
        .replace(['_', ' '], "-")
        .to_ascii_lowercase()
}

fn slugify_skill_command_name(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.trim().chars() {
        let next = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if matches!(ch, '-' | '_' | ' ') {
            Some('-')
        } else {
            None
        };
        let Some(ch) = next else {
            continue;
        };
        if ch == '-' {
            if out.is_empty() || last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        out.push(ch);
    }
    out.trim_matches('-').to_string()
}

fn selector_set(values: &[String]) -> HashSet<String> {
    values
        .iter()
        .map(|value| normalize_selector(value))
        .filter(|value| !value.is_empty())
        .collect()
}

fn skill_allowed(
    skill_name: &str,
    command_slug: &str,
    enabled: &[String],
    disabled: &[String],
) -> bool {
    let name_key = normalize_selector(skill_name);
    let cmd_key = normalize_selector(command_slug);
    let disabled = selector_set(disabled);
    if disabled.contains(&name_key) || disabled.contains(&cmd_key) {
        return false;
    }
    let enabled = selector_set(enabled);
    enabled.is_empty() || enabled.contains(&name_key) || enabled.contains(&cmd_key)
}

fn platform_aliases(platform: Option<&str>) -> Vec<String> {
    let raw = platform
        .unwrap_or(std::env::consts::OS)
        .trim()
        .to_ascii_lowercase();
    match raw.as_str() {
        "macos" | "darwin" => vec!["macos".to_string(), "darwin".to_string()],
        "windows" | "win32" => vec!["windows".to_string(), "win32".to_string()],
        "linux" => vec!["linux".to_string()],
        "" => vec![std::env::consts::OS.to_string()],
        other => vec![other.to_string()],
    }
}

fn skill_matches_platform(skill: &SkillInfo, platform: Option<&str>) -> bool {
    platform_aliases(platform)
        .iter()
        .any(|alias| match_platform(skill, alias))
}


fn skill_description(skill: &SkillInfo) -> String {
    skill
        .frontmatter
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

fn skill_command_entry(skill: &SkillInfo, slug: &str) -> SkillSlashCommandEntry {
    SkillSlashCommandEntry {
        command: format!("/{slug}"),
        skill_name: skill.name.clone(),
        description: skill_description(skill),
        provenance: Some(skill.provenance().clone()),
        blocked_reason: None,
    }
}

fn blocked_skill_command_entry(blocked: &BlockedSkill) -> Option<SkillSlashCommandEntry> {
    let slug = slugify_skill_command_name(&blocked.name);
    (!slug.is_empty()).then(|| SkillSlashCommandEntry {
        command: format!("/{slug}"),
        skill_name: blocked.name.clone(),
        description: String::new(),
        provenance: None,
        blocked_reason: Some(blocked.reason.clone()),
    })
}

/// Scan v2 lock entries and report the slash commands currently visible.
///
/// This is intentionally a snapshot, not a cache invalidation hook. Every
/// invocation performs another full core verifier/revocation/digest pass.
pub fn installed_skill_slash_command_snapshot(
    config: &SkillCommandResolverConfig,
) -> SkillSlashCommandSnapshot {
    let mut available = Vec::new();
    let mut blocked = Vec::new();
    let mut skipped = 0usize;
    let mut seen_slugs = HashSet::new();

    let Some(trust) = config.trust_runtime.as_ref() else {
        for root in &config.roots {
            if let Some(entry) = blocked_skill_command_entry(&BlockedSkill {
                name: root.display().to_string(),
                reason: "no core skill trust verifier is configured".to_string(),
            }) {
                blocked.push(entry);
            }
        }
        return SkillSlashCommandSnapshot {
            roots: config.roots.clone(),
            available,
            blocked,
            skipped,
        };
    };
    let catalog = discover_verified_skills(&config.roots, trust);
    for skill in catalog.available {
        if !skill_matches_platform(&skill, config.platform.as_deref()) {
            skipped = skipped.saturating_add(1);
            continue;
        }
        let slug = slugify_skill_command_name(&skill.name);
        if slug.is_empty() || !skill_allowed(&skill.name, &slug, &config.enabled, &config.disabled)
        {
            skipped = skipped.saturating_add(1);
            continue;
        }
        if !seen_slugs.insert(slug.clone()) {
            skipped = skipped.saturating_add(1);
            continue;
        }
        available.push(skill_command_entry(&skill, &slug));
    }
    for rejection in catalog.blocked {
        let Some(entry) = blocked_skill_command_entry(&rejection) else {
            continue;
        };
        if !seen_slugs.insert(entry.command.trim_start_matches('/').to_string()) {
            continue;
        }
        blocked.push(entry);
    }

    available.sort_by(|a, b| a.command.cmp(&b.command));
    blocked.sort_by(|a, b| a.command.cmp(&b.command));

    SkillSlashCommandSnapshot {
        roots: config.roots.clone(),
        available,
        blocked,
        skipped,
    }
}

/// Render a concise `/reload-skills` reply for CLI/gateway users.
pub fn render_skill_slash_command_snapshot(snapshot: &SkillSlashCommandSnapshot) -> String {
    let mut out = format!(
        "Reloaded installed skill commands: available={} blocked={} skipped={}.\nVerified v2 skill packages are rechecked on every invocation; no prompt cache was invalidated.",
        snapshot.available.len(),
        snapshot.blocked.len(),
        snapshot.skipped
    );
    if !snapshot.available.is_empty() {
        let commands = snapshot
            .available
            .iter()
            .map(|entry| format!("{} ({})", entry.command, entry.skill_name))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str("\nAvailable: ");
        out.push_str(&commands);
    }
    if !snapshot.blocked.is_empty() {
        let blocked = snapshot
            .blocked
            .iter()
            .map(|entry| format!("{} ({})", entry.command, entry.skill_name))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str("\nBlocked by security policy: ");
        out.push_str(&blocked);
    }
    out
}

/// Build the one-shot system note queued after `/reload-skills`.
pub fn build_skill_reload_system_note(snapshot: &SkillSlashCommandSnapshot) -> String {
    let available = if snapshot.available.is_empty() {
        "none".to_string()
    } else {
        snapshot
            .available
            .iter()
            .map(|entry| format!("{} ({})", entry.command, entry.skill_name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let blocked = if snapshot.blocked.is_empty() {
        "none".to_string()
    } else {
        snapshot
            .blocked
            .iter()
            .map(|entry| format!("{} ({})", entry.command, entry.skill_name))
            .collect::<Vec<_>>()
            .join(", ")
    };

    format!(
        "[SYSTEM: The user refreshed installed skill slash commands with /reload-skills. Verified v2 skill packages are rechecked on invocation; no prompt cache was invalidated. Current available commands: {available}. Blocked commands: {blocked}.]"
    )
}

/// Build the agent-facing message for a freshly verified skill invocation.
fn build_skill_slash_invocation_message(
    skill_name: &str,
    skill_body: &str,
    provenance: &SkillTrustIdentity,
    user_instruction: &str,
) -> String {
    let mut parts = vec![
        format!(
            "[SYSTEM: The user has invoked the \"{}\" skill, indicating they want you to follow its instructions. The full skill content is loaded below.]",
            skill_name
        ),
        String::new(),
        render_skill_provenance_tag(provenance),
        String::new(),
        skill_body.trim().to_string(),
    ];

    if !user_instruction.trim().is_empty() {
        parts.push(String::new());
        parts.push(format!(
            "The user has provided the following instruction alongside the skill invocation: {}",
            user_instruction.trim()
        ));
    }

    parts.join("\n")
}

/// Resolve an otherwise-unknown slash command through the core-verified v2
/// skill store. Built-in and quick-command handlers should run before this
/// resolver.
pub fn resolve_installed_skill_slash_command(
    command: &str,
    args: &str,
    config: &SkillCommandResolverConfig,
) -> Result<Option<SkillSlashInvocation>, String> {
    let requested = normalize_selector(command);
    if requested.is_empty() {
        return Ok(None);
    }
    let trust = config
        .trust_runtime
        .as_ref()
        .ok_or_else(|| "no core skill trust verifier is configured".to_string())?;
    let Some(skill) = load_verified_skill_by_selector(&config.roots, &requested, trust)? else {
        return Ok(None);
    };
    if !skill_matches_platform(&skill, config.platform.as_deref()) {
        return Ok(None);
    }
    let slug = slugify_skill_command_name(&skill.name);
    if slug.is_empty() || slug != requested {
        return Ok(None);
    }
    if !skill_allowed(&skill.name, &slug, &config.enabled, &config.disabled) {
        return Ok(None);
    }

    let description = skill_description(&skill);
    let provenance = skill.provenance().clone();
    let message = build_skill_slash_invocation_message(
        &skill.name,
        &skill.body,
        &provenance,
        args,
    );
    Ok(Some(SkillSlashInvocation {
        command: format!("/{slug}"),
        skill_name: skill.name,
        description,
        provenance,
        message,
    }))
}

// ---------------------------------------------------------------------------
// Registration helpers
// ---------------------------------------------------------------------------

/// Scan skills for `/command` definitions in their frontmatter and register them.
///
/// Frontmatter format:
/// ```yaml
/// commands:
///   - name: plan
///     description: "Generate an execution plan"
///     template: "Create a detailed plan for: {args}"
///   - name: review
///     description: "Review code changes"
///     template: "Review the following: {args}"
/// ```
/// Register frontmatter commands only from already verified lock-store entries.
pub fn register_skill_commands(skills: &[SkillInfo]) -> SkillCommandRegistry {
    let mut registry = SkillCommandRegistry::new();

    for skill in skills.iter().filter(|skill| skill.is_verified()) {
        let Some(commands_val) = skill.frontmatter.get("commands") else {
            continue;
        };

        let Some(commands_arr) = commands_val.as_array() else {
            continue;
        };

        for cmd_val in commands_arr {
            let Some(obj) = cmd_val.as_object() else {
                continue;
            };

            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let description = obj
                .get("description")
                .or_else(|| obj.get("desc"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            if !name.is_empty() {
                registry.register(SkillCommand {
                    name,
                    description,
                    skill_name: skill.name.clone(),
                    provenance: skill.provenance().clone(),
                    verified_skill: skill.clone(),
                });
            }
        }
    }

    registry
}

fn verified_command_template(skill: &SkillInfo, command_name: &str) -> Option<String> {
    let commands = skill.frontmatter.get("commands")?.as_array()?;
    for command in commands.iter().filter_map(|command| command.as_object()) {
        if command
            .get("name")
            .and_then(|value| value.as_str())
            .is_some_and(|name| name == command_name)
        {
            return Some(
                command
                    .get("template")
                    .and_then(|value| value.as_str())
                    .unwrap_or("{args}")
                    .to_string(),
            );
        }
    }
    None
}

/// Handle a skill command invocation after refreshing its owning package
/// through the core verifier. Revoked, quarantined, tampered, or otherwise
/// changed packages therefore cannot inject a previously registered template.
pub fn handle_skill_command(
    registry: &SkillCommandRegistry,
    command: &str,
    args: &str,
    trust: &SkillTrustRuntime,
) -> Result<Option<String>, String> {
    let cmd_name = command.trim_start_matches('/');
    let Some(cmd) = registry.get(cmd_name) else {
        return Ok(None);
    };
    let fresh = reload_verified_skill(&cmd.verified_skill, trust)?;
    let template = verified_command_template(&fresh, &cmd.name).ok_or_else(|| {
        format!(
            "verified skill '{}' no longer declares command '/{}'",
            fresh.name, cmd.name
        )
    })?;
    let content = template.replace("{args}", args);
    Ok(Some(format!(
        "{}\n{}",
        render_skill_provenance_tag(fresh.provenance()),
        content
    )))
}

/// Get skills that respond to the `/plan` command.
pub fn get_plan_command_skills(skills: &[SkillInfo]) -> Vec<&SkillInfo> {
    skills
        .iter()
        .filter(|skill| {
            skill.is_verified()
                && skill
                    .frontmatter
                    .get("commands")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter().any(|cmd| {
                            cmd.as_object()
                                .and_then(|o| o.get("name"))
                                .and_then(|v| v.as_str())
                                .map(|n| n == "plan")
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::Skill;
    use hermes_skills::{
        canonical_bundle_digest, full_skill_scan, store_lock, store_revocations, FileSkillStore,
        LocalOwnerAttestation, SignedSkillRevocationList, SkillCompatibility, SkillLockEntryV2,
        SkillLockV2, SkillPackageIdentityV2, SkillPackageManifestV2, SkillPackageProof,
        SkillPackageSource, SkillPermissions, SkillRevocation, SkillSignature, SkillSigningKey,
        SkillStore, SkillTrustKeyring, SkillTrustPolicy, SkillTrustVerifier,
        SKILL_LOCK_SCHEMA_VERSION, SKILL_REVOCATION_SCHEMA_VERSION,
    };
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn make_skill_with_commands() -> SkillInfo {
        let mut frontmatter = HashMap::new();
        frontmatter.insert(
            "commands".to_string(),
            serde_json::json!([
                {
                    "name": "plan",
                    "description": "Generate a plan",
                    "template": "Create a detailed plan for: {args}"
                },
                {
                    "name": "review",
                    "description": "Review code",
                    "template": "Review this code: {args}"
                }
            ]),
        );
        SkillInfo::test_fixture(
            "planning",
            PathBuf::from("planning"),
            String::new(),
            frontmatter,
            String::new(),
        )
    }

    fn make_skill_no_commands() -> SkillInfo {
        SkillInfo::test_fixture(
            "basic",
            PathBuf::from("basic"),
            String::new(),
            HashMap::new(),
            String::new(),
        )
    }

    fn install_attested_local_skill(
        root: &std::path::Path,
        name: &str,
        content: &str,
        description: &str,
    ) {
        let store = FileSkillStore::new(root.to_path_buf());
        futures::executor::block_on(store.save(&Skill {
            name: name.to_string(),
            content: content.to_string(),
            category: None,
            description: Some(description.to_string()),
        }))
        .unwrap();
        let digest = canonical_bundle_digest(&root.join(name)).unwrap();
        store
            .attest_local_skill(
                name,
                LocalOwnerAttestation::new(
                    "test-owner",
                    format!("test-attestation-{name}"),
                    digest,
                    "2026-07-20T00:00:00Z",
                ),
            )
            .unwrap();
    }

    fn install_signed_remote_skill(root: &std::path::Path) -> (SkillTrustRuntime, SkillSigningKey) {
        let name = "signed-skill";
        let package_root = root.join(name);
        fs::create_dir_all(&package_root).unwrap();
        fs::write(
            package_root.join("SKILL.md"),
            "---\nname: signed-skill\ndescription: Signed fixture\n---\n# Signed\n\n1. Follow verified instructions",
        )
        .unwrap();
        let signer = SkillSigningKey::from_seed([23; 32]);
        let identity = SkillPackageIdentityV2::new(
            name,
            "1.0.0",
            "https://registry.example/signed-skill",
            "publisher.example",
            "0123456789abcdef0123456789abcdef01234567",
            canonical_bundle_digest(&package_root).unwrap(),
            "2026-07-20T00:00:00Z",
        );
        let mut manifest = SkillPackageManifestV2::new(
            SkillPackageSource::Remote,
            identity,
            SkillPackageProof::RemoteSignature(SkillSignature::ed25519("test-key", "")),
            full_skill_scan(&package_root),
            SkillCompatibility::default(),
            SkillPermissions::default(),
            Vec::new(),
        );
        signer.sign_manifest(&mut manifest, "test-key").unwrap();
        store_lock(
            root,
            &SkillLockV2 {
                schema_version: SKILL_LOCK_SCHEMA_VERSION,
                packages: vec![SkillLockEntryV2 {
                    install_path: name.to_string(),
                    manifest,
                    quarantine: None,
                }],
            },
        )
        .unwrap();
        let verifier = SkillTrustVerifier::new(
            SkillTrustKeyring::new(vec![signer.trusted_key("test-key", true)]).unwrap(),
            SkillTrustPolicy::default(),
            BTreeMap::new(),
        )
        .unwrap();
        (SkillTrustRuntime::new(verifier), signer)
    }

    fn revoke_signed_remote_skill(root: &std::path::Path, signer: &SkillSigningKey) {
        let mut revocations = SignedSkillRevocationList {
            schema_version: SKILL_REVOCATION_SCHEMA_VERSION,
            issued_at: "2026-07-20T00:00:00Z".to_string(),
            signature: None,
            revoked: vec![SkillRevocation {
                name: Some("signed-skill".to_string()),
                publisher_id: None,
                immutable_source_revision: None,
                content_digest: None,
                key_id: None,
                reason: "fixture revocation".to_string(),
                revoked_at: "2026-07-20T00:00:00Z".to_string(),
            }],
        };
        signer.sign_revocations(&mut revocations, "test-key").unwrap();
        store_revocations(root, &revocations).unwrap();
    }

    fn fresh_registry_for_handler(
        root: &std::path::Path,
    ) -> (SkillCommandRegistry, SkillTrustRuntime) {
        install_attested_local_skill(
            root,
            "planning",
            "---\ncommands:\n  - name: plan\n    description: Generate a plan\n    template: \"Create a detailed plan for: {args}\"\n---\n# Planning\n\n1. Build a verified plan",
            "Planning",
        );
        let trust = SkillTrustRuntime::fail_closed().unwrap();
        let skill = load_verified_skill_by_selector(&[root.to_path_buf()], "planning", &trust)
            .unwrap()
            .unwrap();
        (register_skill_commands(&[skill]), trust)
    }

    #[test]
    fn test_handler_uses_fresh_verified_template_not_mutated_registry_snapshot() {
        let tmp = tempdir().unwrap();
        install_attested_local_skill(
            tmp.path(),
            "planning",
            "---\ncommands:\n  - name: plan\n    description: Generate a plan\n    template: \"Create a detailed plan for: {args}\"\n---\n# Planning\n\n1. Build a verified plan",
            "Planning",
        );
        let trust = SkillTrustRuntime::fail_closed().unwrap();
        let mut cached = load_verified_skill_by_selector(
            &[tmp.path().to_path_buf()],
            "planning",
            &trust,
        )
        .unwrap()
        .unwrap();
        cached.frontmatter.insert(
            "commands".to_string(),
            serde_json::json!([{
                "name": "plan",
                "template": "MUTATED SNAPSHOT: {args}"
            }]),
        );
        let registry = register_skill_commands(&[cached]);

        let result = handle_skill_command(&registry, "/plan", "ship safely", &trust)
            .unwrap()
            .unwrap();
        assert!(result.contains("Create a detailed plan for: ship safely"));
        assert!(!result.contains("MUTATED SNAPSHOT"));
    }

    #[test]
    fn test_register_skill_commands() {
        let skills = vec![make_skill_with_commands(), make_skill_no_commands()];
        let registry = register_skill_commands(&skills);
        assert_eq!(registry.len(), 2);
        assert!(registry.get("plan").is_some());
        assert!(registry.get("review").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_handle_skill_command_refreshes_verified_package() {
        let tmp = tempdir().unwrap();
        let (registry, trust) = fresh_registry_for_handler(tmp.path());
        let result = handle_skill_command(&registry, "/plan", "build a REST API", &trust)
            .unwrap()
            .unwrap();
        assert!(result.contains("Create a detailed plan for: build a REST API"));
        assert!(result.contains("<skill_provenance package=\"planning\""));
    }

    #[test]
    fn test_handle_skill_command_denies_tampered_package_after_registration() {
        let tmp = tempdir().unwrap();
        let (registry, trust) = fresh_registry_for_handler(tmp.path());
        fs::write(
            tmp.path().join("planning").join("SKILL.md"),
            "# Planning\n\n1. Tampered content",
        )
        .unwrap();
        assert!(handle_skill_command(&registry, "/plan", "args", &trust).is_err());
    }

    #[test]
    fn test_handle_unknown_command() {
        let registry = SkillCommandRegistry::new();
        let trust = SkillTrustRuntime::fail_closed().unwrap();
        assert!(
            handle_skill_command(&registry, "/unknown", "args", &trust)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_get_plan_command_skills() {
        let skills = vec![make_skill_with_commands(), make_skill_no_commands()];
        let plan_skills = get_plan_command_skills(&skills);
        assert_eq!(plan_skills.len(), 1);
        assert_eq!(plan_skills[0].name, "planning");
    }

    #[test]
    fn test_command_names() {
        let skills = vec![make_skill_with_commands()];
        let registry = register_skill_commands(&skills);
        let names = registry.command_names();
        assert!(names.contains(&"/plan".to_string()));
        assert!(names.contains(&"/review".to_string()));
    }

    #[test]
    fn test_empty_registry() {
        let registry = SkillCommandRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.list().is_empty());
    }

    #[test]
    fn resolves_installed_skill_by_slug_and_builds_agent_message() {
        let tmp = tempdir().unwrap();
        install_attested_local_skill(
            tmp.path(),
            "release-captain",
            "# Release Captain\n\n1. Inspect changes\n2. Verify gates",
            "Ship releases safely",
        );
        let config = SkillCommandResolverConfig {
            roots: vec![tmp.path().to_path_buf()],
            platform: Some("linux".to_string()),
            ..SkillCommandResolverConfig::default()
        };
        let resolved = resolve_installed_skill_slash_command("/release_captain", "cut v1", &config)
            .unwrap()
            .expect("skill command");
        assert_eq!(resolved.command, "/release-captain");
        assert_eq!(resolved.skill_name, "release-captain");
        assert_eq!(resolved.description, "Ship releases safely");
        assert!(resolved.message.contains("Verify gates"));
        assert!(resolved.message.contains("cut v1"));
        assert!(resolved.message.contains("<skill_provenance"));
    }

    #[test]
    fn signed_slash_package_loads_then_revocation_denies_snapshot_and_invocation() {
        let tmp = tempdir().unwrap();
        let (trust, signer) = install_signed_remote_skill(tmp.path());
        let config = SkillCommandResolverConfig {
            roots: vec![tmp.path().to_path_buf()],
            platform: Some("linux".to_string()),
            trust_runtime: Some(trust.clone()),
            ..SkillCommandResolverConfig::default()
        };
        let loaded = resolve_installed_skill_slash_command("/signed-skill", "verify", &config)
            .unwrap()
            .expect("signed package must load");
        assert_eq!(loaded.skill_name, "signed-skill");
        assert_eq!(loaded.provenance.key_id.as_deref(), Some("test-key"));
        assert_eq!(installed_skill_slash_command_snapshot(&config).available.len(), 1);

        revoke_signed_remote_skill(tmp.path(), &signer);
        assert!(resolve_installed_skill_slash_command("/signed-skill", "", &config).is_err());
        let snapshot = installed_skill_slash_command_snapshot(&config);
        assert!(snapshot.available.is_empty());
        assert_eq!(snapshot.blocked.len(), 1);
        assert_eq!(snapshot.blocked[0].skill_name, "signed-skill");
    }

    #[test]
    fn resolver_respects_enabled_and_disabled_filters() {
        let tmp = tempdir().unwrap();
        install_attested_local_skill(
            tmp.path(),
            "beta",
            "# Beta\n\n1. Do beta work",
            "Beta skill",
        );
        let disabled = SkillCommandResolverConfig {
            roots: vec![tmp.path().to_path_buf()],
            disabled: vec!["beta".to_string()],
            platform: Some("linux".to_string()),
            ..SkillCommandResolverConfig::default()
        };
        assert!(
            resolve_installed_skill_slash_command("/beta", "", &disabled)
                .unwrap()
                .is_none()
        );
        let enabled = SkillCommandResolverConfig {
            roots: vec![tmp.path().to_path_buf()],
            enabled: vec!["beta".to_string()],
            platform: Some("linux".to_string()),
            ..SkillCommandResolverConfig::default()
        };
        assert!(
            resolve_installed_skill_slash_command("/beta", "", &enabled)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn resolver_ignores_unlocked_dropin() {
        let tmp = tempdir().unwrap();
        let dropin = tmp.path().join("danger");
        fs::create_dir_all(&dropin).unwrap();
        fs::write(
            dropin.join("SKILL.md"),
            "---\nname: danger\n---\n# Danger\n\n1. Do not load",
        )
        .unwrap();
        let config = SkillCommandResolverConfig {
            roots: vec![tmp.path().to_path_buf()],
            ..SkillCommandResolverConfig::default()
        };
        assert!(
            resolve_installed_skill_slash_command("/danger", "", &config)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn snapshot_reports_tampered_blocked_and_filtered_packages() {
        let tmp = tempdir().unwrap();
        install_attested_local_skill(
            tmp.path(),
            "release-captain",
            "# Release Captain\n\n1. Inspect changes",
            "Ship releases safely",
        );
        install_attested_local_skill(
            tmp.path(),
            "danger",
            "# Danger\n\n1. Perform a safe action",
            "Danger",
        );
        install_attested_local_skill(
            tmp.path(),
            "mac-only",
            "# Mac only\n\n1. Use macOS",
            "Mac only",
        );
        fs::write(
            tmp.path().join("danger").join("SKILL.md"),
            "# Danger\n\n1. Tampered content",
        )
        .unwrap();
        let config = SkillCommandResolverConfig {
            roots: vec![tmp.path().to_path_buf()],
            enabled: vec!["release-captain".to_string()],
            platform: Some("linux".to_string()),
            ..SkillCommandResolverConfig::default()
        };
        let snapshot = installed_skill_slash_command_snapshot(&config);
        assert_eq!(snapshot.available.len(), 1);
        assert_eq!(snapshot.available[0].command, "/release-captain");
        assert_eq!(snapshot.blocked.len(), 1);
        assert_eq!(snapshot.blocked[0].command, "/danger");
        assert!(snapshot.blocked[0].blocked_reason.is_some());
        assert_eq!(snapshot.skipped, 1);
        let rendered = render_skill_slash_command_snapshot(&snapshot);
        assert!(rendered.contains("available=1 blocked=1 skipped=1"));
        assert!(rendered.contains("/release-captain"));
        let note = build_skill_reload_system_note(&snapshot);
        assert!(note.contains("/reload-skills"));
        assert!(note.contains("/release-captain"));
        assert!(note.contains("/danger"));
    }
}
