//! Skills management tools: list, view, and manage skills

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, SkillProvider, ToolError, ToolHandler, ToolSchema};

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use super::skill_utils::{
    discover_verified_skills, load_verified_skill_by_selector, read_verified_skill_file,
    render_skill_provenance_tag, SkillTrustRuntime,
};

// ---------------------------------------------------------------------------
// SkillsListHandler
// ---------------------------------------------------------------------------

/// Tool for listing only v2-admitted skills.
pub struct SkillsListHandler {
    skill_roots: Vec<PathBuf>,
    trust_runtime: Option<SkillTrustRuntime>,
}

impl SkillsListHandler {
    pub fn new(_provider: Arc<dyn SkillProvider>) -> Self {
        Self::with_skill_roots(_provider, default_skill_roots())
    }

    pub fn with_skill_roots(_provider: Arc<dyn SkillProvider>, skill_roots: Vec<PathBuf>) -> Self {
        Self {
            skill_roots,
            trust_runtime: SkillTrustRuntime::fail_closed().ok(),
        }
    }

    pub fn with_skill_roots_and_trust(
        _provider: Arc<dyn SkillProvider>,
        skill_roots: Vec<PathBuf>,
        trust_runtime: SkillTrustRuntime,
    ) -> Self {
        Self {
            skill_roots,
            trust_runtime: Some(trust_runtime),
        }
    }
}

#[async_trait]
impl ToolHandler for SkillsListHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let trust = self.trust_runtime.as_ref().ok_or_else(|| {
            ToolError::ExecutionFailed("no core skill trust verifier is configured".into())
        })?;
        let catalog = discover_verified_skills(&self.skill_roots, trust);
        let names: Vec<String> = catalog
            .available
            .iter()
            .map(|skill| skill.name.clone())
            .collect();
        let skills: Vec<Value> = catalog
            .available
            .iter()
            .map(|skill| {
                json!({
                    "name": skill.name,
                    "category": skill.frontmatter.get("category").and_then(Value::as_str),
                    "description": skill.frontmatter.get("description").and_then(Value::as_str),
                    "provenance": skill.provenance(),
                })
            })
            .collect();
        let blocked: Vec<Value> = catalog
            .blocked
            .iter()
            .map(|skill| json!({"name": skill.name, "reason": skill.reason}))
            .collect();

        Ok(serde_json::to_string_pretty(&json!({
            "count": skills.len(),
            "names": names,
            "skills": skills,
            "blocked_count": blocked.len(),
            "blocked": blocked,
        }))
        .unwrap_or_else(|_| "[]".to_string()))
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "skills_list",
            "List v2 lock-backed skills admitted by the core trust verifier.",
            JsonSchema::new("object"),
        )
    }
}

// ---------------------------------------------------------------------------
// SkillViewHandler
// ---------------------------------------------------------------------------

/// Tool for viewing a specific, freshly reverified skill package.
pub struct SkillViewHandler {
    skill_roots: Vec<PathBuf>,
    trust_runtime: Option<SkillTrustRuntime>,
}

impl SkillViewHandler {
    pub fn new(_provider: Arc<dyn SkillProvider>) -> Self {
        Self::with_skill_roots(_provider, default_skill_roots())
    }

    pub fn with_skill_roots(_provider: Arc<dyn SkillProvider>, skill_roots: Vec<PathBuf>) -> Self {
        Self {
            skill_roots,
            trust_runtime: SkillTrustRuntime::fail_closed().ok(),
        }
    }

    pub fn with_skill_roots_and_trust(
        _provider: Arc<dyn SkillProvider>,
        skill_roots: Vec<PathBuf>,
        trust_runtime: SkillTrustRuntime,
    ) -> Self {
        Self {
            skill_roots,
            trust_runtime: Some(trust_runtime),
        }
    }
}

