//! Skill orchestrator and slash command system.
//!
//! Resolves only v2 lock-backed skill packages through the shared trust store,
//! builds a `/command` → verified package mapping, and provides helpers for
//! slash invocation and session-wide preloading.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use hermes_tools::skill_utils::{
    discover_verified_skills, reload_verified_skill, render_skill_provenance_tag,
    skill_trust_runtime_for_skills_root, SkillInfo, SkillTrustIdentity, SkillTrustRuntime,
};
use regex::Regex;

// ---------------------------------------------------------------------------
// Skill scaffolding extraction
// ---------------------------------------------------------------------------

const SKILL_INVOCATION_SYSTEM_PREFIX: &str = "[SYSTEM: The user has invoked the ";
const SKILL_INVOCATION_IMPORTANT_PREFIX: &str = "[IMPORTANT: The user has invoked the ";
const SINGLE_SKILL_MARKER: &str = "The full skill content is loaded below.]";
const SINGLE_SKILL_INSTRUCTION: &str =
    "The user has provided the following instruction alongside the skill invocation: ";
const RUNTIME_NOTE_MARKER: &str = "\n\n[Runtime note:";
const BUNDLE_MARKER: &str = " skill bundle,";
const BUNDLE_USER_INSTRUCTION: &str = "\nUser instruction: ";
const BUNDLE_FIRST_SKILL_BLOCK: &str = "\n\n[Loaded as part of the ";

/// Recover the user's instruction from an expanded slash-skill turn.
///
/// Normal user messages pass through unchanged. Skill invocations with a user
/// instruction return only that instruction. Bare skill invocations return
/// `None` so memory providers can skip the prompt-scaffolding-only turn.
pub fn extract_user_instruction_from_skill_message(content: &str) -> Option<Cow<'_, str>> {
    if !content.starts_with(SKILL_INVOCATION_SYSTEM_PREFIX)
        && !content.starts_with(SKILL_INVOCATION_IMPORTANT_PREFIX)
    {
        return Some(Cow::Borrowed(content));
    }

    let header = content.lines().next().unwrap_or(content);

    if header.contains(BUNDLE_MARKER) {
        return extract_bundle_user_instruction(content).map(Cow::Owned);
    }

    if header.contains(SINGLE_SKILL_MARKER) {
        return extract_single_skill_user_instruction(content).map(Cow::Owned);
    }

    None
}

fn extract_single_skill_user_instruction(message: &str) -> Option<String> {
    let marker_idx = message.rfind(SINGLE_SKILL_INSTRUCTION)?;
    let mut instruction = &message[marker_idx + SINGLE_SKILL_INSTRUCTION.len()..];
    if let Some(runtime_idx) = instruction.find(RUNTIME_NOTE_MARKER) {
        instruction = &instruction[..runtime_idx];
    }
    let instruction = instruction.trim();
    (!instruction.is_empty()).then(|| instruction.to_string())
}

fn extract_bundle_user_instruction(message: &str) -> Option<String> {
    let marker_idx = message.find(BUNDLE_USER_INSTRUCTION)?;
    let mut instruction = &message[marker_idx + BUNDLE_USER_INSTRUCTION.len()..];
    if let Some(first_skill_idx) = instruction.find(BUNDLE_FIRST_SKILL_BLOCK) {
        instruction = &instruction[..first_skill_idx];
    }
    let instruction = instruction.trim();
    (!instruction.is_empty()).then(|| instruction.to_string())
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Information about a registered, currently verified skill command.
#[derive(Debug, Clone)]
pub struct SkillCommandInfo {
    /// Canonical package name from the verified manifest.
    pub name: String,
    /// Short description from verified package frontmatter.
    pub description: String,
    /// Typed immutable package identity included in prompt evidence.
    pub provenance: SkillTrustIdentity,
    verified_skill: SkillInfo,
}

/// Parsed SKILL.md frontmatter.
#[derive(Debug, Clone, Default)]
pub struct SkillFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub platform: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Frontmatter parsing
// ---------------------------------------------------------------------------

/// Parse YAML frontmatter from a SKILL.md file.
///
/// Expects the file to start with `---\n...\n---\n` followed by the body.
/// Returns (frontmatter, body).
pub fn parse_frontmatter(content: &str) -> (SkillFrontmatter, &str) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (SkillFrontmatter::default(), content);
    }

    // Find the closing ---
    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);

    if let Some(end_idx) = after_first.find("\n---") {
        let yaml_block = &after_first[..end_idx];
        let body_start = end_idx + 4; // skip \n---
        let body = after_first[body_start..].trim_start_matches(['\r', '\n']);

        let fm = parse_yaml_frontmatter(yaml_block);
        (fm, body)
    } else {
        (SkillFrontmatter::default(), content)
    }
}

