use super::*;
use std::process::Command;
use std::sync::{mpsc, Arc, Barrier, Mutex};

const PRIVATE_SENTINEL: &str = "PRIVATE-SENTINEL-9e3f";

fn synthetic_openai_key() -> String {
    ["sk", "-", "abcdefghijklmnopqrstuvwxyz123456"].concat()
}

#[derive(Default)]
struct MemoryCredentialStore {
    secrets: Mutex<HashMap<(String, String), Vec<u8>>>,
    provision_lock: Arc<EphemeralCredentialProvisionLock>,
}

impl MemoryCredentialStore {
    fn remove_secret(&self, service: &str, account: &str) {
        self.secrets
            .lock()
            .unwrap()
            .remove(&(service.to_string(), account.to_string()));
    }

    fn replace_secret(&self, service: &str, account: &str, secret: Vec<u8>) {
        self.secrets
            .lock()
            .unwrap()
            .insert((service.to_string(), account.to_string()), secret);
    }
}

impl ArtifactCredentialStore for MemoryCredentialStore {
    fn load_secret(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, SealedArtifactError> {
        Ok(self
            .secrets
            .lock()
            .unwrap()
            .get(&(service.to_string(), account.to_string()))
            .cloned()
            .map(Zeroizing::new))
    }

    fn store_secret(
        &self,
        service: &str,
        account: &str,
        secret: &[u8],
    ) -> Result<(), SealedArtifactError> {
        self.secrets
            .lock()
            .unwrap()
            .insert((service.to_string(), account.to_string()), secret.to_vec());
        Ok(())
    }

    fn acquire_provision_lock(
        &self,
        _service: &str,
        _store_namespace: &str,
    ) -> Result<Box<dyn ArtifactCredentialProvisionGuard>, SealedArtifactError> {
        let mut held = self.provision_lock.held.lock().map_err(|_| {
            SealedArtifactError::KeyUnavailable(
                "test credential provision lock is poisoned".to_string(),
            )
        })?;
        while *held {
            held = self.provision_lock.available.wait(held).map_err(|_| {
                SealedArtifactError::KeyUnavailable(
                    "test credential provision lock is poisoned".to_string(),
                )
            })?;
        }
        *held = true;
        drop(held);
        Ok(Box::new(EphemeralCredentialProvisionGuard {
            lock: self.provision_lock.clone(),
        }))
    }
}

#[cfg(unix)]
#[derive(Clone)]
struct TestFileCredentialStore {
    root: PathBuf,
}

#[cfg(unix)]
struct TestFileCredentialProvisionGuard {
    _file: File,
}

#[cfg(unix)]
impl ArtifactCredentialProvisionGuard for TestFileCredentialProvisionGuard {}

#[cfg(unix)]
impl TestFileCredentialStore {
    fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn secret_path(&self, service: &str, account: &str) -> PathBuf {
        let mut digest = Sha256::new();
        digest.update(b"test-artifact-credential/v1");
        digest.update(service.as_bytes());
        digest.update([0]);
        digest.update(account.as_bytes());
        self.root
            .join(format!(".secret-{}", hex::encode(digest.finalize())))
    }
}

#[cfg(unix)]
impl ArtifactCredentialStore for TestFileCredentialStore {
    fn load_secret(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, SealedArtifactError> {
        let path = self.secret_path(service, account);
        let mut file = match open_private_file(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SealedArtifactError::KeyUnavailable(error.to_string())),
        };
        let length = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
        if length > 64 {
            return Err(SealedArtifactError::KeyUnavailable(
                "test credential file exceeds key material bound".to_string(),
            ));
        }
        let mut secret = Zeroizing::new(Vec::with_capacity(length));
        file.read_to_end(secret.as_mut())?;
        Ok(Some(secret))
    }

    fn store_secret(
        &self,
        service: &str,
        account: &str,
        secret: &[u8],
    ) -> Result<(), SealedArtifactError> {
        let path = self.secret_path(service, account);
        let mut file = create_private_file(&path).map_err(|error| {
            SealedArtifactError::KeyUnavailable(format!(
                "test credential could not be atomically created: {error}"
            ))
        })?;
        file.write_all(secret)?;
        file.sync_all()?;
        drop(file);
        sync_directory(&self.root)?;
        Ok(())
    }

    fn acquire_provision_lock(
        &self,
        _service: &str,
        _store_namespace: &str,
    ) -> Result<Box<dyn ArtifactCredentialProvisionGuard>, SealedArtifactError> {
        let path = self.root.join(".test-credential-provision.lock");
        let file = match create_private_file(&path) {
            Ok(file) => {
                file.sync_all()?;
                sync_directory(&self.root)?;
                file
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open_private_file(&path)?,
            Err(error) => return Err(SealedArtifactError::Io(error)),
        };
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(Box::new(TestFileCredentialProvisionGuard { _file: file }))
    }
}

struct TestRedactor;

impl ArtifactRedactor for TestRedactor {
    fn version(&self) -> &str {
        "test-redactor-v1"
    }

    fn redact(&self, raw: &Value) -> Result<RedactedArtifact, SealedArtifactError> {
        let task = raw
            .get("task")
            .cloned()
            .ok_or_else(|| SealedArtifactError::Redaction("missing task".to_string()))?;
        let private = raw
            .get("private")
            .cloned()
            .ok_or_else(|| SealedArtifactError::Redaction("missing private payload".to_string()))?;
        Ok(RedactedArtifact {
            public_identity: serde_json::json!({ "task": task }),
            credential_free_private: serde_json::json!({ "private": private }),
        })
    }
}

struct RejectingRedactor;

impl ArtifactRedactor for RejectingRedactor {
    fn version(&self) -> &str {
        "reject-v1"
    }

    fn redact(&self, _raw: &Value) -> Result<RedactedArtifact, SealedArtifactError> {
        Err(SealedArtifactError::Redaction(
            "unclassified credential".to_string(),
        ))
    }
}

struct Allow;

impl ExactArtifactAuthorization for Allow {
    fn authorize_exact_read(
        &self,
        _artifact: &SealedArtifactRef,
    ) -> Result<(), SealedArtifactError> {
        Ok(())
    }
}

struct Deny;

impl ExactArtifactAuthorization for Deny {
    fn authorize_exact_read(
        &self,
        _artifact: &SealedArtifactRef,
    ) -> Result<(), SealedArtifactError> {
        Err(SealedArtifactError::Unauthorized(
            "test policy denied".to_string(),
        ))
    }
}

fn fingerprint_key() -> FingerprintKey {
    FingerprintKey::new("fingerprint-test", 7, [0x11; 32]).unwrap()
}

fn keyring(byte: u8) -> StaticArtifactKeyring {
    StaticArtifactKeyring::new(SealingKey::new("seal-test-v1", [byte; 32]).unwrap(), []).unwrap()
}

#[cfg(unix)]
fn private_tempdir() -> io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir()?;
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
    Ok(root)
}

#[cfg(unix)]
fn artifact_entry_count(root: &Path, suffix: &str) -> usize {
    fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(suffix))
        })
        .count()
}