#[async_trait]
impl ToolHandler for SkillViewHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|value| value.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
        let trust = self.trust_runtime.as_ref().ok_or_else(|| {
            ToolError::ExecutionFailed("no core skill trust verifier is configured".into())
        })?;
        let skill = load_verified_skill_by_selector(&self.skill_roots, name, trust)
            .map_err(ToolError::ExecutionFailed)?
            .ok_or_else(|| ToolError::NotFound(format!("Skill '{}' not found", name)))?;

        if let Some(file_path) = params
            .get("file_path")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if has_traversal_component(file_path) {
                return Ok(json!({
                    "success": false,
                    "error": "Path traversal ('..') is not allowed.",
                    "hint": "Use a relative path within the verified package"
                })
                .to_string());
            }

            let bytes = match read_verified_skill_file(&skill, file_path, trust) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return Ok(json!({
                        "success": false,
                        "error": format!("File '{}' is unavailable from the verified package: {}", file_path, error),
                        "hint": "Use a relative path contained in the admitted package"
                    })
                    .to_string())
                }
            };
            let content = String::from_utf8(bytes).map_err(|_| {
                ToolError::ExecutionFailed(
                    "verified package contained a non-UTF-8 file despite trust verification".into(),
                )
            })?;

            return Ok(json!({
                "success": true,
                "name": skill.name,
                "file": file_path,
                "content": content,
                "file_type": Path::new(file_path)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| format!(".{}", extension))
                    .unwrap_or_default(),
                "provenance": skill.provenance(),
            })
            .to_string());
        }

        Ok(format!(
            "{}\n\n{}",
            render_skill_provenance_tag(skill.provenance()),
            skill.content
        ))
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Canonical package name of the skill to view"
            }),
        );
        props.insert(
            "file_path".into(),
            json!({
                "type": "string",
                "description": "Optional relative file inside the verified package (e.g., references/api.md)"
            }),
        );

        tool_schema(
            "skill_view",
            "View a v2-admitted skill or a contained reverified package file.",
            JsonSchema::object(props, vec!["name".into()]),
        )
    }
}

fn default_skill_roots() -> Vec<PathBuf> {
    // `hermes_config::skills_dir` already performs the sole supported
    // legacy-home migration decision. Adding a second legacy root here would
    // create a competing lock authority and admit a package from the wrong
    // store.
    vec![hermes_config::skills_dir()]
}