/// Minimal YAML frontmatter parser (no full YAML dependency needed).
fn parse_yaml_frontmatter(yaml: &str) -> SkillFrontmatter {
    let mut fm = SkillFrontmatter::default();

    for line in yaml.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'');
            match key {
                "name" => fm.name = Some(value.to_string()),
                "description" => fm.description = Some(value.to_string()),
                _ => {}
            }
        }
    }

    fm
}

// ---------------------------------------------------------------------------
// Slug normalization
// ---------------------------------------------------------------------------

lazy_static::lazy_static! {
    static ref SKILL_INVALID_CHARS: Regex = Regex::new(r"[^a-z0-9-]").unwrap();
    static ref SKILL_MULTI_HYPHEN: Regex = Regex::new(r"-{2,}").unwrap();
}

/// Normalize a skill name into a clean hyphen-separated slug.
fn slugify_skill_name(name: &str) -> String {
    let lower = name.to_lowercase().replace([' ', '_'], "-");
    let cleaned = SKILL_INVALID_CHARS.replace_all(&lower, "");
    let deduped = SKILL_MULTI_HYPHEN.replace_all(&cleaned, "-");
    deduped.trim_matches('-').to_string()
}

// ---------------------------------------------------------------------------
// SkillOrchestrator
// ---------------------------------------------------------------------------

/// Manages verified package cataloging, slash command resolution, and prompt
/// construction. The core verifier is consulted again before every load.
pub struct SkillOrchestrator {
    /// Mapping from `/command-name` to verified skill info.
    commands: HashMap<String, SkillCommandInfo>,
    /// Actual v2 skill root.
    skills_dir: PathBuf,
    /// Core verifier context; absent context fails closed.
    trust_runtime: Option<SkillTrustRuntime>,
    /// Provisioning failure retained so catalog scans explain their fail-closed
    /// result without accepting an alternate trust source.
    trust_runtime_error: Option<String>,
    /// Explicit allow-list of skill identifiers (`name` and `/command` forms).
    enabled: HashSet<String>,
    /// Explicit deny-list of skill identifiers (`name` and `/command` forms).
    disabled: HashSet<String>,
}

fn record_security_skip(skips: &mut HashMap<String, Vec<String>>, skill_name: &str, reason: &str) {
    let reason = reason
        .replace('\n', " ")
        .replace('\r', " ")
        .trim()
        .to_string();
    skips
        .entry(reason)
        .or_default()
        .push(skill_name.to_string());
}

fn emit_security_skip_summary(skips: &HashMap<String, Vec<String>>) {
    if skips.is_empty() {
        return;
    }
    let total: usize = skips.values().map(Vec::len).sum();
    let mut buckets: Vec<(&String, &Vec<String>)> = skips.iter().collect();
    buckets.sort_by(|(reason_a, names_a), (reason_b, names_b)| {
        names_b
            .len()
            .cmp(&names_a.len())
            .then_with(|| reason_a.cmp(reason_b))
    });
    let mut details = Vec::new();
    for (reason, names) in buckets.iter().take(4) {
        let mut sample: Vec<String> = names.iter().take(3).cloned().collect();
        if names.len() > 3 {
            sample.push(format!("+{} more", names.len() - 3));
        }
        details.push(format!(
            "{}x {} [{}]",
            names.len(),
            reason,
            sample.join(", ")
        ));
    }
    tracing::warn!(
        "Skipped {} skill(s) due to security policy. {}",
        total,
        details.join(" | ")
    );
}

