{
            let skill_spec = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills install <name>".into(),
                )
            })?;
            let (skill_name, _requested_version) = parse_skill_name_and_version(&skill_spec);
            println!("Installing skill: {}", skill_name);

            let client = reqwest::Client::new();
            let registry_prefixed = parse_registry_prefixed_skill(&skill_name);
            let explicit = if registry_prefixed.is_some() {
                None
            } else {
                parse_explicit_github_skill(&skill_name)
            };

            let (files, install_seed, provenance) = if let Some((source, key)) = registry_prefixed {
                match source.as_str() {
                    "official" => {
                        let install_key = key.clone();
                        let resolved = resolve_official_skill_source(&client, &key).await?;
                        println!(
                            "Resolved official source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            install_key,
                            SkillInstallProvenance {
                                source: "official".to_string(),
                                identifier: key.clone(),
                                trust_level: default_trust_level_for_source("official").to_string(),
                                metadata: serde_json::json!({
                                    "repo": resolved.repo,
                                    "branch": resolved.branch,
                                    "skill_dir": resolved.skill_dir,
                                }),
                            },
                        )
                    }
                    "skills.sh" => {
                        let install_key = key.clone();
                        let resolved = resolve_skills_sh_source(&client, &key).await?;
                        println!(
                            "Resolved skills.sh source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            install_key,
                            SkillInstallProvenance {
                                source: "skills.sh".to_string(),
                                identifier: key.clone(),
                                trust_level: default_trust_level_for_source("skills.sh")
                                    .to_string(),
                                metadata: serde_json::json!({
                                    "repo": resolved.repo,
                                    "branch": resolved.branch,
                                    "skill_dir": resolved.skill_dir,
                                }),
                            },
                        )
                    }
                    "lobehub" => {
                        println!("Resolved lobehub source: {}", key);
                        (
                            fetch_lobehub_skill_files(&client, &key).await?,
                            key.clone(),
                            SkillInstallProvenance {
                                source: "lobehub".to_string(),
                                identifier: key,
                                trust_level: default_trust_level_for_source("lobehub").to_string(),
                                metadata: serde_json::json!({}),
                            },
                        )
                    }
                    "clawhub" => {
                        println!("Resolved clawhub source: {}", key);
                        (
                            fetch_clawhub_skill_files(&client, &key, _requested_version.as_deref())
                                .await?,
                            key.clone(),
                            SkillInstallProvenance {
                                source: "clawhub".to_string(),
                                identifier: key,
                                trust_level: default_trust_level_for_source("clawhub").to_string(),
                                metadata: serde_json::json!({}),
                            },
                        )
                    }
                    "claude-marketplace" => {
                        let install_key = key.clone();
                        let resolved = resolve_claude_marketplace_skill(&client, &key).await?;
                        println!(
                            "Resolved claude-marketplace source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            install_key,
                            SkillInstallProvenance {
                                source: "claude-marketplace".to_string(),
                                identifier: key.clone(),
                                trust_level: default_trust_level_for_source("claude-marketplace")
                                    .to_string(),
                                metadata: serde_json::json!({
                                    "repo": resolved.repo,
                                    "branch": resolved.branch,
                                    "skill_dir": resolved.skill_dir,
                                }),
                            },
                        )
                    }
                    "github" => {
                        let (repo, maybe_branch, skill_dir) = parse_explicit_github_skill(&key)
                            .ok_or_else(|| {
                                AgentError::Config(format!(
                                    "github/ installs require owner/repo/path, got '{}'",
                                    key
                                ))
                            })?;
                        let branch = if let Some(branch) = maybe_branch {
                            branch
                        } else {
                            github_default_branch(&client, &repo).await?
                        };
                        let resolved = ResolvedSkillSource {
                            repo,
                            branch,
                            skill_dir,
                        };
                        let identifier = format!("{}/{}", resolved.repo, resolved.skill_dir);
                        println!(
                            "Resolved github source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            key,
                            SkillInstallProvenance {
                                source: "github".to_string(),
                                identifier,
                                trust_level: default_trust_level_for_source("github").to_string(),
                                metadata: serde_json::json!({
                                    "repo": resolved.repo,
                                    "branch": resolved.branch,
                                    "skill_dir": resolved.skill_dir,
                                }),
                            },
                        )
                    }
                    _ => {
                        return Err(AgentError::Config(format!(
                            "Unsupported skill registry source '{}'",
                            source
                        )))
                    }
                }
            } else if let Some((repo, maybe_branch, skill_dir)) = explicit {
                let branch = if let Some(branch) = maybe_branch {
                    branch
                } else {
                    github_default_branch(&client, &repo).await?
                };
                let resolved = ResolvedSkillSource {
                    repo,
                    branch,
                    skill_dir,
                };
                let identifier = format!("{}/{}", resolved.repo, resolved.skill_dir);
                println!(
                    "Resolved source: {}/{} @ {}",
                    resolved.repo, resolved.skill_dir, resolved.branch
                );
                (
                    fetch_skill_files_from_github(&client, &resolved).await?,
                    skill_name.clone(),
                    SkillInstallProvenance {
                        source: "github".to_string(),
                        identifier,
                        trust_level: default_trust_level_for_source("github").to_string(),
                        metadata: serde_json::json!({
                            "repo": resolved.repo,
                            "branch": resolved.branch,
                            "skill_dir": resolved.skill_dir,
                        }),
                    },
                )
            } else if let Some(skill_hint) = _requested_version
                .as_deref()
                .filter(|_| looks_like_github_repo_slug(&skill_name))
            {
                let resolved =
                    resolve_skill_in_repo(&client, &skill_name, skill_hint, Some("skills")).await?;
                println!(
                    "Resolved source: {}/{} @ {}",
                    resolved.repo, resolved.skill_dir, resolved.branch
                );
                (
                    fetch_skill_files_from_github(&client, &resolved).await?,
                    skill_name.clone(),
                    SkillInstallProvenance {
                        source: "github".to_string(),
                        identifier: format!("{}/{}", resolved.repo, resolved.skill_dir),
                        trust_level: default_trust_level_for_source("github").to_string(),
                        metadata: serde_json::json!({
                            "repo": resolved.repo,
                            "branch": resolved.branch,
                            "skill_dir": resolved.skill_dir,
                        }),
                    },
                )
            } else {
                let from_index = resolve_skill_via_registry_index(&client, &skill_name, None).await;
                if let Ok(hit) = from_index {
                    if hit.source.eq_ignore_ascii_case("official") {
                        let resolved =
                            resolve_official_skill_source(&client, &hit.identifier).await?;
                        println!(
                            "Resolved source [official]: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            hit.identifier,
                            SkillInstallProvenance {
                                source: "official".to_string(),
                                identifier: format!("{}/{}", resolved.repo, resolved.skill_dir),
                                trust_level: default_trust_level_for_source("official").to_string(),
                                metadata: serde_json::json!({
                                    "repo": resolved.repo,
                                    "branch": resolved.branch,
                                    "skill_dir": resolved.skill_dir,
                                }),
                            },
                        )
                    } else {
                        match hit.install_source {
                            RegistryInstallSource::GitRepo(resolved) => {
                                let branch = github_default_branch(&client, &resolved.repo).await?;
                                let resolved = ResolvedSkillSource { branch, ..resolved };
                                println!(
                                    "Resolved source [{}]: {}/{} @ {}",
                                    hit.source, resolved.repo, resolved.skill_dir, resolved.branch
                                );
                                (
                                    fetch_skill_files_from_github(&client, &resolved).await?,
                                    hit.identifier,
                                    SkillInstallProvenance {
                                        source: hit.source,
                                        identifier: format!(
                                            "{}/{}",
                                            resolved.repo, resolved.skill_dir
                                        ),
                                        trust_level: default_trust_level_for_source("github")
                                            .to_string(),
                                        metadata: serde_json::json!({
                                            "repo": resolved.repo,
                                            "branch": resolved.branch,
                                            "skill_dir": resolved.skill_dir,
                                        }),
                                    },
                                )
                            }
                            RegistryInstallSource::LobeRegistry { slug } => {
                                println!("Resolved source [lobehub]: {}", slug);
                                (
                                    fetch_lobehub_skill_files(&client, &slug).await?,
                                    slug.clone(),
                                    SkillInstallProvenance {
                                        source: "lobehub".to_string(),
                                        identifier: slug,
                                        trust_level: default_trust_level_for_source("lobehub")
                                            .to_string(),
                                        metadata: serde_json::json!({}),
                                    },
                                )
                            }
                            RegistryInstallSource::ClawRegistry { slug, version } => {
                                println!("Resolved source [clawhub]: {}", slug);
                                (
                                    fetch_clawhub_skill_files(&client, &slug, version.as_deref())
                                        .await?,
                                    slug.clone(),
                                    SkillInstallProvenance {
                                        source: "clawhub".to_string(),
                                        identifier: slug,
                                        trust_level: default_trust_level_for_source("clawhub")
                                            .to_string(),
                                        metadata: serde_json::json!({ "version_hint": version }),
                                    },
                                )
                            }
                        }
                    }
                } else {
                    let taps_file = hermes_config::hermes_home().join("skill_taps.json");
                    let subscriptions_file = skills_dir.join("subscriptions.json");
                    let taps = effective_skill_taps(&taps_file, &subscriptions_file);
                    let (resolved, route) =
                        resolve_install_via_fallback_router(&client, &skill_name, &taps).await?;
                    match route {
                        InstallFallbackSource::SkillsSh => println!(
                            "Resolved source [skills.sh fallback]: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        ),
                        InstallFallbackSource::Tap => println!(
                            "Resolved source (tap): {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        ),
                    }
                    (
                        fetch_skill_files_from_github(&client, &resolved).await?,
                        skill_name.clone(),
                        SkillInstallProvenance {
                            source: match route {
                                InstallFallbackSource::SkillsSh => "skills.sh".to_string(),
                                InstallFallbackSource::Tap => "tap".to_string(),
                            },
                            identifier: format!("{}/{}", resolved.repo, resolved.skill_dir),
                            trust_level: default_trust_level_for_source(match route {
                                InstallFallbackSource::SkillsSh => "skills.sh",
                                InstallFallbackSource::Tap => "tap",
                            })
                            .to_string(),
                            metadata: serde_json::json!({
                                "repo": resolved.repo,
                                "branch": resolved.branch,
                                "skill_dir": resolved.skill_dir,
                            }),
                        },
                    )
                }
            };

            let install_name = sanitize_skill_install_name(
                _requested_version
                    .as_deref()
                    .filter(|_| looks_like_github_repo_slug(&skill_name))
                    .unwrap_or(install_seed.as_str()),
            );
            let target = install_skill_files(&skills_dir, &install_name, &files)?;
            record_skill_install_in_hub_lock(
                &skills_dir,
                &install_name,
                &target,
                &files,
                &provenance,
            )?;
            println!("Skill '{}' installed to {}", install_name, target.display());
            maybe_run_skill_bootstrap(&install_name, &target, &files).await?;
        }