fn raw(private: &str) -> Value {
    serde_json::json!({
        "credential": "must-not-survive-redaction",
        "private": private,
        "task": { "b": 2, "a": 1 }
    })
}

fn exact_runtime_artifact(
    evidence_id: &str,
    evidence_subject: &str,
    artifact_kind: &str,
    outcome: &str,
    private: Value,
) -> Value {
    serde_json::json!({
        "identity": {
            "artifact_kind": artifact_kind,
            "evidence_id": evidence_id,
            "evidence_subject": evidence_subject,
            "outcome": outcome,
            "replay_fidelity": "credential_redacted",
            "schema": EXACT_RUNTIME_EVIDENCE_SCHEMA,
        },
        "private": private,
    })
}

#[test]
fn exact_llm_and_tool_transcripts_are_byte_identical_and_fail_closed() {
    let llm_private = serde_json::json!({
        "evidence_id": "llm-attempt-1",
        "evidence_subject": "llm-request",
        "payload": {
            "extra_body": null,
            "max_tokens": 64,
            "messages": [{"content": "SAFE-LLM-SENTINEL", "role": "user"}],
            "model": "test-model",
            "provider": "test-provider",
            "stream": false,
            "temperature": null,
            "tools": [],
        },
        "phase": "request",
        "schema": EXACT_RUNTIME_EVIDENCE_SCHEMA,
    });
    let tool_private = serde_json::json!({
        "evidence_id": "tool-call-1",
        "evidence_subject": "tool-invocation",
        "payload": {
            "arguments": {"query": "SAFE-TOOL-SENTINEL"},
            "call_id": "tool-call-1",
            "effect_intent": "none",
            "execution_trust": "observation",
            "tool_name": "lookup",
        },
        "phase": "request",
        "schema": EXACT_RUNTIME_EVIDENCE_SCHEMA,
    });

    for (schema, raw, private) in [
        (
            ExactRuntimeEvidenceSchema::LlmRequestV1,
            exact_runtime_artifact(
                "llm-attempt-1",
                "llm-request",
                "request",
                "not_applicable",
                llm_private.clone(),
            ),
            llm_private,
        ),
        (
            ExactRuntimeEvidenceSchema::ToolIntentV1,
            exact_runtime_artifact(
                "tool-call-1",
                "tool-invocation",
                "request",
                "not_applicable",
                tool_private.clone(),
            ),
            tool_private.clone(),
        ),
    ] {
        let prepared = prepare_exact_runtime_artifact_bound(
            schema,
            &raw,
            &private,
            &LedgerEvidenceRedactor,
            &fingerprint_key(),
        )
        .unwrap();
        assert_eq!(
            prepared.private_jcs.as_slice(),
            serde_json_canonicalizer::to_vec(&private).unwrap().as_slice()
        );
        let public_identity: Value =
            serde_json::from_slice(&prepared.identity().public_identity_jcs).unwrap();
        let binding =
            validate_closed_exact_runtime_private_jcs(&public_identity, &prepared.private_jcs)
                .unwrap();
        assert_eq!(binding.schema, schema);
    }

    let mut sensitive = tool_private.clone();
    sensitive["payload"]["arguments"]["authorization"] =
        Value::String("Bearer abcdefghijklmnopqrstuvwxyz123456".to_string());
    let sensitive_raw = exact_runtime_artifact(
        "tool-call-1",
        "tool-invocation",
        "request",
        "not_applicable",
        sensitive.clone(),
    );
    assert!(prepare_exact_runtime_artifact_bound(
        ExactRuntimeEvidenceSchema::ToolIntentV1,
        &sensitive_raw,
        &sensitive,
        &LedgerEvidenceRedactor,
        &fingerprint_key(),
    )
    .is_err());

    let raw = exact_runtime_artifact(
        "tool-call-1",
        "tool-invocation",
        "request",
        "not_applicable",
        tool_private.clone(),
    );
    let mut mismatched_private = tool_private.clone();
    mismatched_private["payload"]["arguments"]["query"] =
        Value::String("different".to_string());
    assert!(prepare_exact_runtime_artifact_bound(
        ExactRuntimeEvidenceSchema::ToolIntentV1,
        &raw,
        &mismatched_private,
        &LedgerEvidenceRedactor,
        &fingerprint_key(),
    )
    .is_err());

    let mut unknown = tool_private.clone();
    unknown["payload"]["unknown"] = Value::Bool(true);
    let unknown_raw = exact_runtime_artifact(
        "tool-call-1",
        "tool-invocation",
        "request",
        "not_applicable",
        unknown.clone(),
    );
    assert!(prepare_exact_runtime_artifact_bound(
        ExactRuntimeEvidenceSchema::ToolIntentV1,
        &unknown_raw,
        &unknown,
        &LedgerEvidenceRedactor,
        &fingerprint_key(),
    )
    .is_err());
}

#[test]
fn sealed_envelope_capacity_sizing_handles_base64_boundaries_without_buffers() {
    assert_eq!(checked_base64_encoded_len(0).unwrap(), 0);
    assert_eq!(checked_base64_encoded_len(1).unwrap(), 4);
    assert_eq!(checked_base64_encoded_len(2).unwrap(), 4);
    assert_eq!(checked_base64_encoded_len(3).unwrap(), 4);
    assert_eq!(checked_base64_encoded_len(4).unwrap(), 8);
    assert!(matches!(
        checked_base64_encoded_len(usize::MAX),
        Err(SealedArtifactError::InvalidMetadata(_))
    ));
}

#[test]
fn sealed_envelope_capacity_sizing_covers_maximum_private_payload_without_allocating_it() {
    let required = sealed_envelope_required_free_bytes(MAX_ARTIFACT_BYTES).unwrap();
    let ciphertext_len = MAX_ARTIFACT_BYTES
        .checked_add(PLAINTEXT_DIGEST_BYTES)
        .and_then(|value| value.checked_add(AES_GCM_TAG_BYTES))
        .unwrap();
    let base64_len = checked_base64_encoded_len(ciphertext_len).unwrap();
    let lower_bound = u64::try_from(base64_len)
        .unwrap()
        .checked_add(ARTIFACT_FILESYSTEM_SAFETY_MARGIN_BYTES)
        .unwrap();

    assert!(required >= lower_bound);
    assert!(required > 170 * 1024 * 1024);
    assert!(matches!(
        sealed_envelope_required_free_bytes(MAX_ARTIFACT_BYTES + 1),
        Err(SealedArtifactError::TooLarge {
            actual,
            limit: MAX_ARTIFACT_BYTES,
        }) if actual == MAX_ARTIFACT_BYTES + 1
    ));
}

