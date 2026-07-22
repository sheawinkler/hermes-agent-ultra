{
            let skill_spec = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills install <name>".into(),
                )
            })?;
            let local_attestation = skill_extra_value(extra.as_deref(), "--attest-local")?;
            let local_attestation_key_file = skill_extra_value(extra.as_deref(), "--attest-key-file")?;
            let local_attestation_key_id = skill_extra_value(extra.as_deref(), "--attest-key-id")?;
            let bootstrap = bootstrap_requested(extra.as_deref());
            ensure_private_skill_root(&skills_dir)?;

            if let Some((local_root, declared_revision)) = parse_local_skill_source(&skill_spec)? {
                let owner_id = local_attestation.ok_or_else(|| {
                    AgentError::Config(
                        "Local skill installs require explicit --extra '--attest-local <owner-id>'"
                            .to_string(),
                    )
                })?;
                let key_file = local_attestation_key_file.ok_or_else(|| {
                    AgentError::Config(
                        "Local skill installs require --extra '--attest-key-file <absolute-owner-key-file>'"
                            .to_string(),
                    )
                })?;
                let key_id = local_attestation_key_id.ok_or_else(|| {
                    AgentError::Config(
                        "Local skill installs require --extra '--attest-key-id <configured-key-id>'"
                            .to_string(),
                    )
                })?;
                if skill_extra_value(extra.as_deref(), "--manifest")?.is_some() {
                    return Err(AgentError::Config(
                        "Local authored skill installs must use a local owner attestation, not a remote manifest"
                            .to_string(),
                    ));
                }
                let authority =
                    cli_skill_trust_authority_files(&skills_dir, authority_dir.as_deref())?;
                let files = collect_local_skill_files(&local_root)?;
                let install_name = sanitize_skill_install_name(
                    local_root
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("skill"),
                );
                let manifest = install_local_authored_skill(
                    &skills_dir,
                    &install_name,
                    &declared_revision,
                    &owner_id,
                    files,
                    &PathBuf::from(key_file),
                    &authority.local_owner_keyring_file,
                    &key_id,
                    bootstrap,
                )?;
                println!(
                    "Installed local-attested skill '{}' (revision={}, verdict={}, effects_allowed={}).",
                    install_name,
                    manifest.identity.immutable_source_revision,
                    manifest.scan.verdict.as_str(),
                    false
                );
                return Ok(());
            }

            if local_attestation.is_some()
                || local_attestation_key_file.is_some()
                || local_attestation_key_id.is_some()
            {
                return Err(AgentError::Config(
                    "--attest-local, --attest-key-file, and --attest-key-id are only valid for explicit file:// local sources; remote sources remain Remote"
                        .to_string(),
                ));
            }
            let authority =
                load_cli_skill_trust_authority_paths(&skills_dir, authority_dir.as_deref())?;
            authority
                .build_verifier(
                    hermes_skills::SkillTrustPolicy::default(),
                    std::collections::BTreeMap::new(),
                )
                .map_err(|error| trust_error("Invalid externally anchored skill trust", error))?;

            let manifest = load_remote_skill_manifest(extra.as_deref())?;
            let (skill_name, requested_version) = parse_skill_name_and_version(&skill_spec);
            if skill_name.is_empty() {
                return Err(AgentError::Config("Skill name cannot be empty".to_string()));
            }
            println!("Resolving signed remote skill: {}", skill_name);

            let client = reqwest::Client::new();
            let registry_prefixed = parse_registry_prefixed_skill(&skill_name);
            let explicit = if registry_prefixed.is_some() {
                None
            } else {
                parse_explicit_github_skill(&skill_name)
            };

            let (files, origin, immutable_revision, source_label) =
                if let Some((source, key)) = registry_prefixed {
                    match source.as_str() {
                        "official" => {
                            let resolved = resolve_official_skill_source(&client, &key).await?;
                            let origin = format!("github://{}/{}", resolved.repo, resolved.skill_dir);
                            let revision = resolved.immutable_revision.clone();
                            (
                                fetch_skill_files_from_github(&client, &resolved).await?,
                                origin,
                                revision,
                                "official".to_string(),
                            )
                        }
                        "skills.sh" => {
                            let resolved = resolve_skills_sh_source(&client, &key).await?;
                            let origin = format!("github://{}/{}", resolved.repo, resolved.skill_dir);
                            let revision = resolved.immutable_revision.clone();
                            (
                                fetch_skill_files_from_github(&client, &resolved).await?,
                                origin,
                                revision,
                                "skills.sh".to_string(),
                            )
                        }
                        "claude-marketplace" => {
                            let resolved = resolve_claude_marketplace_skill(&client, &key).await?;
                            let origin = format!("github://{}/{}", resolved.repo, resolved.skill_dir);
                            let revision = resolved.immutable_revision.clone();
                            (
                                fetch_skill_files_from_github(&client, &resolved).await?,
                                origin,
                                revision,
                                "claude-marketplace".to_string(),
                            )
                        }
                        "github" => {
                            let (repo, requested_ref, skill_dir) = parse_explicit_github_skill(&key)
                                .ok_or_else(|| {
                                    AgentError::Config(format!(
                                        "github/ installs require owner/repo/path, got '{}'",
                                        key
                                    ))
                                })?;
                            let resolved = resolve_github_skill_source(
                                &client,
                                &repo,
                                requested_ref.as_deref(),
                                &skill_dir,
                            )
                            .await?;
                            let origin = format!("github://{}/{}", resolved.repo, resolved.skill_dir);
                            let revision = resolved.immutable_revision.clone();
                            (
                                fetch_skill_files_from_github(&client, &resolved).await?,
                                origin,
                                revision,
                                "github".to_string(),
                            )
                        }
                        "lobehub" => {
                            let revision = manifest.identity.immutable_source_revision.clone();
                            if revision != manifest.identity.content_digest
                                || !revision.starts_with("sha256:")
                            {
                                return Err(AgentError::Config(
                                    "lobehub sources require a signed content-digest immutable revision"
                                        .to_string(),
                                ));
                            }
                            (
                                fetch_lobehub_skill_files(&client, &key).await?,
                                format!("lobehub://{key}"),
                                revision,
                                "lobehub".to_string(),
                            )
                        }
                        "clawhub" => {
                            let revision = manifest.identity.immutable_source_revision.clone();
                            if revision != manifest.identity.content_digest
                                || !revision.starts_with("sha256:")
                            {
                                return Err(AgentError::Config(
                                    "clawhub sources require a signed content-digest immutable revision"
                                        .to_string(),
                                ));
                            }
                            (
                                fetch_clawhub_skill_files(
                                    &client,
                                    &key,
                                    requested_version.as_deref(),
                                )
                                .await?,
                                format!("clawhub://{key}"),
                                revision,
                                "clawhub".to_string(),
                            )
                        }
                        _ => {
                            return Err(AgentError::Config(format!(
                                "Unsupported skill registry source '{}'",
                                source
                            )));
                        }
                    }
                } else if let Some((repo, requested_ref, skill_dir)) = explicit {
                    let resolved = resolve_github_skill_source(
                        &client,
                        &repo,
                        requested_ref.as_deref(),
                        &skill_dir,
                    )
                    .await?;
                    let origin = format!("github://{}/{}", resolved.repo, resolved.skill_dir);
                    let revision = resolved.immutable_revision.clone();
                    (
                        fetch_skill_files_from_github(&client, &resolved).await?,
                        origin,
                        revision,
                        "github".to_string(),
                    )
                } else if let Some(skill_hint) = requested_version
                    .as_deref()
                    .filter(|_| looks_like_github_repo_slug(&skill_name))
                {
                    let resolved =
                        resolve_skill_in_repo(&client, &skill_name, skill_hint, Some("skills"))
                            .await?;
                    let origin = format!("github://{}/{}", resolved.repo, resolved.skill_dir);
                    let revision = resolved.immutable_revision.clone();
                    (
                        fetch_skill_files_from_github(&client, &resolved).await?,
                        origin,
                        revision,
                        "github".to_string(),
                    )
                } else if let Ok(hit) =
                    resolve_skill_via_registry_index(&client, &skill_name, None).await
                {
                    match hit.install_source {
                        RegistryInstallSource::GitRepo {
                            repo,
                            requested_ref,
                            skill_dir,
                        } => {
                            let resolved = resolve_github_skill_source(
                                &client,
                                &repo,
                                requested_ref.as_deref(),
                                &skill_dir,
                            )
                            .await?;
                            let origin =
                                format!("github://{}/{}", resolved.repo, resolved.skill_dir);
                            let revision = resolved.immutable_revision.clone();
                            (
                                fetch_skill_files_from_github(&client, &resolved).await?,
                                origin,
                                revision,
                                hit.source,
                            )
                        }
                        RegistryInstallSource::LobeRegistry { slug } => {
                            let revision = manifest.identity.immutable_source_revision.clone();
                            if revision != manifest.identity.content_digest
                                || !revision.starts_with("sha256:")
                            {
                                return Err(AgentError::Config(
                                    "lobehub sources require a signed content-digest immutable revision"
                                        .to_string(),
                                ));
                            }
                            (
                                fetch_lobehub_skill_files(&client, &slug).await?,
                                format!("lobehub://{slug}"),
                                revision,
                                hit.source,
                            )
                        }
                        RegistryInstallSource::ClawRegistry { slug, version } => {
                            let revision = manifest.identity.immutable_source_revision.clone();
                            if revision != manifest.identity.content_digest
                                || !revision.starts_with("sha256:")
                            {
                                return Err(AgentError::Config(
                                    "clawhub sources require a signed content-digest immutable revision"
                                        .to_string(),
                                ));
                            }
                            (
                                fetch_clawhub_skill_files(&client, &slug, version.as_deref())
                                    .await?,
                                format!("clawhub://{slug}"),
                                revision,
                                hit.source,
                            )
                        }
                    }
                } else {
                    let taps_file = hermes_config::hermes_home().join("skill_taps.json");
                    let subscriptions_file = skills_dir.join("subscriptions.json");
                    let taps = effective_skill_taps(&taps_file, &subscriptions_file);
                    let (resolved, route) =
                        resolve_install_via_fallback_router(&client, &skill_name, &taps).await?;
                    let origin = format!("github://{}/{}", resolved.repo, resolved.skill_dir);
                    let revision = resolved.immutable_revision.clone();
                    let source_label = match route {
                        InstallFallbackSource::SkillsSh => "skills.sh",
                        InstallFallbackSource::Tap => "tap",
                    }
                    .to_string();
                    (
                        fetch_skill_files_from_github(&client, &resolved).await?,
                        origin,
                        revision,
                        source_label,
                    )
                };

            validate_remote_manifest_source(&manifest, &origin, &immutable_revision)?;
            let install_name = sanitize_skill_install_name(&manifest.identity.name);
            let trust_label = skill_package_trust_label(&manifest);
            let admission = install_verified_skill_material(
                &skills_dir,
                SkillInstallMaterial {
                    install_name: install_name.clone(),
                    manifest,
                    files,
                },
                &authority,
                bootstrap,
            )?;
            println!(
                "Installed signed remote skill '{}' from {} (revision={}, trust={}, verdict={}, effects_allowed={}).",
                install_name,
                source_label,
                immutable_revision,
                trust_label,
                admission.scan.verdict.as_str(),
                admission.effects_allowed
            );
        }