impl SkillOrchestrator {
    /// Create an orchestrator bound to the canonical external trust authority
    /// layout for its supplied skill root.
    pub fn new(skills_dir: impl Into<PathBuf>) -> Self {
        let skills_dir = skills_dir.into();
        let (trust_runtime, trust_runtime_error) =
            match skill_trust_runtime_for_skills_root(&skills_dir) {
                Ok(runtime) => (Some(runtime), None),
                Err(error) => (None, Some(error)),
            };
        Self {
            commands: HashMap::new(),
            skills_dir,
            trust_runtime,
            trust_runtime_error,
            enabled: HashSet::new(),
            disabled: HashSet::new(),
        }
    }

    /// Inject a runtime-owned verifier context (for trusted keys, embedded
    /// manifests, or deterministic tests) without accepting a source label as
    /// evidence.
    pub fn with_trust_runtime(
        skills_dir: impl Into<PathBuf>,
        trust_runtime: SkillTrustRuntime,
    ) -> Self {
        Self {
            commands: HashMap::new(),
            skills_dir: skills_dir.into(),
            trust_runtime: Some(trust_runtime),
            trust_runtime_error: None,
            enabled: HashSet::new(),
            disabled: HashSet::new(),
        }
    }

    /// Build the command mapping from fresh v2 lock admissions. Missing,
    /// legacy, tampered, revoked, incompatible, or quarantined packages never
    /// enter `commands`.
    pub fn scan_skill_commands(&mut self) -> &HashMap<String, SkillCommandInfo> {
        self.commands.clear();
        let mut seen_names = std::collections::HashSet::new();
        let mut security_skips: HashMap<String, Vec<String>> = HashMap::new();
        let Some(trust) = self.trust_runtime.clone() else {
            record_security_skip(
                &mut security_skips,
                "<skill-store>",
                self.trust_runtime_error
                    .as_deref()
                    .unwrap_or("no core skill trust verifier is configured"),
            );
            emit_security_skip_summary(&security_skips);
            return &self.commands;
        };

        let catalog = discover_verified_skills(&[self.skills_dir.clone()], &trust);
        for rejection in catalog.blocked {
            record_security_skip(&mut security_skips, &rejection.name, &rejection.reason);
        }
        for skill in catalog.available {
            self.register_verified_skill(skill, &mut seen_names, &mut security_skips);
        }
        emit_security_skip_summary(&security_skips);

        &self.commands
    }

    /// Apply runtime skill filters from config (parity with Python skills gating).
    pub fn set_enabled_disabled(&mut self, enabled: &[String], disabled: &[String]) {
        self.enabled = enabled
            .iter()
            .map(|s| Self::normalize_skill_selector(s))
            .filter(|s| !s.is_empty())
            .collect();
        self.disabled = disabled
            .iter()
            .map(|s| Self::normalize_skill_selector(s))
            .filter(|s| !s.is_empty())
            .collect();
    }

    fn normalize_skill_selector(raw: &str) -> String {
        raw.trim()
            .trim_start_matches('/')
            .replace('_', "-")
            .to_ascii_lowercase()
    }

    fn is_skill_allowed(&self, name: &str, cmd_name: &str) -> bool {
        let name_key = Self::normalize_skill_selector(name);
        let cmd_key = Self::normalize_skill_selector(cmd_name);
        if self.disabled.contains(&name_key) || self.disabled.contains(&cmd_key) {
            return false;
        }
        if self.enabled.is_empty() {
            return true;
        }
        self.enabled.contains(&name_key) || self.enabled.contains(&cmd_key)
    }