#[cfg(unix)]
#[test]
fn os_key_manager_provisions_rotates_and_resolves_by_stable_store_namespace() {
    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let namespace = store.credential_namespace();
    drop(store);
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    assert_eq!(store.credential_namespace(), namespace);

    let credentials = Arc::new(MemoryCredentialStore::default());
    let first_config =
        OsArtifactKeyManagerConfig::new("hermes-test", "sealing-v1", "fingerprint-v1", 1, &store)
            .unwrap();
    let first = OsArtifactKeyManager::with_store(first_config, credentials.clone());
    assert!(matches!(
        first.active_key(),
        Err(SealedArtifactError::KeyUnavailable(_))
    ));
    first.provision_active_keys_for_store(&store).unwrap();
    let first_sealing = first.active_key().unwrap();
    let first_fingerprint = first.active_fingerprint_key().unwrap();

    let rotated_config =
        OsArtifactKeyManagerConfig::new("hermes-test", "sealing-v2", "fingerprint-v2", 2, &store)
            .unwrap();
    let rotated = OsArtifactKeyManager::with_store(rotated_config, credentials.clone());
    rotated.provision_active_keys_for_store(&store).unwrap();
    let active_sealing = rotated.active_key().unwrap();
    let active_fingerprint = rotated.active_fingerprint_key().unwrap();
    let historical = rotated.resolve_key("sealing-v1").unwrap();
    assert_eq!(*historical.material, *first_sealing.material);
    assert_ne!(*active_sealing.material, *first_sealing.material);
    assert_ne!(*active_fingerprint.material, *first_fingerprint.material);

    let other_root = private_tempdir().unwrap();
    let other_store = FilesystemSealedArtifactStore::open(other_root.path()).unwrap();
    assert_ne!(
        other_store.credential_namespace(),
        store.credential_namespace()
    );
    assert!(matches!(
        first.provision_active_keys_for_store(&other_store),
        Err(SealedArtifactError::InvalidMetadata(_))
    ));
    let other_config = OsArtifactKeyManagerConfig::new(
        "hermes-test",
        "sealing-v1",
        "fingerprint-v1",
        1,
        &other_store,
    )
    .unwrap();
    let other = OsArtifactKeyManager::with_store(other_config, credentials);
    other.provision_active_keys_for_store(&other_store).unwrap();
    assert_ne!(
        *other.active_key().unwrap().material,
        *first_sealing.material
    );
}

#[cfg(unix)]
#[test]
fn legacy_credentials_backfill_an_atomic_durable_provision_marker() {
    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let config =
        OsArtifactKeyManagerConfig::new("hermes-test", "sealing-v1", "fingerprint-v1", 1, &store)
            .unwrap();
    let keys = OsArtifactKeyManager::with_store(config, credentials);
    let marker = ArtifactKeyProvisionMarker::from_config(&keys.config);
    let marker_path = keys.provision_marker_path(&store, &marker).unwrap();

    keys.provision_active_keys().unwrap();
    let before_sealing = keys.active_key().unwrap();
    let before_fingerprint = keys.active_fingerprint_key().unwrap();
    assert!(!marker_path.exists());

    keys.provision_active_keys_for_store(&store).unwrap();
    assert!(marker_path.exists());
    assert!(keys
        .load_provision_marker(&store, &marker)
        .unwrap()
        .is_some());
    assert_eq!(
        *keys.active_key().unwrap().material,
        *before_sealing.material
    );
    assert_eq!(
        *keys.active_fingerprint_key().unwrap().material,
        *before_fingerprint.material
    );
}

#[cfg(unix)]
#[test]
fn durable_marker_refuses_deleted_or_malformed_active_credentials() {
    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let config =
        OsArtifactKeyManagerConfig::new("hermes-test", "sealing-v1", "fingerprint-v1", 1, &store)
            .unwrap();
    let keys = OsArtifactKeyManager::with_store(config, credentials.clone());
    keys.provision_active_keys_for_store(&store).unwrap();
    keys.resolve_active_keys_for_store(&store).unwrap();
    let sealing_account = keys.sealing_account("sealing-v1");
    credentials.remove_secret(&keys.config.service, &sealing_account);

    assert!(matches!(
        keys.provision_active_keys_for_store(&store),
        Err(SealedArtifactError::KeyUnavailable(_))
    ));
    assert!(matches!(
        keys.resolve_active_keys_for_store(&store),
        Err(SealedArtifactError::KeyUnavailable(_))
    ));
    assert!(matches!(
        keys.active_key(),
        Err(SealedArtifactError::KeyUnavailable(_))
    ));
    assert!(credentials
        .load_secret(&keys.config.service, &sealing_account)
        .unwrap()
        .is_none());

    let malformed_root = private_tempdir().unwrap();
    let malformed_store = FilesystemSealedArtifactStore::open(malformed_root.path()).unwrap();
    let malformed_config = OsArtifactKeyManagerConfig::new(
        "hermes-test",
        "sealing-v1",
        "fingerprint-v1",
        1,
        &malformed_store,
    )
    .unwrap();
    let malformed = OsArtifactKeyManager::with_store(malformed_config, credentials.clone());
    malformed
        .provision_active_keys_for_store(&malformed_store)
        .unwrap();
    let malformed_account = malformed.sealing_account("sealing-v1");
    credentials.replace_secret(
        &malformed.config.service,
        &malformed_account,
        vec![0x55; 31],
    );
    assert!(matches!(
        malformed.provision_active_keys_for_store(&malformed_store),
        Err(SealedArtifactError::KeyUnavailable(_))
    ));
    assert_eq!(
        credentials
            .load_secret(&malformed.config.service, &malformed_account)
            .unwrap()
            .unwrap()
            .len(),
        31
    );
}

#[cfg(unix)]
#[test]
fn key_marker_is_config_bound_and_corruption_fails_closed() {
    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    assert!(matches!(
        OsArtifactKeyManagerConfig::new("hermes-test", "same-key-id", "same-key-id", 1, &store,),
        Err(SealedArtifactError::InvalidMetadata(_))
    ));

    let credentials = Arc::new(MemoryCredentialStore::default());
    let config =
        OsArtifactKeyManagerConfig::new("hermes-test", "sealing-v1", "fingerprint-v1", 1, &store)
            .unwrap();
    let keys = OsArtifactKeyManager::with_store(config, credentials);
    keys.provision_active_keys_for_store(&store).unwrap();
    let marker = ArtifactKeyProvisionMarker::from_config(&keys.config);
    let path = keys.provision_marker_path(&store, &marker).unwrap();
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .unwrap();
    file.write_all(b"{}").unwrap();
    file.sync_all().unwrap();
    drop(file);

    assert!(matches!(
        keys.resolve_active_keys_for_store(&store),
        Err(SealedArtifactError::KeyUnavailable(_))
    ));
    assert!(matches!(
        keys.provision_active_keys_for_store(&store),
        Err(SealedArtifactError::KeyUnavailable(_))
    ));
}

#[cfg(unix)]
#[test]
fn incomplete_credentials_with_existing_cas_fail_closed_on_reopen() {
    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let config =
        OsArtifactKeyManagerConfig::new("hermes-test", "sealing-v1", "fingerprint-v1", 1, &store)
            .unwrap();
    let keys = OsArtifactKeyManager::with_store(config.clone(), credentials.clone());
    let sealing_account = keys.sealing_account("sealing-v1");
    keys.provision_secret(&sealing_account, "active sealing key")
        .unwrap();
    let bytes = b"existing sealed artifact";
    let digest = hex::encode(Sha256::digest(bytes));
    store.put_if_absent(&digest, bytes).unwrap();

    assert!(matches!(
        keys.provision_active_keys_for_store(&store),
        Err(SealedArtifactError::KeyUnavailable(_))
    ));

    drop(store);
    let reopened_store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let reopened = OsArtifactKeyManager::with_store(config, credentials.clone());
    assert!(matches!(
        reopened.provision_active_keys_for_store(&reopened_store),
        Err(SealedArtifactError::KeyUnavailable(_))
    ));
    assert!(credentials
        .load_secret(
            &reopened.config.service,
            &reopened.sealing_account("sealing-v1")
        )
        .unwrap()
        .is_some());
    assert!(credentials
        .load_secret(
            &reopened.config.service,
            &reopened.fingerprint_account("fingerprint-v1", 1),
        )
        .unwrap()
        .is_none());
}

