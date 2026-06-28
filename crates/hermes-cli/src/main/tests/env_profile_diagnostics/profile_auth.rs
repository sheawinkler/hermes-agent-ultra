#[test]
fn read_env_key_treats_empty_values_as_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env_file = tmp.path().join(".env");
    std::fs::write(
        &env_file,
        "OPENROUTER_API_KEY=\nMINIMAX_API_KEY='   '\nOPENAI_API_KEY=real-key\n",
    )
    .expect("write env");

    assert_eq!(read_env_key(&env_file, "OPENROUTER_API_KEY"), None);
    assert_eq!(read_env_key(&env_file, "MINIMAX_API_KEY"), None);
    assert_eq!(
        read_env_key(&env_file, "OPENAI_API_KEY").as_deref(),
        Some("real-key")
    );
}

#[test]
fn merge_missing_env_keys_skips_empty_values() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("legacy.env");
    let dst = tmp.path().join("target.env");
    std::fs::write(
        &src,
        "OPENROUTER_API_KEY=\nMINIMAX_API_KEY='  '\nOPENAI_API_KEY=real-key\n",
    )
    .expect("write source env");

    let imported = merge_missing_env_keys(&src, &dst, "legacy.env").expect("merge env keys");
    assert_eq!(imported, 1);
    let contents = std::fs::read_to_string(&dst).expect("read merged env");
    assert!(contents.contains("OPENAI_API_KEY=real-key"));
    assert!(!contents.contains("OPENROUTER_API_KEY="));
    assert!(!contents.contains("MINIMAX_API_KEY="));
}

#[test]
fn read_env_key_handles_non_utf8_bytes_without_crashing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env_file = tmp.path().join(".env");
    let mut bytes = b"OPENAI_API_KEY=real-key\nBROKEN=".to_vec();
    bytes.extend_from_slice(&[0xFF, 0xFE, 0x81, b'\n']);
    std::fs::write(&env_file, bytes).expect("write non-utf8 env");

    assert_eq!(
        read_env_key(&env_file, "OPENAI_API_KEY").as_deref(),
        Some("real-key")
    );
}

#[test]
fn provenance_sign_and_verify_round_trip() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("create cfg dir");
    let cli = Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        config_dir.to_str().expect("cfg path utf8"),
    ]);

    let artifact = tmp.path().join("doctor-snapshot.json");
    let body = b"{\"ok\":true}";
    std::fs::write(&artifact, body).expect("write artifact");

    let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
    let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");
    let verified =
        verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
    assert!(verified.ok, "verification should pass");
    assert_eq!(verified.code, "ok");
    assert!(verified.reason.is_none(), "no reason on success");
}

#[test]
fn provenance_verify_detects_tampered_artifact() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("create cfg dir");
    let cli = Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        config_dir.to_str().expect("cfg path utf8"),
    ]);

    let artifact = tmp.path().join("doctor-snapshot.json");
    let body = b"{\"ok\":true}";
    std::fs::write(&artifact, body).expect("write artifact");
    let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
    let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");

    std::fs::write(&artifact, b"{\"ok\":false}").expect("tamper artifact");

    let verified =
        verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
    assert!(!verified.ok, "tamper must fail");
    assert_eq!(verified.code, "artifact_sha256_mismatch");
    assert_eq!(verified.reason.as_deref(), Some("artifact_sha256 mismatch"));
}

#[test]
fn provenance_verify_detects_signature_mismatch() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("create cfg dir");
    let cli = Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        config_dir.to_str().expect("cfg path utf8"),
    ]);

    let artifact = tmp.path().join("doctor-snapshot.json");
    let body = b"{\"ok\":true}";
    std::fs::write(&artifact, body).expect("write artifact");
    let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
    let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");

    let mut parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).expect("read sidecar"))
            .expect("parse sidecar");
    parsed["signature_hex"] = serde_json::json!("deadbeef");
    std::fs::write(
        &sidecar,
        serde_json::to_string_pretty(&parsed).expect("serialize sidecar"),
    )
    .expect("write tampered sidecar");

    let verified =
        verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
    assert!(!verified.ok, "signature mismatch must fail");
    assert_eq!(verified.code, "signature_mismatch");
    assert_eq!(verified.reason.as_deref(), Some("signature mismatch"));
}