fn has_traversal_component(path: &str) -> bool {
    let p = Path::new(path);
    if p.is_absolute() {
        return true;
    }
    p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

// ---------------------------------------------------------------------------
// SkillManageHandler
// ---------------------------------------------------------------------------

fn local_authored_staging_message(name: &str, action: &str) -> String {
    format!(
        "Skill '{}' {} as LocalAuthored staging and remains quarantined. An owner must record an explicit attestation before it can be loaded or used for effects.",
        name, action
    )
}

/// Tool for creating, updating, and deleting skills.
pub struct SkillManageHandler {
    provider: Arc<dyn SkillProvider>,
}

impl SkillManageHandler {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl ToolHandler for SkillManageHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "create" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let content = params.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'content' parameter".into()))?;
                let category = params.get("category").and_then(|v| v.as_str());

                self.provider.create_skill(name, content, category)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(local_authored_staging_message(name, "was created"))
            }
            "update" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let content = params.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'content' parameter".into()))?;

                self.provider.update_skill(name, content)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(local_authored_staging_message(name, "was updated"))
            }
            "delete" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;

                self.provider.delete_skill(name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!(
                    "Skill '{}' was removed from the skill store and is no longer loadable.",
                    name
                ))
            }
            "auto_create" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let summary = params.get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Auto-generated from completed task.");
                let steps = params.get("steps")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let mut content = format!("# {}\n\n## Summary\n{}\n\n## Steps\n", name, summary);
                for (idx, s) in steps.iter().enumerate() {
                    content.push_str(&format!("{}. {}\n", idx + 1, s));
                }
                self.provider.create_skill(name, &content, Some("auto-generated"))
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(local_authored_staging_message(name, "was auto-created"))
            }
            "self_improve" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let feedback = params.get("feedback")
                    .and_then(|v| v.as_str())
                    .unwrap_or("No feedback provided.");
                let existing = self.provider.get_skill(name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .ok_or_else(|| ToolError::NotFound(format!("Skill '{}' not found", name)))?;
                let improved = format!("{}\n\n## Improvement Feedback\n{}\n", existing.content, feedback);
                self.provider.update_skill(name, &improved)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(local_authored_staging_message(name, "was improved"))
            }
            "sync" => Ok("Skill sync request accepted (provider-specific hub sync path).".to_string()),
            "install_builtins" => {
                let builtins = [
                    "planning","debugging","refactoring","testing","docs","git","review","web-research",
                    "terminal","file-edit","security","performance","api-design","db-migrations",
                    "incident-response","release","prompting","agent-orchestration","mcp","gateway",
                    "cron","memory","session-search","tool-authoring","skill-authoring"
                ];
                let mut created = 0usize;
                for name in builtins {
                    let exists = self.provider.get_skill(name)
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                        .is_some();
                    if !exists {
                        let content = format!("# {}\n\n1. Understand\n2. Execute\n3. Verify\n", name);
                        self.provider.create_skill(name, &content, Some("builtin"))
                            .await
                            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                        created += 1;
                    }
                }
                Ok(format!(
                    "Staged {} generated skills as LocalAuthored. Each remains quarantined pending explicit owner attestation.",
                    created
                ))
            }
            other => Err(ToolError::InvalidParams(format!("Unknown action: '{}'. Use create/update/delete/auto_create/self_improve/sync/install_builtins.", other))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("action".into(), json!({
            "type": "string",
            "description": "Action to perform",
            "enum": ["create", "update", "delete", "auto_create", "self_improve", "sync", "install_builtins"]
        }));
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Name of the skill"
            }),
        );
        props.insert(
            "content".into(),
            json!({
                "type": "string",
                "description": "Skill content (for create/update). New or changed content is staged as LocalAuthored and requires explicit owner attestation before loading."
            }),
        );
        props.insert(
            "category".into(),
            json!({
                "type": "string",
                "description": "Skill category (for create)"
            }),
        );
        props.insert(
            "summary".into(),
            json!({
                "type": "string",
                "description": "Task summary used by auto_create"
            }),
        );
        props.insert(
            "steps".into(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Step list for auto_create"
            }),
        );
        props.insert(
            "feedback".into(),
            json!({
                "type": "string",
                "description": "Feedback used by self_improve"
            }),
        );

        tool_schema(
            "skill_manage",
            "Create, update, or delete skills. Create and update stage LocalAuthored content; they do not make it executable.",
            JsonSchema::object(props, vec!["action".into(), "name".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{AgentError, Skill, SkillMeta};
    use hermes_skills::{
        canonical_bundle_digest, full_skill_scan, store_lock, store_revocations, FileSkillStore,
        LocalOwnerAttestation, SignedSkillRevocationList, SkillCompatibility, SkillLockEntryV2,
        SkillLockV2, SkillManager, SkillPackageIdentityV2, SkillPackageManifestV2,
        SkillPackageProof, SkillPackageSource, SkillPermissions, SkillRevocation, SkillSignature,
        SkillSigningKey, SkillStore, SkillTrustKeyring, SkillTrustPolicy, SkillTrustVerifier,
        SKILL_LOCK_SCHEMA_VERSION, SKILL_REVOCATION_SCHEMA_VERSION,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    struct MockSkillProvider;
    #[async_trait]
    impl SkillProvider for MockSkillProvider {
        async fn create_skill(
            &self,
            name: &str,
            content: &str,
            category: Option<&str>,
        ) -> Result<Skill, AgentError> {
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: category.map(String::from),
                description: None,
            })
        }
        async fn get_skill(&self, name: &str) -> Result<Option<Skill>, AgentError> {
            Ok(Some(Skill {
                name: name.into(),
                content: "skill content".into(),
                category: None,
                description: None,
            }))
        }
        async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError> {
            Ok(vec![SkillMeta {
                name: "test".into(),
                category: None,
                description: None,
            }])
        }
        async fn update_skill(&self, name: &str, content: &str) -> Result<Skill, AgentError> {
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: None,
                description: None,
            })
        }
        async fn delete_skill(&self, _name: &str) -> Result<(), AgentError> {
            Ok(())
        }
    }

    async fn install_attested_local_skill(
        skills_root: &Path,
        name: &str,
        content: &str,
        extra_files: &[(&str, &str)],
    ) {
        let skill_dir = skills_root.join(name);
        for (relative_path, bytes) in extra_files {
            let path = skill_dir.join(relative_path);
            fs::create_dir_all(path.parent().expect("fixture parent")).unwrap();
            fs::write(path, bytes).unwrap();
        }
        let store = FileSkillStore::new(skills_root.to_path_buf());
        store
            .save(&Skill {
                name: name.to_string(),
                content: content.to_string(),
                category: None,
                description: Some("verified test skill".to_string()),
            })
            .await
            .unwrap();
        let digest = canonical_bundle_digest(&skill_dir).unwrap();
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

    fn install_signed_remote_skill(skills_root: &Path) -> (SkillTrustRuntime, SkillSigningKey) {
        let name = "signed-skill";
        let package_root = skills_root.join(name);
        fs::create_dir_all(&package_root).unwrap();
        fs::write(
            package_root.join("SKILL.md"),
            "---\nname: signed-skill\ndescription: Signed fixture\n---\n# Signed\n\n1. Follow verified instructions",
        )
        .unwrap();
        let signer = SkillSigningKey::from_seed([41; 32]);
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
            skills_root,
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

    fn revoke_signed_remote_skill(skills_root: &Path, signer: &SkillSigningKey) {
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
        store_revocations(skills_root, &revocations).unwrap();
    }

    #[tokio::test]
    async fn test_skills_list_excludes_unlocked_provider_rows() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(&skills_root, "locked-skill", "skill content", &[]).await;

        let handler =
            SkillsListHandler::with_skill_roots(Arc::new(MockSkillProvider), vec![skills_root]);
        let result = handler.execute(json!({})).await.unwrap();
        let payload: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(payload["count"], json!(1));
        assert_eq!(
            payload["names"][0],
            Value::String("locked-skill".to_string())
        );
        assert_eq!(
            payload["skills"][0]["provenance"]["source"],
            Value::String("local_authored".to_string())
        );
        assert_eq!(
            payload["skills"][0]["provenance"]["effects_allowed"],
            Value::Bool(false)
        );
    }

    #[tokio::test]
    async fn test_skill_view_returns_fresh_provenance_and_content() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(&skills_root, "test", "skill content", &[]).await;

        let handler =
            SkillViewHandler::with_skill_roots(Arc::new(MockSkillProvider), vec![skills_root]);
        let result = handler.execute(json!({"name": "test"})).await.unwrap();
        assert!(result.contains("<skill_provenance package=\"test\""));
        assert!(result.contains("skill content"));
    }

    #[tokio::test]
    async fn signed_skill_view_and_list_load_then_revocation_denies_both() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let (trust, signer) = install_signed_remote_skill(&skills_root);
        let list = SkillsListHandler::with_skill_roots_and_trust(
            Arc::new(MockSkillProvider),
            vec![skills_root.clone()],
            trust.clone(),
        );
        let view = SkillViewHandler::with_skill_roots_and_trust(
            Arc::new(MockSkillProvider),
            vec![skills_root.clone()],
            trust.clone(),
        );
        let initial_list: Value =
            serde_json::from_str(&list.execute(json!({})).await.unwrap()).unwrap();
        assert_eq!(initial_list["count"], Value::from(1));
        let initial_view = view.execute(json!({"name": "signed-skill"})).await.unwrap();
        assert!(initial_view.contains("Follow verified instructions"));
        assert!(initial_view.contains("key_id=\"test-key\""));

        revoke_signed_remote_skill(&skills_root, &signer);
        assert!(view.execute(json!({"name": "signed-skill"})).await.is_err());
        let revoked_list: Value =
            serde_json::from_str(&list.execute(json!({})).await.unwrap()).unwrap();
        assert_eq!(revoked_list["count"], Value::from(0));
        assert_eq!(revoked_list["blocked_count"], Value::from(1));
    }

    #[tokio::test]
    async fn test_skill_view_file_path_reads_reverified_reference_file() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(
            &skills_root,
            "test-skill",
            "skill content",
            &[("references/api.md", "API docs here")],
        )
        .await;

        let handler =
            SkillViewHandler::with_skill_roots(Arc::new(MockSkillProvider), vec![skills_root]);
        let result = handler
            .execute(json!({"name": "test-skill", "file_path": "references/api.md"}))
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(payload["success"], Value::Bool(true));
        assert_eq!(
            payload["content"],
            Value::String("API docs here".to_string())
        );
        assert_eq!(
            payload["provenance"]["package_name"],
            Value::String("test-skill".to_string())
        );
    }

    #[tokio::test]
    async fn test_skill_view_empty_file_path_uses_verified_content() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(&skills_root, "test", "skill content", &[]).await;

        let handler =
            SkillViewHandler::with_skill_roots(Arc::new(MockSkillProvider), vec![skills_root]);
        let result = handler
            .execute(json!({"name": "test", "file_path": ""}))
            .await
            .unwrap();
        assert!(result.contains("skill content"));
        assert!(result.contains("skill_provenance"));
    }

    #[tokio::test]
    async fn test_skill_view_file_path_blocks_dotdot_traversal() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(&skills_root, "test-skill", "skill content", &[]).await;
        fs::write(tmp.path().join(".env"), "SECRET_API_KEY=sk-do-not-leak").unwrap();

        let handler =
            SkillViewHandler::with_skill_roots(Arc::new(MockSkillProvider), vec![skills_root]);
        let result = handler
            .execute(json!({"name": "test-skill", "file_path": "../../.env"}))
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(payload["success"], Value::Bool(false));
        let error = payload["error"].as_str().unwrap_or("").to_ascii_lowercase();
        assert!(error.contains("traversal"));
        assert!(!result.contains("sk-do-not-leak"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_skill_view_tampered_symlink_is_quarantined_before_read() {
        use std::os::unix::fs as unix_fs;

        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        install_attested_local_skill(&skills_root, "test-skill", "skill content", &[]).await;
        let secret = tmp.path().join("secret.txt");
        fs::write(&secret, "TOP SECRET DATA").unwrap();
        unix_fs::symlink(&secret, skills_root.join("test-skill").join("evil-link")).unwrap();

        let handler =
            SkillViewHandler::with_skill_roots(Arc::new(MockSkillProvider), vec![skills_root]);
        let error = handler
            .execute(json!({"name": "test-skill", "file_path": "evil-link"}))
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("TOP SECRET DATA"));
        assert!(
            error.to_string().contains("quarantined")
                || error.to_string().contains("symlink")
                || error.to_string().contains("digest")
        );
    }
    #[tokio::test]
    async fn test_skill_manage_create_stages_pending_local_package() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let store = Arc::new(FileSkillStore::new(skills_root.clone()));
        let manager = Arc::new(SkillManager::new(store.clone()));
        let handler = SkillManageHandler::new(manager);

        let result = handler
            .execute(json!({
                "action": "create",
                "name": "new-skill",
                "content": "# Local draft\n\n1. Review the draft"
            }))
            .await
            .unwrap();
        assert!(result.contains("LocalAuthored staging"));

        let list =
            SkillsListHandler::with_skill_roots(Arc::new(MockSkillProvider), vec![skills_root]);
        let payload: Value = serde_json::from_str(&list.execute(json!({})).await.unwrap()).unwrap();
        assert_eq!(payload["count"], Value::from(0));
        assert_eq!(payload["blocked_count"], Value::from(1));
        assert!(payload["blocked"][0]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("PendingOwnerAttestation"));
        assert!(store.load("new-skill").await.is_err());
    }

    #[tokio::test]
    async fn test_skill_manage_delete_removes_pending_package_from_verified_catalog() {
        let tmp = tempdir().unwrap();
        let skills_root = tmp.path().join("skills");
        let store = Arc::new(FileSkillStore::new(skills_root.clone()));
        let handler = SkillManageHandler::new(Arc::new(SkillManager::new(store.clone())));

        handler
            .execute(json!({
                "action": "create",
                "name": "remove-me",
                "content": "# Local draft\n\n1. Review the draft"
            }))
            .await
            .unwrap();
        handler
            .execute(json!({"action": "delete", "name": "remove-me"}))
            .await
            .unwrap();

        assert!(store.load("remove-me").await.unwrap().is_none());
        let list =
            SkillsListHandler::with_skill_roots(Arc::new(MockSkillProvider), vec![skills_root]);
        let payload: Value = serde_json::from_str(&list.execute(json!({})).await.unwrap()).unwrap();
        assert_eq!(payload["count"], Value::from(0));
        assert_eq!(payload["blocked_count"], Value::from(0));
    }
}