#[cfg(unix)]
#[test]
fn concurrent_key_initialization_converges_on_one_marked_pair() {
    let root = private_tempdir().unwrap();
    let first_store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let second_store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let config = OsArtifactKeyManagerConfig::new(
        "hermes-test",
        "sealing-v1",
        "fingerprint-v1",
        1,
        &first_store,
    )
    .unwrap();
    let first = OsArtifactKeyManager::with_store(config.clone(), credentials.clone());
    let second = OsArtifactKeyManager::with_store(config, credentials.clone());
    let barrier = Arc::new(Barrier::new(2));
    let first_barrier = barrier.clone();
    let second_barrier = barrier;

    let first_init = std::thread::spawn(move || {
        first_barrier.wait();
        first.provision_active_keys_for_store(&first_store)
    });
    let second_init = std::thread::spawn(move || {
        second_barrier.wait();
        second.provision_active_keys_for_store(&second_store)
    });
    first_init.join().unwrap().unwrap();
    second_init.join().unwrap().unwrap();

    let reopened_store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let reopened = OsArtifactKeyManager::with_store(
        OsArtifactKeyManagerConfig::new(
            "hermes-test",
            "sealing-v1",
            "fingerprint-v1",
            1,
            &reopened_store,
        )
        .unwrap(),
        credentials,
    );
    reopened
        .provision_active_keys_for_store(&reopened_store)
        .unwrap();
    let marker = ArtifactKeyProvisionMarker::from_config(&reopened.config);
    assert!(reopened
        .load_provision_marker(&reopened_store, &marker)
        .unwrap()
        .is_some());
    assert_ne!(
        *reopened.active_key().unwrap().material,
        *reopened.active_fingerprint_key().unwrap().material
    );
}

#[cfg(unix)]
#[test]
fn concurrent_first_install_uses_real_child_processes() {
    const STORE_ENV: &str = "HERMES_SEALED_ARTIFACT_CHILD_STORE";
    const CREDENTIAL_ENV: &str = "HERMES_SEALED_ARTIFACT_CHILD_CREDENTIALS";
    const START_ENV: &str = "HERMES_SEALED_ARTIFACT_CHILD_START";
    const ROLE_ENV: &str = "HERMES_SEALED_ARTIFACT_CHILD_ROLE";

    if let (Ok(store_root), Ok(credential_root), Ok(start_path), Ok(role)) = (
        std::env::var(STORE_ENV),
        std::env::var(CREDENTIAL_ENV),
        std::env::var(START_ENV),
        std::env::var(ROLE_ENV),
    ) {
        let ready_path = PathBuf::from(credential_root.clone()).join(format!(".ready-{role}"));
        fs::write(&ready_path, b"ready").unwrap();
        let start_path = PathBuf::from(start_path);
        for _ in 0..1_000 {
            if start_path.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            start_path.exists(),
            "parent never released child start barrier"
        );

        let store = FilesystemSealedArtifactStore::open(store_root).unwrap();
        let credentials = Arc::new(TestFileCredentialStore::new(credential_root));
        let config = OsArtifactKeyManagerConfig::new(
            "hermes-test-child",
            "sealing-v1",
            "fingerprint-v1",
            1,
            &store,
        )
        .unwrap();
        let keys = OsArtifactKeyManager::with_store(config, credentials);
        keys.provision_active_keys_for_store(&store).unwrap();
        assert_ne!(
            *keys.active_key().unwrap().material,
            *keys.active_fingerprint_key().unwrap().material
        );
        return;
    }

    let store_root = private_tempdir().unwrap();
    let credential_root = private_tempdir().unwrap();
    let start_path = credential_root.path().join(".start");
    let executable = std::env::current_exe().unwrap();
    let test_name = "sealed_artifact::tests::concurrent_first_install_uses_real_child_processes";
    let child = |role: &str| {
        Command::new(&executable)
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env(STORE_ENV, store_root.path())
            .env(CREDENTIAL_ENV, credential_root.path())
            .env(START_ENV, &start_path)
            .env(ROLE_ENV, role)
            .spawn()
            .unwrap()
    };
    let mut first = child("one");
    let mut second = child("two");
    for _ in 0..1_000 {
        if credential_root.path().join(".ready-one").exists()
            && credential_root.path().join(".ready-two").exists()
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        credential_root.path().join(".ready-one").exists()
            && credential_root.path().join(".ready-two").exists(),
        "children did not reach the concurrent provisioning barrier"
    );
    fs::write(&start_path, b"go").unwrap();
    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());

    let store = FilesystemSealedArtifactStore::open(store_root.path()).unwrap();
    let credentials = Arc::new(TestFileCredentialStore::new(credential_root.path()));
    let config = OsArtifactKeyManagerConfig::new(
        "hermes-test-child",
        "sealing-v1",
        "fingerprint-v1",
        1,
        &store,
    )
    .unwrap();
    let keys = OsArtifactKeyManager::with_store(config, credentials);
    keys.provision_active_keys_for_store(&store).unwrap();
    let marker = ArtifactKeyProvisionMarker::from_config(&keys.config);
    assert!(keys
        .load_provision_marker(&store, &marker)
        .unwrap()
        .is_some());
    assert_ne!(
        *keys.active_key().unwrap().material,
        *keys.active_fingerprint_key().unwrap().material
    );
}

#[cfg(unix)]
#[test]
fn two_runtime_key_managers_share_sealing_but_mark_distinct_fingerprints() {
    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let runtime_config = OsArtifactKeyManagerConfig::new(
        "hermes-test",
        "sealing-v1",
        "runtime-fingerprint-v1",
        1,
        &store,
    )
    .unwrap();
    let session_config = OsArtifactKeyManagerConfig::new(
        "hermes-test",
        "sealing-v1",
        "session-fingerprint-v1",
        1,
        &store,
    )
    .unwrap();
    let runtime = OsArtifactKeyManager::with_store(runtime_config.clone(), credentials.clone());
    let session = OsArtifactKeyManager::with_store(session_config.clone(), credentials.clone());
    runtime.provision_active_keys_for_store(&store).unwrap();
    session.provision_active_keys_for_store(&store).unwrap();

    assert_eq!(
        *runtime.active_key().unwrap().material,
        *session.active_key().unwrap().material
    );
    assert_ne!(
        *runtime.active_fingerprint_key().unwrap().material,
        *session.active_fingerprint_key().unwrap().material
    );
    let runtime_marker = ArtifactKeyProvisionMarker::from_config(&runtime.config);
    let session_marker = ArtifactKeyProvisionMarker::from_config(&session.config);
    assert!(runtime
        .load_provision_marker(&store, &runtime_marker)
        .unwrap()
        .is_some());
    assert!(session
        .load_provision_marker(&store, &session_marker)
        .unwrap()
        .is_some());

    drop(store);
    let reopened_store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let reopened_runtime = OsArtifactKeyManager::with_store(runtime_config, credentials.clone());
    let reopened_session = OsArtifactKeyManager::with_store(session_config, credentials);
    reopened_runtime
        .resolve_active_keys_for_store(&reopened_store)
        .unwrap();
    reopened_session
        .resolve_active_keys_for_store(&reopened_store)
        .unwrap();
    assert_eq!(
        *reopened_runtime.active_key().unwrap().material,
        *reopened_session.active_key().unwrap().material
    );
    assert_ne!(
        *reopened_runtime.active_fingerprint_key().unwrap().material,
        *reopened_session.active_fingerprint_key().unwrap().material
    );
}