#[test]
fn provenance_verify_detects_missing_sidecar_with_code() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("create cfg dir");
    let cli = Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        config_dir.to_str().expect("cfg path utf8"),
    ]);

    let artifact = tmp.path().join("doctor-snapshot.json");
    std::fs::write(&artifact, b"{\"ok\":true}").expect("write artifact");

    let verified = verify_artifact_provenance(&cli, &artifact, None).expect("verify");
    assert!(!verified.ok, "missing sidecar must fail");
    assert_eq!(verified.code, "signature_read_error");
    assert!(verified
        .reason
        .as_deref()
        .unwrap_or("")
        .contains(".sig.json"));
}

#[tokio::test]
async fn rotate_provenance_key_archives_previous_key_and_rekeys() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("create cfg dir");
    let cli = Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        config_dir.to_str().expect("cfg path utf8"),
    ]);

    let old_key = load_or_create_provenance_key(&cli, true).expect("create key");
    run_rotate_provenance_key(cli.clone(), true)
        .await
        .expect("rotate key");
    let new_key = load_or_create_provenance_key(&cli, false).expect("load rotated key");
    assert_ne!(old_key, new_key, "rotation must change active key bytes");

    let auth_dir = provenance_key_path_for_cli(&cli)
        .parent()
        .expect("key path parent")
        .to_path_buf();
    let archived_count = std::fs::read_dir(auth_dir)
        .expect("read auth dir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("provenance.key.")
                && entry.file_name().to_string_lossy().ends_with(".bak")
        })
        .count();
    assert!(archived_count >= 1, "rotation should archive previous key");
}

#[test]
fn upsert_env_key_rewrites_existing_and_appends_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env_file = tmp.path().join(".env");
    std::fs::write(
        &env_file,
        "OPENAI_API_KEY=old\nHERMES_AUTH_DEFAULT_PROVIDER=openai\n",
    )
    .expect("write env");
    upsert_env_key(&env_file, "HERMES_AUTH_DEFAULT_PROVIDER", "nous").expect("upsert");
    upsert_env_key(&env_file, "NOUS_API_KEY", "tok").expect("append");
    let raw = std::fs::read_to_string(&env_file).expect("read env");
    assert!(raw.contains("HERMES_AUTH_DEFAULT_PROVIDER=nous"));
    assert!(raw.contains("NOUS_API_KEY=tok"));
    assert!(!raw.contains("HERMES_AUTH_DEFAULT_PROVIDER=openai"));
}

#[tokio::test]
async fn profile_create_no_skills_strips_cloned_skill_overrides() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");

    let source_profile = profiles_dir.join("source.yaml");
    std::fs::write(
        &source_profile,
        r#"
name: source
model: openai:gpt-4o
personality: technical
max_turns: 50
skills:
  enabled:
  - contextlattice-agent-contract
  disabled:
  - noisy-skill
"#,
    )
    .expect("write source profile");
    write_active_profile_name(&profiles_dir, "source").expect("set active profile");

    run_profile(
        cli,
        Some("create".to_string()),
        Some("target".to_string()),
        None,
        None,
        None,
        None,
        false,
        false,
        true,
        true,
        Some("source".to_string()),
        true,
        true,
    )
    .await
    .expect("create profile");

    let target_profile = profiles_dir.join("target.yaml");
    let parsed: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(&target_profile).expect("read target profile"),
    )
    .expect("parse target profile");
    let map = parsed.as_mapping().expect("mapping profile");
    let skills_key = serde_yaml::Value::String("skills".to_string());
    assert!(
        !map.contains_key(&skills_key),
        "skills key should be stripped"
    );
}