    /// Register a package that has already passed the core verifier. The
    /// frontmatter can describe it, but cannot rename or authorize it.
    fn register_verified_skill(
        &mut self,
        skill: SkillInfo,
        seen_names: &mut std::collections::HashSet<String>,
        security_skips: &mut HashMap<String, Vec<String>>,
    ) {
        if !skill.is_verified() {
            record_security_skip(
                security_skips,
                &skill.name,
                "skill lacks a verified package locator",
            );
            return;
        }
        let name = skill.name.clone();
        if !seen_names.insert(name.clone()) {
            record_security_skip(
                security_skips,
                &name,
                "duplicate canonical package name in verified catalog",
            );
            return;
        }

        let description = skill
            .frontmatter
            .get("description")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                skill
                    .body
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(|line| line.chars().take(80).collect())
                    .unwrap_or_else(|| format!("Invoke the {} skill", name))
            });

        let cmd_name = slugify_skill_name(&name);
        if cmd_name.is_empty() {
            record_security_skip(security_skips, &name, "empty canonical command slug");
            return;
        }
        if !self.is_skill_allowed(&name, &cmd_name) {
            tracing::debug!(
                "Skipping verified skill '{}' due to enabled/disabled filters",
                name
            );
            return;
        }

        self.commands.insert(
            format!("/{cmd_name}"),
            SkillCommandInfo {
                name,
                description,
                provenance: skill.provenance().clone(),
                verified_skill: skill,
            },
        );
    }

    /// Get the current commands mapping.
    pub fn get_commands(&self) -> &HashMap<String, SkillCommandInfo> {
        &self.commands
    }

    /// Return verified frontmatter category metadata, falling back to the
    /// immutable install path for legacy packages.
    pub fn skill_category(&self, info: &SkillCommandInfo) -> Option<String> {
        info.verified_skill
            .frontmatter
            .get("category")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|category| !category.is_empty())
            .map(str::to_string)
            .or_else(|| info.verified_skill.category().map(str::to_string))
    }

    /// Resolve a user-typed `/command` to its canonical key.
    ///
    /// Hyphens and underscores are treated interchangeably.
    pub fn resolve_skill_command_key(&self, command: &str) -> Option<String> {
        if command.is_empty() {
            return None;
        }
        let normalized = format!("/{}", command.trim_start_matches('/').replace('_', "-"));
        if self.commands.contains_key(&normalized) {
            Some(normalized)
        } else {
            None
        }
    }

    /// Build the user message content for a freshly reverified skill slash
    /// invocation.
    pub fn build_skill_invocation_message(
        &self,
        cmd_key: &str,
        user_instruction: &str,
    ) -> Option<String> {
        let info = self.commands.get(cmd_key)?;
        let trust = self.trust_runtime.as_ref()?;
        let loaded = match reload_verified_skill(&info.verified_skill, trust) {
            Ok(skill) => skill,
            Err(error) => {
                tracing::warn!(
                    skill = %info.name,
                    error = %error,
                    "blocked skill invocation after fresh trust verification"
                );
                return None;
            }
        };

        let mut parts = vec![
            format!(
                "[SYSTEM: The user has invoked the \"{}\" skill, indicating they want \
                 you to follow its instructions. The full skill content is loaded below.]",
                loaded.name
            ),
            String::new(),
            render_skill_provenance_tag(loaded.provenance()),
            String::new(),
            loaded.body.trim().to_string(),
        ];

        if !user_instruction.is_empty() {
            parts.push(String::new());
            parts.push(format!(
                "The user has provided the following instruction alongside the skill invocation: {}",
                user_instruction
            ));
        }

        Some(parts.join("\n"))
    }

    /// Load multiple verified packages for session-wide preloading.
    ///
    /// Returns (prompt_text, loaded_skill_names, missing_identifiers). Every
    /// selected package is reverified against the current lock, revocations,
    /// compatibility, scan, and content digest immediately before injection.
    pub fn build_preloaded_skills_prompt(
        &self,
        identifiers: &[String],
    ) -> (String, Vec<String>, Vec<String>) {
        let mut prompt_parts = Vec::new();
        let mut loaded_names = Vec::new();
        let mut missing = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let Some(trust) = self.trust_runtime.as_ref() else {
            missing.extend(
                identifiers
                    .iter()
                    .map(|identifier| identifier.trim())
                    .filter(|identifier| !identifier.is_empty())
                    .map(str::to_string),
            );
            return (String::new(), loaded_names, missing);
        };

        for identifier in identifiers {
            let identifier = identifier.trim();
            if identifier.is_empty() || !seen.insert(identifier.to_string()) {
                continue;
            }

            let cmd_key = self.resolve_skill_command_key(identifier);
            let info = cmd_key.as_ref().and_then(|key| self.commands.get(key));

            match info {
                Some(info) => match reload_verified_skill(&info.verified_skill, trust) {
                    Ok(loaded) => {
                        let activation = format!(
                            "[SYSTEM: The user launched this CLI session with the \"{}\" skill \
                             preloaded. Treat its instructions as active guidance for the duration \
                             of this session unless the user overrides them.]",
                            loaded.name
                        );
                        prompt_parts.push(format!(
                            "{}\n{}\n\n{}",
                            activation,
                            render_skill_provenance_tag(loaded.provenance()),
                            loaded.body.trim()
                        ));
                        loaded_names.push(loaded.name);
                    }
                    Err(error) => {
                        tracing::warn!(
                            skill = %info.name,
                            error = %error,
                            "skipping preloaded skill after fresh trust verification"
                        );
                        missing.push(identifier.to_string());
                    }
                },
                None => missing.push(identifier.to_string()),
            }
        }

        (prompt_parts.join("\n\n"), loaded_names, missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::Skill;
    use hermes_skills::{
        canonical_bundle_digest, full_skill_scan, load_lock, store_local_owner_keyring_file,
        store_lock, store_revocations, FileSkillStore, LocalOwnerKeyring, LocalOwnerSigningKey,
        SignedSkillRevocationList, SkillCompatibility, SkillLockEntryV2, SkillLockV2,
        SkillPackageIdentityV2, SkillPackageManifestV2, SkillPackageProof, SkillPackageSource,
        SkillPermissions, SkillRevocation, SkillSignature, SkillSigningKey, SkillStore,
        SkillTrustKeyring, SkillTrustPolicy, SkillTrustVerifier, SKILL_LOCK_SCHEMA_VERSION,
        SKILL_REVOCATION_SCHEMA_VERSION,
    };
    use std::collections::BTreeMap;

    fn install_attested_local_skill(
        root: &std::path::Path,
        name: &str,
        body: &str,
        description: &str,
    ) {
        install_attested_local_skill_with_category(root, name, body, description, None);
    }

    fn install_attested_local_skill_with_category(
        root: &std::path::Path,
        name: &str,
        body: &str,
        description: &str,
        category: Option<&str>,
    ) {
        std::fs::create_dir_all(root).expect("create skill root");
        let home = root.parent().expect("skills root has a home");
        let layout = hermes_config::SkillTrustAuthorityLayout::from_resolved_home(home)
            .expect("derive skill trust authority layout");
        assert_eq!(layout.skills_root(), root);
        std::fs::create_dir_all(layout.authority_root()).expect("create skill authority root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                layout.authority_root(),
                std::fs::Permissions::from_mode(0o700),
            )
            .expect("restrict skill authority root");
        }

        let signer = LocalOwnerSigningKey::from_seed([29; 32]);
        let local_owner_keys = vec![signer.trusted_key("test-owner", "test-owner-key", true)];
        let keyring =
            LocalOwnerKeyring::new(local_owner_keys.clone()).expect("create local owner keyring");
        store_local_owner_keyring_file(root, layout.local_owner_keyring_file(), &keyring)
            .expect("store local owner keyring");

        let store =
            FileSkillStore::new(root.to_path_buf()).with_trusted_local_owner_keys(local_owner_keys);
        futures::executor::block_on(store.save(&Skill {
            name: name.to_string(),
            content: body.to_string(),
            category: category.map(str::to_string),
            description: Some(description.to_string()),
        }))
        .expect("stage local skill");
        let mut manifest = store
            .pending_local_manifest(name)
            .expect("load staged local skill")
            .expect("staged local skill manifest");
        let attestation = signer
            .sign_local_manifest(
                &mut manifest,
                "test-owner",
                "test-owner-key",
                format!("test-attestation-{name}"),
                "2026-07-20T00:00:00Z",
            )
            .expect("sign local skill attestation");
        store
            .attest_local_skill(name, attestation)
            .expect("attest local skill");
    }

    fn install_signed_remote_skill(root: &std::path::Path) -> (SkillTrustRuntime, SkillSigningKey) {
        let name = "signed-skill";
        let package_root = root.join(name);
        std::fs::create_dir_all(&package_root).unwrap();
        std::fs::write(
            package_root.join("SKILL.md"),
            "---\nname: signed-skill\ndescription: Signed fixture\n---\n# Signed\n\n1. Follow verified instructions",
        )
        .unwrap();
        let signer = SkillSigningKey::from_seed([31; 32]);
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
        signer
            .sign_revocations(&mut revocations, "test-key")
            .unwrap();
        store_revocations(root, &revocations).unwrap();
    }

    #[test]
    fn test_parse_frontmatter_with_yaml() {
        let content = "---\nname: my-skill\ndescription: A test skill\n---\n# Body\nHello world";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.name.as_deref(), Some("my-skill"));
        assert_eq!(fm.description.as_deref(), Some("A test skill"));
        assert!(body.contains("Hello world"));
    }

    #[test]
    fn test_parse_frontmatter_without_yaml() {
        let content = "# Just a body\nNo frontmatter here.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.name.is_none());
        assert!(body.contains("Just a body"));
    }

    #[test]
    fn test_slugify_skill_name() {
        assert_eq!(slugify_skill_name("My Cool Skill"), "my-cool-skill");
        assert_eq!(slugify_skill_name("claude_code"), "claude-code");
        assert_eq!(slugify_skill_name("gif+search"), "gifsearch");
        assert_eq!(slugify_skill_name("---test---"), "test");
    }

    #[test]
    fn test_resolve_skill_command_key() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(
            skills_root.as_path(),
            "my-skill",
            "# My skill\n\n1. Follow the skill",
            "test",
        );
        let mut orch = SkillOrchestrator::new(skills_root);
        orch.scan_skill_commands();

        assert_eq!(
            orch.resolve_skill_command_key("my-skill"),
            Some("/my-skill".to_string())
        );
        assert_eq!(
            orch.resolve_skill_command_key("my_skill"),
            Some("/my-skill".to_string())
        );
        assert_eq!(
            orch.resolve_skill_command_key("/my-skill"),
            Some("/my-skill".to_string())
        );
        assert!(orch.resolve_skill_command_key("nonexistent").is_none());
        assert!(orch.resolve_skill_command_key("").is_none());
    }

    #[test]
    fn test_scan_skill_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(
            skills_root.as_path(),
            "test-skill",
            "# Test\n\n1. Do the thing",
            "A test",
        );

        let mut orch = SkillOrchestrator::new(skills_root);
        orch.scan_skill_commands();

        assert!(orch.commands.contains_key("/test-skill"));
        let info = &orch.commands["/test-skill"];
        assert_eq!(info.name, "test-skill");
        assert_eq!(info.description, "A test");
        assert_eq!(
            info.provenance.source,
            hermes_skills::SkillPackageSource::LocalAuthored
        );
        assert!(!info.provenance.effects_allowed);
    }

    #[test]
    fn test_build_skill_invocation_message() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(
            skills_root.as_path(),
            "greet",
            "# Greet\n\n1. Say hello to the user.",
            "Greeting skill",
        );

        let mut orch = SkillOrchestrator::new(skills_root);
        orch.scan_skill_commands();

        let msg = orch
            .build_skill_invocation_message("/greet", "in French")
            .unwrap();
        assert!(msg.contains("greet"));
        assert!(msg.contains("Say hello to the user."));
        assert!(msg.contains("in French"));
        assert!(msg.contains("<skill_provenance"));
    }

    #[test]
    fn test_tampered_skill_is_denied_after_catalog_before_invocation() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(
            skills_root.as_path(),
            "greet",
            "# Greet\n\n1. Say hello to the user.",
            "Greeting skill",
        );

        let mut orch = SkillOrchestrator::new(skills_root.clone());
        orch.scan_skill_commands();
        let lock = load_lock(&skills_root).expect("load skill lock");
        let install_path = lock
            .packages
            .iter()
            .find(|entry| entry.manifest.identity.name == "greet")
            .expect("greet lock entry")
            .install_path
            .clone();
        std::fs::write(
            skills_root.join(install_path).join("SKILL.md"),
            "# Greet\n\n1. Inject tampered body",
        )
        .unwrap();

        assert!(orch
            .build_skill_invocation_message("/greet", "in French")
            .is_none());
        let (prompt, loaded, missing) = orch.build_preloaded_skills_prompt(&["greet".to_string()]);
        assert!(prompt.is_empty());
        assert!(loaded.is_empty());
        assert_eq!(missing, vec!["greet"]);
    }

    #[test]
    fn signed_orchestrator_package_loads_then_revocation_denies_invoke_and_preload() {
        let tmp = tempfile::tempdir().unwrap();
        let (trust, signer) = install_signed_remote_skill(tmp.path());
        let mut orch = SkillOrchestrator::with_trust_runtime(tmp.path(), trust);
        orch.scan_skill_commands();
        assert!(orch.commands.contains_key("/signed-skill"));
        let message = orch
            .build_skill_invocation_message("/signed-skill", "verify")
            .expect("signed skill must load");
        assert!(message.contains("key_id=\"test-key\""));
        assert!(message.contains("Follow verified instructions"));

        revoke_signed_remote_skill(tmp.path(), &signer);
        assert!(orch
            .build_skill_invocation_message("/signed-skill", "verify")
            .is_none());
        let (prompt, loaded, missing) =
            orch.build_preloaded_skills_prompt(&["signed-skill".to_string()]);
        assert!(prompt.is_empty());
        assert!(loaded.is_empty());
        assert_eq!(missing, vec!["signed-skill"]);
        orch.scan_skill_commands();
        assert!(orch.commands.is_empty());
    }

    #[test]
    fn extract_user_instruction_passes_plain_message_through() {
        assert_eq!(
            extract_user_instruction_from_skill_message("just a normal request").as_deref(),
            Some("just a normal request")
        );
    }

    #[test]
    fn extract_user_instruction_strips_single_skill_scaffolding() {
        let message = concat!(
            "[SYSTEM: The user has invoked the \"skill-creator\" skill, indicating they want ",
            "you to follow its instructions. The full skill content is loaded below.]\n\n",
            "# Skill Creator\n\n",
            "Large skill body that must not be embedded.\n\n",
            "The user has provided the following instruction alongside the skill invocation: ",
            "make a skill for release triage"
        );

        assert_eq!(
            extract_user_instruction_from_skill_message(message).as_deref(),
            Some("make a skill for release triage")
        );
    }

    #[test]
    fn extract_user_instruction_skips_bare_skill_scaffolding() {
        let message = concat!(
            "[SYSTEM: The user has invoked the \"skill-creator\" skill, indicating they want ",
            "you to follow its instructions. The full skill content is loaded below.]\n\n",
            "# Skill Creator\n\n",
            "Large skill body, no user instruction."
        );

        assert!(extract_user_instruction_from_skill_message(message).is_none());
    }

    #[test]
    fn extract_user_instruction_trims_runtime_note() {
        let message = concat!(
            "[SYSTEM: The user has invoked the \"skill-creator\" skill, indicating they want ",
            "you to follow its instructions. The full skill content is loaded below.]\n\n",
            "# Skill Creator\n\n",
            "The user has provided the following instruction alongside the skill invocation: ",
            "make a skill for release triage\n\n",
            "[Runtime note: delegated subagent context]"
        );

        assert_eq!(
            extract_user_instruction_from_skill_message(message).as_deref(),
            Some("make a skill for release triage")
        );
    }

    #[test]
    fn extract_user_instruction_strips_bundle_scaffolding() {
        let message = concat!(
            "[IMPORTANT: The user has invoked the \"backend-dev\" skill bundle, ",
            "loading 2 skills together. Treat every skill below as active guidance for this turn.]\n\n",
            "Bundle: backend-dev\n",
            "Skills loaded: test-driven-development, code-review\n\n",
            "User instruction: fix the failing retrieval test\n\n",
            "[Loaded as part of the \"backend-dev\" skill bundle.]\n\n",
            "Large bundled skill body that must not be searched or embedded."
        );

        assert_eq!(
            extract_user_instruction_from_skill_message(message).as_deref(),
            Some("fix the failing retrieval test")
        );
    }

    #[test]
    fn test_build_preloaded_skills_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(
            skills_root.as_path(),
            "coder",
            "# Coder\n\n1. Write clean code.",
            "Coding assistant",
        );

        let mut orch = SkillOrchestrator::new(skills_root);
        orch.scan_skill_commands();

        let (prompt, loaded, missing) =
            orch.build_preloaded_skills_prompt(&["coder".to_string(), "nonexistent".to_string()]);

        assert_eq!(loaded, vec!["coder"]);
        assert_eq!(missing, vec!["nonexistent"]);
        assert!(prompt.contains("Write clean code."));
        assert!(prompt.contains("preloaded"));
        assert!(prompt.contains("<skill_provenance"));
    }

    #[test]
    fn test_scan_skill_commands_respects_disabled_filters() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(&skills_root, "alpha", "# Alpha\n\n1. Alpha", "Alpha skill");
        install_attested_local_skill(&skills_root, "beta", "# Beta\n\n1. Beta", "Beta skill");

        let mut orch = SkillOrchestrator::new(skills_root);
        orch.set_enabled_disabled(&[], &["alpha".to_string()]);
        orch.scan_skill_commands();
        assert!(!orch.commands.contains_key("/alpha"));
        assert!(orch.commands.contains_key("/beta"));
    }

    #[test]
    fn test_scan_skill_commands_respects_enabled_filters() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(&skills_root, "alpha", "# Alpha\n\n1. Alpha", "Alpha skill");
        install_attested_local_skill(&skills_root, "beta", "# Beta\n\n1. Beta", "Beta skill");

        let mut orch = SkillOrchestrator::new(skills_root);
        orch.set_enabled_disabled(&["beta".to_string()], &[]);
        orch.scan_skill_commands();
        assert!(!orch.commands.contains_key("/alpha"));
        assert!(orch.commands.contains_key("/beta"));
    }

    #[test]
    fn test_skill_category_uses_verified_frontmatter_for_local_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill_with_category(
            &skills_root,
            "categorized",
            "# Categorized\n\n1. Keep category metadata",
            "Category fixture",
            Some("coding"),
        );

        let mut orch = SkillOrchestrator::new(skills_root);
        orch.scan_skill_commands();
        let info = orch
            .get_commands()
            .get("/categorized")
            .expect("verified categorized skill");
        assert_eq!(orch.skill_category(info).as_deref(), Some("coding"));
    }

    #[test]
    fn test_record_security_skip_sanitizes_multiline_reason() {
        let mut skips: HashMap<String, Vec<String>> = HashMap::new();
        record_security_skip(&mut skips, "alpha", "line one\nline two\rline three");
        let (reason, names) = skips.into_iter().next().expect("entry");
        assert_eq!(reason, "line one line two line three");
        assert_eq!(names, vec!["alpha".to_string()]);
    }
}