#[cfg(unix)]
#[test]
fn marked_lineage_permits_key_rotation_and_historical_exact_unseal() {
    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let initial_config =
        OsArtifactKeyManagerConfig::new("hermes-test", "sealing-v1", "fingerprint-v1", 1, &store)
            .unwrap();
    let initial = OsArtifactKeyManager::with_store(initial_config, credentials.clone());
    initial.provision_active_keys_for_store(&store).unwrap();
    let prepared = prepare_artifact(
        &raw("historical artifact"),
        &TestRedactor,
        &initial.active_fingerprint_key().unwrap(),
    )
    .unwrap();
    let reference = ArtifactSealer::new(&initial, &store)
        .seal(prepared)
        .unwrap()
        .commit_with(|_, reference| Ok::<_, ()>(reference))
        .unwrap();

    let rotated_config =
        OsArtifactKeyManagerConfig::new("hermes-test", "sealing-v2", "fingerprint-v2", 2, &store)
            .unwrap();
    let rotated = OsArtifactKeyManager::with_store(rotated_config.clone(), credentials.clone());
    rotated.provision_active_keys_for_store(&store).unwrap();
    assert_ne!(
        *rotated.active_key().unwrap().material,
        *initial.active_key().unwrap().material
    );
    assert_ne!(
        *rotated.active_fingerprint_key().unwrap().material,
        *initial.active_fingerprint_key().unwrap().material
    );
    assert_eq!(
        *rotated.resolve_key("sealing-v1").unwrap().material,
        *initial.resolve_key("sealing-v1").unwrap().material
    );

    drop(initial);
    drop(rotated);
    drop(store);
    let reopened_store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let reopened = OsArtifactKeyManager::with_store(rotated_config, credentials.clone());
    reopened
        .provision_active_keys_for_store(&reopened_store)
        .unwrap();
    let recovered = ArtifactSealer::new(&reopened, &reopened_store)
        .unseal_exact(&reference, &Allow)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&recovered).unwrap(),
        serde_json::json!({ "private": "historical artifact" })
    );

    credentials.remove_secret(
        &reopened.config.service,
        &reopened.sealing_account("sealing-v1"),
    );
    assert!(matches!(
        ArtifactSealer::new(&reopened, &reopened_store).unseal_exact(&reference, &Allow),
        Err(SealedArtifactError::KeyUnavailable(_))
    ));
}