#[tokio::test]
async fn profile_create_clone_from_implies_config_clone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");

    let source_profile = profiles_dir.join("coder.yaml");
    std::fs::write(
        &source_profile,
        r#"
name: coder
model: anthropic/claude-sonnet-4
personality: focused
max_turns: 77
"#,
    )
    .expect("write source profile");

    run_profile(
        cli,
        Some("create".to_string()),
        Some("target".to_string()),
        None,
        None,
        None,
        None,
        false,
        false,
        false,
        false,
        Some("coder".to_string()),
        true,
        false,
    )
    .await
    .expect("create profile");

    let target_profile = profiles_dir.join("target.yaml");
    let parsed: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(&target_profile).expect("read target profile"),
    )
    .expect("parse target profile");
    let map = parsed.as_mapping().expect("mapping profile");
    assert_eq!(
        map.get(serde_yaml::Value::String("model".to_string()))
            .and_then(|v| v.as_str()),
        Some("anthropic/claude-sonnet-4")
    );
    assert_eq!(
        map.get(serde_yaml::Value::String("personality".to_string()))
            .and_then(|v| v.as_str()),
        Some("focused")
    );
    assert_eq!(
        map.get(serde_yaml::Value::String("max_turns".to_string()))
            .and_then(|v| v.as_i64()),
        Some(77)
    );
}

#[test]
fn validate_profile_name_rejects_paths() {
    let err = validate_profile_name("../danger").expect_err("should reject traversal");
    assert!(
        err.to_string().contains("path separators"),
        "unexpected error: {err}"
    );
    let err = validate_profile_name("alpha beta").expect_err("should reject spaces");
    assert!(
        err.to_string().contains("letters, numbers"),
        "unexpected error: {err}"
    );
    assert_eq!(
        validate_profile_name("prod-profile_1.2").expect("valid"),
        "prod-profile_1.2"
    );
}

#[test]
fn profile_alias_label_prefers_custom_aliases() {
    let mut aliases = std::collections::BTreeMap::new();
    aliases.insert("steve".to_string(), "steve".to_string());
    aliases.insert("qiaobusi".to_string(), "steve".to_string());
    aliases.insert("jobs".to_string(), "steve".to_string());
    aliases.insert("other".to_string(), "research".to_string());

    assert_eq!(
        profile_alias_label(&aliases, "steve").as_deref(),
        Some("aliases: jobs, qiaobusi")
    );
    assert_eq!(
        profile_alias_label(&aliases, "research").as_deref(),
        Some("alias: other")
    );
    assert_eq!(profile_alias_label(&aliases, "missing"), None);
}

#[tokio::test]
async fn profile_import_refuses_directory_clobber_target() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");

    let source_profile = tmp.path().join("source.yaml");
    std::fs::write(
        &source_profile,
        r#"
name: source
model: openai:gpt-4o
personality: default
max_turns: 50
"#,
    )
    .expect("write source profile");

    let clobber_target_dir = profiles_dir.join("target.yaml");
    std::fs::create_dir_all(&clobber_target_dir).expect("create clobber directory");

    let err = run_profile(
        cli,
        Some("import".to_string()),
        Some(source_profile.to_string_lossy().into_owned()),
        None,
        None,
        Some("target".to_string()),
        None,
        false,
        true,
        false,
        false,
        None,
        true,
        false,
    )
    .await
    .expect_err("directory clobber should be rejected");

    assert!(
        err.to_string().contains("target path is a directory"),
        "unexpected error: {err}"
    );
}

#[test]
fn qqbot_connect_url_encodes_task_id() {
    let url = qqbot_connect_url("task id/+");
    assert!(url.contains("task_id=task%20id%2F%2B"));
    assert!(url.contains("source=hermes"));
}

#[test]
fn qqbot_decrypt_secret_roundtrip() {
    let key = [7u8; 32];
    let nonce = [3u8; 12];
    let key_b64 = BASE64_STANDARD.encode(key);

    let cipher =
        <Aes256Gcm as aes_gcm::aead::KeyInit>::new_from_slice(&key).expect("cipher init");
    let ciphertext = cipher
        .encrypt(aes_gcm::Nonce::from_slice(&nonce), b"qq-secret".as_ref())
        .expect("encrypt");
    let mut payload = nonce.to_vec();
    payload.extend_from_slice(&ciphertext);
    let encrypted_b64 = BASE64_STANDARD.encode(payload);

    let decrypted = qqbot_decrypt_secret(&encrypted_b64, &key_b64).expect("decrypt");
    assert_eq!(decrypted, "qq-secret");
}
