{
            println!("Checking verified skill packages...\n");
            if !skills_dir.exists() {
                println!("No skills installed.");
                return Ok(());
            }

            let lock = load_v2_skill_lock(&skills_dir)?;
            if lock.packages.is_empty() {
                println!(
                    "No v2 verified skill packages are tracked in {}.",
                    hermes_skills::skill_lock_path(&skills_dir).display()
                );
                return Ok(());
            }

            let requires_remote_trust = lock
                .packages
                .iter()
                .any(|entry| entry.manifest.source == hermes_skills::SkillPackageSource::Remote);
            let authority = if requires_remote_trust {
                Some(load_cli_skill_trust_authority_paths(
                    &skills_dir,
                    authority_dir.as_deref(),
                )?)
            } else {
                None
            };
            let verifier = load_cli_skill_trust_verifier(
                &skills_dir,
                authority_dir.as_deref(),
                requires_remote_trust,
            )?;
            let revocations = load_cli_skill_revocations(&skills_dir)?;
            let apply_updates = skills_extra_has_flag(extra.as_deref(), "--apply");
            println!(
                "{:28} {:14} {:18} {:22} Status",
                "Skill", "Version", "Revision", "Trust"
            );
            println!("{}", "-".repeat(112));

            if !apply_updates {
                for entry in &lock.packages {
                    let path = skills_dir.join(&entry.install_path);
                    let status = match verifier.verify_bundle(
                        &path,
                        &entry.manifest,
                        &revocations,
                        false,
                    ) {
                        Ok(admission) if admission.is_allowed() => {
                            format!("verified:{}", admission.scan.verdict.as_str())
                        }
                        Ok(admission) => admission
                            .quarantine
                            .as_ref()
                            .map(|quarantine| format!("quarantined:{:?}", quarantine.reason))
                            .unwrap_or_else(|| "not-admitted".to_string()),
                        Err(error) => format!("verification-error:{error}"),
                    };
                    println!(
                        "{:28} {:14} {:18} {:22} {}",
                        entry.manifest.identity.name,
                        entry.manifest.identity.version,
                        entry.manifest.identity.immutable_source_revision,
                        skill_package_trust_label(&entry.manifest),
                        status
                    );
                }
                println!(
                    "\nNo update was fetched. Re-run with --extra '--apply --manifest <signed-manifest.json>' to stage and verify a signed immutable update."
                );
                return Ok(());
            }

            let manifest = load_remote_skill_manifest(extra.as_deref())?;
            let matching_entry = lock
                .packages
                .iter()
                .find(|entry| entry.manifest.identity.name == manifest.identity.name)
                .ok_or_else(|| {
                    AgentError::Config(format!(
                        "No installed v2 package matches signed update manifest '{}'",
                        manifest.identity.name
                    ))
                })?;
            if matching_entry.manifest.source != hermes_skills::SkillPackageSource::Remote {
                return Err(AgentError::Config(
                    "Only signed Remote packages can be updated from a remote source".to_string(),
                ));
            }
            if manifest.identity.origin != matching_entry.manifest.identity.origin {
                return Err(AgentError::Config(
                    "Signed update origin differs from the installed package; refusing provenance switch"
                        .to_string(),
                ));
            }
            if manifest.identity.publisher_id != matching_entry.manifest.identity.publisher_id {
                return Err(AgentError::Config(
                    "Signed update publisher differs from the installed package; refusing provenance switch"
                        .to_string(),
                ));
            }

            let origin = manifest.identity.origin.clone();
            let rest = origin.strip_prefix("github://").ok_or_else(|| {
                AgentError::Config(
                    "Signed update manifest origin is not a GitHub immutable source; refusing update"
                        .to_string(),
                )
            })?;
            let pieces: Vec<&str> = rest.split('/').filter(|piece| !piece.is_empty()).collect();
            if pieces.len() < 3 {
                return Err(AgentError::Config(
                    "Signed update manifest GitHub origin must include owner/repo/skill-path"
                        .to_string(),
                ));
            }
            let repo = format!("{}/{}", pieces[0], pieces[1]);
            let skill_dir = pieces[2..].join("/");
            let client = reqwest::Client::new();
            let declared_revision = manifest.identity.immutable_source_revision.clone();
            let resolved = resolve_github_skill_source(
                &client,
                &repo,
                Some(&declared_revision),
                &skill_dir,
            )
            .await?;
            let resolved_origin = format!("github://{}/{}", resolved.repo, resolved.skill_dir);
            validate_remote_manifest_source(
                &manifest,
                &resolved_origin,
                &resolved.immutable_revision,
            )?;

            if matching_entry.manifest.identity.immutable_source_revision
                == manifest.identity.immutable_source_revision
                && matching_entry.manifest.identity.content_digest
                    == manifest.identity.content_digest
            {
                println!(
                    "{} is already pinned to signed revision {}.",
                    matching_entry.manifest.identity.name,
                    manifest.identity.immutable_source_revision
                );
                return Ok(());
            }

            let files = fetch_skill_files_from_github(&client, &resolved).await?;
            let trust_label = skill_package_trust_label(&manifest);
            let install_name = manifest.identity.name.clone();
            let authority = authority.as_ref().ok_or_else(|| {
                AgentError::Config(
                    "Signed remote updates require an externally anchored trust authority".to_string(),
                )
            })?;
            let admission = install_verified_skill_material(
                &skills_dir,
                SkillInstallMaterial {
                    install_name: install_name.clone(),
                    manifest,
                    files,
                },
                authority,
                bootstrap_requested(extra.as_deref()),
            )?;
            println!(
                "Updated '{}' to immutable revision {} (trust={}, verdict={}, effects_allowed={}).",
                install_name,
                admission.identity.immutable_source_revision,
                trust_label,
                admission.scan.verdict.as_str(),
                admission.effects_allowed
            );
        }