#[cfg(unix)]
#[test]
fn publication_guard_holds_retention_until_reference_reload() {
    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let cleanup_store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let keys = keyring(0x52);
    let sealer = ArtifactSealer::new(&keys, &store);
    let prepared = prepare_artifact(&raw("guarded"), &TestRedactor, &fingerprint_key()).unwrap();
    let pending = sealer.seal(prepared).unwrap();
    let digest = pending.reference().envelope_digest.clone();
    let artifact_path = root.path().join(format!("{digest}.sealed"));

    let referenced_digest = digest.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (loaded_tx, loaded_rx) = mpsc::channel();
    let cleanup = std::thread::spawn(move || {
        let reopened = cleanup_store;
        started_tx.send(()).unwrap();
        let cutoff = SystemTime::now() + std::time::Duration::from_secs(60);
        reopened
            .cleanup_stale_with_reference_loader(cutoff, cutoff, || {
                loaded_tx.send(()).unwrap();
                Ok(HashSet::from([referenced_digest]))
            })
            .unwrap()
    });

    started_rx.recv().unwrap();
    assert!(matches!(
        loaded_rx.recv_timeout(std::time::Duration::from_millis(250)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let (_identity, committed_reference) = pending
        .commit_with(|identity, reference| Ok::<_, ()>((identity, reference)))
        .unwrap();
    assert_eq!(committed_reference.envelope_digest, digest);
    loaded_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("retention did not reload references after publication commit");
    let maintenance = cleanup.join().unwrap();
    assert_eq!(maintenance.stale_unreferenced_files_removed, 0);
    assert!(artifact_path.exists());
}

#[test]
fn redaction_is_fail_closed_and_identity_uses_rfc8785_jcs() {
    assert!(matches!(
        prepare_artifact(
            &raw(PRIVATE_SENTINEL),
            &RejectingRedactor,
            &fingerprint_key()
        ),
        Err(SealedArtifactError::Redaction(_))
    ));

    let prepared =
        prepare_artifact(&raw(PRIVATE_SENTINEL), &TestRedactor, &fingerprint_key()).unwrap();
    assert_eq!(
        prepared.identity().public_identity_jcs,
        br#"{"task":{"a":1,"b":2}}"#
    );
    assert!(
        !String::from_utf8_lossy(&prepared.identity().public_identity_jcs)
            .contains(PRIVATE_SENTINEL)
    );
    assert_eq!(prepared.identity().private_fingerprint.len(), 64);
    assert_eq!(
        prepared.identity().fingerprint_algorithm,
        ARTIFACT_FINGERPRINT_ALGORITHM
    );
    assert_eq!(
        prepared.identity().fingerprint_version,
        ARTIFACT_FINGERPRINT_VERSION
    );
    assert_eq!(prepared.identity().fingerprint_key_version, 7);
    assert_eq!(
        prepared.identity().canonicalization_algorithm,
        ARTIFACT_CANONICALIZATION
    );
    assert_eq!(
        prepared.identity().canonicalization_version,
        ARTIFACT_CANONICALIZATION_VERSION
    );
    assert_eq!(
        prepared.identity().private_fingerprint,
        "b714435c159eae5e428870f36bf70ba1b2961b00d0c91ee182f62c3260455654"
    );
    assert_ne!(
        prepared.identity().private_fingerprint,
        hex::encode(Sha256::digest(
            serde_json_canonicalizer::to_vec(&serde_json::json!({
                "private": PRIVATE_SENTINEL
            }))
            .unwrap()
        ))
    );
}

#[test]
fn append_identity_is_deterministic_but_private_changes_are_distinct() {
    let first = prepare_artifact(&raw("one"), &TestRedactor, &fingerprint_key()).unwrap();
    let again = prepare_artifact(&raw("one"), &TestRedactor, &fingerprint_key()).unwrap();
    let changed = prepare_artifact(&raw("two"), &TestRedactor, &fingerprint_key()).unwrap();
    assert_eq!(first.identity(), again.identity());
    assert_eq!(
        first.identity().public_identity_jcs,
        changed.identity().public_identity_jcs
    );
    assert_ne!(
        first.identity().private_fingerprint,
        changed.identity().private_fingerprint
    );
}

#[cfg(unix)]
#[test]
fn sealed_round_trip_requires_authorization_and_hides_plaintext() {
    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let keys = keyring(0x22);
    let sealer = ArtifactSealer::new(&keys, &store);
    let prepared =
        prepare_artifact(&raw(PRIVATE_SENTINEL), &TestRedactor, &fingerprint_key()).unwrap();
    let (_identity, reference) = sealer
        .seal(prepared)
        .unwrap()
        .commit_with(|identity, reference| Ok::<_, ()>((identity, reference)))
        .unwrap();
    assert_eq!(
        reference.canonicalization_algorithm,
        ARTIFACT_CANONICALIZATION
    );
    assert_eq!(
        reference.canonicalization_version,
        ARTIFACT_CANONICALIZATION_VERSION
    );

    let envelope = store.read(&reference.envelope_digest).unwrap();
    assert!(!String::from_utf8_lossy(&envelope).contains(PRIVATE_SENTINEL));
    assert!(matches!(
        sealer.unseal_exact(&reference, &Deny),
        Err(SealedArtifactError::Unauthorized(_))
    ));
    let private = sealer.unseal_exact(&reference, &Allow).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&private).unwrap(),
        serde_json::json!({ "private": PRIVATE_SENTINEL })
    );
}

#[test]
fn ledger_redactor_uses_closed_private_shapes_and_rejects_alpha_task_credentials() {
    let unknown_private = serde_json::json!({
        "identity": {
            "artifact_kind": "result",
            "evidence_id": "call-1",
            "evidence_subject": "llm-response",
            "outcome": "returned",
            "replay_fidelity": "credential_redacted",
            "schema": "hermes.runtime-evidence/v1",
        },
        "private": {
            "evidence_id": "call-1",
            "evidence_subject": "llm-response",
            "payload": {},
            "provider_token": "Bearer abcdefghijklmnopqrstuvwxyz123456",
        },
    });
    assert!(matches!(
        LedgerEvidenceRedactor.redact(&unknown_private),
        Err(SealedArtifactError::Redaction(_))
    ));

    let alpha_task = serde_json::json!({
        "identity": {
            "artifact_kind": "request",
            "evidence_id": "alpha-runtime-v1-task",
            "evidence_subject": "alpha-runtime-v1",
            "outcome": "not_applicable",
            "replay_fidelity": "credential_redacted",
            "schema": "hermes.runtime-evidence/v1",
        },
        "private": {
            "envelope": {
                "event_type": "dispatch",
                "loop_id": "primary",
                "schema": ALPHA_RUNTIME_V1_ENVELOPE_SCHEMA,
                "variant": "background_job",
            },
            "schema": ALPHA_RUNTIME_V1_PRIVATE_TASK_SCHEMA,
            "task": "implement token refresh and cookie parsing without logging an API key or password",
        },
    });
    let safe = LedgerEvidenceRedactor.redact(&alpha_task).unwrap();
    assert_eq!(
        safe.credential_free_private["task"],
        "implement token refresh and cookie parsing without logging an API key or password"
    );

    let mut environment_reference = alpha_task.clone();
    environment_reference["private"]["task"] = Value::String(
        "load api_key=environment_variable from deployment configuration".to_string(),
    );
    assert!(LedgerEvidenceRedactor
        .redact(&environment_reference)
        .is_ok());

    let mut prefixed_key = alpha_task.clone();
    prefixed_key["private"]["task"] =
        Value::String(format!("execute with {}", synthetic_openai_key()));
    assert!(matches!(
        LedgerEvidenceRedactor.redact(&prefixed_key),
        Err(SealedArtifactError::Redaction(_))
    ));

    let mut header_assignment = alpha_task.clone();
    header_assignment["private"]["task"] =
        Value::String("configure x-api-key=abcDEF0123456789uvwxyz".to_string());
    assert!(matches!(
        LedgerEvidenceRedactor.redact(&header_assignment),
        Err(SealedArtifactError::Redaction(_))
    ));

    let mut cookie_assignment = alpha_task.clone();
    cookie_assignment["private"]["task"] =
        Value::String("send Cookie: session=abcDEF0123456789uvwxyz".to_string());
    assert!(matches!(
        LedgerEvidenceRedactor.redact(&cookie_assignment),
        Err(SealedArtifactError::Redaction(_))
    ));

    let mut bare_bearer = alpha_task;
    bare_bearer["private"]["task"] =
        Value::String("execute with Bearer abcdefghijklmnopqrstuvwxyz123456".to_string());
    assert!(matches!(
        LedgerEvidenceRedactor.redact(&bare_bearer),
        Err(SealedArtifactError::Redaction(_))
    ));
}

#[test]
fn ledger_redactor_accepts_oauth_refresh_as_closed_bound_evidence() {
    for (artifact_kind, outcome, payload) in [
        (
            "request",
            "not_applicable",
            serde_json::json!({ "provider": "qwen-oauth" }),
        ),
        (
            "result",
            "returned",
            serde_json::json!({
                "provider": "qwen-oauth",
                "result": { "resolution": "refreshed" },
            }),
        ),
    ] {
        let raw = serde_json::json!({
            "identity": {
                "artifact_kind": artifact_kind,
                "evidence_id": "oauth-refresh-1111111111111111111111111111111111111111111111111111111111111111",
                "evidence_subject": "oauth-refresh",
                "outcome": outcome,
                "replay_fidelity": "credential_redacted",
                "schema": "hermes.runtime-evidence/v1",
            },
            "private": {
                "evidence_id": "oauth-refresh-1111111111111111111111111111111111111111111111111111111111111111",
                "evidence_subject": "oauth-refresh",
                "payload": payload,
            },
        });
        let redacted = LedgerEvidenceRedactor.redact(&raw).unwrap();
        assert_eq!(redacted.public_identity, raw["identity"]);
        assert_eq!(
            redacted.credential_free_private,
            serde_json::json!({
                "payload_kind": "object",
                "schema": CLOSED_RUNTIME_REPLAY_SCHEMA,
                "shape": "bound-runtime-evidence",
            })
        );
    }
}

#[test]
fn ledger_redactor_accepts_uuid_derived_agent_output_id() {
    let raw = serde_json::json!({
        "identity": {
            "artifact_kind": "result",
            "evidence_id": "run-output-11111111-1111-1111-1111-111111111111",
            "evidence_subject": "agent-output",
            "outcome": "returned",
            "replay_fidelity": "credential_redacted",
            "schema": "hermes.runtime-evidence/v1",
        },
        "private": {
            "response": "finished",
        },
    });
    let redacted = LedgerEvidenceRedactor.redact(&raw).unwrap();
    assert_eq!(redacted.public_identity, raw["identity"]);
    assert_eq!(
        redacted.credential_free_private,
        serde_json::json!({
            "response_byte_len": 8,
            "schema": CLOSED_RUNTIME_REPLAY_SCHEMA,
            "shape": "agent-output",
        })
    );
}

#[test]
fn delivery_redactor_accepts_schema_typed_digests_and_rejects_nested_secrets() {
    let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let mut evidence = serde_json::json!({
        "identity": {
            "artifact_kind": "result",
            "evidence_id": "delivery-result-1",
            "evidence_subject": "hermes.session-run.outbox/alpha-runtime-v1",
            "outcome": "returned",
            "replay_fidelity": "credential_redacted",
            "schema": "hermes.runtime-evidence/v1",
        },
        "private": {
            "attempt_count": 1,
            "completion": "acknowledged",
            "delivery_failure": null,
            "delivery_receipt": null,
            "event_id": "event-1",
            "idempotency_key": digest,
            "outbox_id": "outbox-1",
            "run_id": "run-1",
            "schema": ALPHA_RUNTIME_V1_DELIVERY_EVIDENCE_SCHEMA,
            "source_sealed_artifact_ref": {
                "canonicalization_algorithm": ARTIFACT_CANONICALIZATION,
                "canonicalization_version": ARTIFACT_CANONICALIZATION_VERSION,
                "encryption_algorithm": ARTIFACT_ENCRYPTION_ALGORITHM,
                "envelope_digest": digest,
                "key_id": "seal-v1",
                "redactor_version": "ledger-evidence-v5",
                "schema_version": SEALED_ARTIFACT_SCHEMA_VERSION,
            },
        },
    });
    let redacted = LedgerEvidenceRedactor.redact(&evidence).unwrap();
    assert_eq!(
        redacted.credential_free_private,
        serde_json::json!({
            "completion": "acknowledged",
            "schema": CLOSED_RUNTIME_REPLAY_SCHEMA,
            "shape": "delivery-result",
        })
    );

    evidence["private"]["source_sealed_artifact_ref"]["key_id"] =
        Value::String(synthetic_openai_key());
    assert!(matches!(
        LedgerEvidenceRedactor.redact(&evidence),
        Err(SealedArtifactError::Redaction(_))
    ));

    evidence["private"]["source_sealed_artifact_ref"]["key_id"] =
        Value::String("seal-v1".to_string());
    evidence["private"]["source_sealed_artifact_ref"]["envelope_digest"] =
        Value::String(synthetic_openai_key());
    assert!(matches!(
        LedgerEvidenceRedactor.redact(&evidence),
        Err(SealedArtifactError::InvalidMetadata(_))
    ));
}
#[cfg(unix)]
#[test]
fn bound_metadata_is_credential_free_and_source_distinct() {
    let safe = serde_json::json!({
        "identity": {
            "artifact_kind": "result",
            "outcome": "returned",
            "replay_fidelity": "credential_redacted",
            "schema": "hermes.runtime-evidence/v1",
            "evidence_id": "call-1",
            "evidence_subject": "llm-response",
        },
        "private": {
            "evidence_id": "call-1",
            "evidence_subject": "llm-response",
            "payload": {
                "authorization": "opaque-auth",
                "cookie": "opaque-cookie",
                "providerSecret": "opaque-provider",
                "response": "opaqueValue123",
                "accessToken": "short-token",
                "opaque-secret-as-key": true,
                "numeric_secret_bytes": [987654321, 123456789],
            },
        },
    });
    let evidence = serde_json::json!({
        "payload": {
            "authorization": "opaque-auth",
            "cookie": "opaque-cookie",
            "providerSecret": "opaque-provider",
            "response": "opaqueValue123",
            "accessToken": "short-token",
            "opaque-secret-as-key": true,
            "numeric_secret_bytes": [987654321, 123456789],
        },
        "tool_call_id": "call-1",
        "tool_name": "demo",
    });
    let changed = serde_json::json!({
        "payload": {
            "authorization": "opaque-auth",
            "cookie": "opaque-cookie",
            "providerSecret": "opaque-provider",
            "response": "differentOpaqueValue",
            "accessToken": "short-token",
        },
        "tool_call_id": "call-1",
        "tool_name": "demo",
    });
    let prepared = prepare_artifact_bound(
        &safe,
        &evidence,
        &LedgerEvidenceRedactor,
        &fingerprint_key(),
    )
    .unwrap();
    let changed_prepared =
        prepare_artifact_bound(&safe, &changed, &LedgerEvidenceRedactor, &fingerprint_key())
            .unwrap();
    assert_ne!(
        prepared.identity().private_fingerprint,
        changed_prepared.identity().private_fingerprint
    );

    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let keys = keyring(0x2a);
    let sealer = ArtifactSealer::new(&keys, &store);
    let reference = sealer
        .seal(prepared)
        .unwrap()
        .commit_with(|_, reference| Ok::<_, ()>(reference))
        .unwrap();
    let decrypted = sealer.unseal_exact(&reference, &Allow).unwrap();
    let decrypted: Value = serde_json::from_slice(&decrypted).unwrap();
    assert_eq!(decrypted["replay_fidelity"], "metadata_only");
    assert_eq!(
        decrypted["payload"],
        serde_json::json!({
            "payload_kind": "object",
            "schema": CLOSED_RUNTIME_REPLAY_SCHEMA,
            "shape": "bound-runtime-evidence",
        })
    );
    assert_eq!(
        decrypted["source_commitment_algorithm"],
        ARTIFACT_FINGERPRINT_ALGORITHM
    );
    let decrypted_text = serde_json::to_string(&decrypted).unwrap();
    for sentinel in [
        "opaque-auth",
        "opaque-cookie",
        "opaque-provider",
        "opaqueValue123",
        "short-token",
        "opaque-secret-as-key",
        "numeric_secret_bytes",
        "987654321",
    ] {
        assert!(
            !decrypted_text.contains(sentinel),
            "decrypted artifact retained {sentinel}"
        );
    }
}

#[cfg(unix)]
#[test]
fn wrong_key_tamper_and_reference_mismatch_fail_closed() {
    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let correct_keys = keyring(0x33);
    let sealer = ArtifactSealer::new(&correct_keys, &store);
    let prepared = prepare_artifact(&raw("secret"), &TestRedactor, &fingerprint_key()).unwrap();
    let (_identity, reference) = sealer
        .seal(prepared)
        .unwrap()
        .commit_with(|identity, reference| Ok::<_, ()>((identity, reference)))
        .unwrap();

    let wrong_keys = keyring(0x44);
    let wrong_sealer = ArtifactSealer::new(&wrong_keys, &store);
    assert!(matches!(
        wrong_sealer.unseal_exact(&reference, &Allow),
        Err(SealedArtifactError::InvalidEnvelope(_))
    ));

    let mut mismatched = reference.clone();
    mismatched.redactor_version = "other-v1".to_string();
    assert!(matches!(
        sealer.unseal_exact(&mismatched, &Allow),
        Err(SealedArtifactError::InvalidEnvelope(_))
    ));

    let path = root
        .path()
        .join(format!("{}.sealed", reference.envelope_digest));
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.last_mut().unwrap();
    *last ^= 1;
    fs::write(&path, bytes).unwrap();
    assert!(matches!(
        sealer.unseal_exact(&reference, &Allow),
        Err(SealedArtifactError::CasConflict(_))
    ));
}

#[cfg(unix)]
#[test]
fn private_store_rejects_permissions_links_and_digest_conflicts() {
    use std::os::unix::fs::PermissionsExt;

    let root = private_tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        FilesystemSealedArtifactStore::open(root.path()),
        Err(SealedArtifactError::Io(_))
    ));
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let bytes = b"sealed";
    let digest = hex::encode(Sha256::digest(bytes));
    store.put_if_absent(&digest, bytes).unwrap();
    let path = root.path().join(format!("{digest}.sealed"));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let alias = root.path().join("alias");
    fs::hard_link(&path, &alias).unwrap();
    assert!(matches!(
        store.read(&digest),
        Err(SealedArtifactError::Io(_))
    ));
    fs::remove_file(alias).unwrap();
    fs::write(&path, b"different").unwrap();
    assert!(matches!(
        store.put_if_absent(&digest, bytes),
        Err(SealedArtifactError::CasConflict(_))
    ));
}

#[cfg(unix)]
#[test]
fn concurrent_identical_cas_writes_converge_without_temp_files() {
    let root = private_tempdir().unwrap();
    let store = Arc::new(FilesystemSealedArtifactStore::open(root.path()).unwrap());
    let bytes = Arc::new(b"same encrypted envelope".to_vec());
    let digest = hex::encode(Sha256::digest(bytes.as_slice()));
    let barrier = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let store = Arc::clone(&store);
        let bytes = Arc::clone(&bytes);
        let digest = digest.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            store.put_if_absent(&digest, &bytes)
        }));
    }
    for worker in workers {
        worker.join().unwrap().unwrap();
    }
    assert_eq!(store.read(&digest).unwrap(), bytes.as_slice());
    assert_eq!(artifact_entry_count(root.path(), ".sealed"), 1);
    assert_eq!(artifact_entry_count(root.path(), ".tmp"), 0);
}

#[cfg(unix)]
#[test]
fn crash_boundaries_recover_verified_temp_and_renamed_artifacts() {
    let bytes = b"crash-safe encrypted envelope";
    let digest = hex::encode(Sha256::digest(bytes));

    for crash_at in [
        CasCrashPoint::AfterTempFsync,
        CasCrashPoint::AfterDigestVerification,
    ] {
        let root = private_tempdir().unwrap();
        let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
        assert!(matches!(
            store.put_with_injected_crash(&digest, bytes, crash_at),
            Err(SealedArtifactError::InjectedFailure(_))
        ));
        assert_eq!(artifact_entry_count(root.path(), ".tmp"), 1);
        drop(store);

        let recovered = FilesystemSealedArtifactStore::open(root.path()).unwrap();
        assert_eq!(artifact_entry_count(root.path(), ".tmp"), 0);
        recovered.put_if_absent(&digest, bytes).unwrap();
        assert_eq!(recovered.read(&digest).unwrap(), bytes);
    }

    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    assert!(matches!(
        store.put_with_injected_crash(&digest, bytes, CasCrashPoint::AfterAtomicRename),
        Err(SealedArtifactError::InjectedFailure(_))
    ));
    drop(store);
    let recovered = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    recovered.put_if_absent(&digest, bytes).unwrap();
    assert_eq!(recovered.read(&digest).unwrap(), bytes);
    assert_eq!(artifact_entry_count(root.path(), ".sealed"), 1);
    assert_eq!(artifact_entry_count(root.path(), ".tmp"), 0);
}

#[cfg(unix)]
#[test]
fn temp_digest_is_recomputed_before_atomic_publication() {
    let root = private_tempdir().unwrap();
    let store = FilesystemSealedArtifactStore::open(root.path()).unwrap();
    let bytes = b"verified before rename";
    let digest = hex::encode(Sha256::digest(bytes));
    assert!(matches!(
        store.corrupt_temporary_before_verification(&digest, bytes),
        Err(SealedArtifactError::CasConflict(_))
    ));
    assert_eq!(artifact_entry_count(root.path(), ".sealed"), 0);
    assert_eq!(artifact_entry_count(root.path(), ".tmp"), 0);
}

#[cfg(unix)]
#[test]
#[ignore = "executed as a child process by store_recovery_waits_for_live_writer_process"]
fn artifact_store_lock_child() {
    let Ok(root) = std::env::var("HERMES_TEST_ARTIFACT_LOCK_ROOT") else {
        return;
    };
    let ready =
        PathBuf::from(std::env::var("HERMES_TEST_ARTIFACT_LOCK_READY").expect("child ready path"));
    let release = PathBuf::from(
        std::env::var("HERMES_TEST_ARTIFACT_LOCK_RELEASE").expect("child release path"),
    );
    let store = FilesystemSealedArtifactStore::open(&root).expect("child opens store");
    let _writer_lock = store
        .shared_operation_lock()
        .expect("child acquires writer lock");
    let temporary = store.temporary_path();
    let mut cleanup = TemporaryArtifact::new(temporary.clone());
    let mut file = create_private_file(&temporary).expect("child creates temp");
    file.write_all(b"live-child-temp")
        .expect("child writes temp");
    file.sync_all().expect("child fsyncs temp");
    cleanup.disarm();
    fs::write(&ready, b"ready").expect("child signals ready");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !release.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "parent never released child writer"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn store_recovery_waits_for_live_writer_process() {
    let root = private_tempdir().unwrap();
    let coordination = tempfile::tempdir().unwrap();
    let ready = coordination.path().join("ready");
    let release = coordination.path().join("release");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "sealed_artifact::tests::artifact_store_lock_child",
            "--nocapture",
        ])
        .env("HERMES_TEST_ARTIFACT_LOCK_ROOT", root.path())
        .env("HERMES_TEST_ARTIFACT_LOCK_READY", &ready)
        .env("HERMES_TEST_ARTIFACT_LOCK_RELEASE", &release)
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "child never acquired the live writer lock"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(artifact_entry_count(root.path(), ".tmp"), 1);

    let root_path = root.path().to_path_buf();
    let (opened_tx, opened_rx) = mpsc::channel();
    let opener = std::thread::spawn(move || {
        opened_tx
            .send(FilesystemSealedArtifactStore::open(root_path))
            .unwrap();
    });
    assert!(matches!(
        opened_rx.recv_timeout(std::time::Duration::from_millis(250)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    fs::write(&release, b"release").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "child failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let recovered = opened_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("recovery did not resume after child exit")
        .unwrap();
    opener.join().unwrap();
    assert_eq!(artifact_entry_count(root.path(), ".tmp"), 0);
    drop(recovered);
}

#[test]
fn key_metadata_and_digest_paths_reject_ambiguous_values() {
    assert!(FingerprintKey::new("", 1, [0; 32]).is_err());
    assert!(FingerprintKey::new("valid", 0, [0; 32]).is_err());
    assert!(SealingKey::new("../escape", [0; 32]).is_err());
    assert!(validate_digest(&"A".repeat(64)).is_err());
    assert!(validate_digest("abcd").is_err());
}
