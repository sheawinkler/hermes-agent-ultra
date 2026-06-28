{
            println!("Checking for skill updates...\n");
            if !skills_dir.exists() {
                println!("No skills installed.");
                return Ok(());
            }

            let apply_updates = extra.as_deref() == Some("--apply");
            let lock = read_skills_hub_lock(&skills_dir);
            if lock.installed.is_empty() {
                println!(
                    "No hub-installed skills tracked in {}.",
                    skills_hub_lock_path(&skills_dir).display()
                );
                println!("Install skills with `hermes skills install <identifier>` to enable source-aware updates.");
                return Ok(());
            }

            println!(
                "{:28} {:14} {:14} {:16} Status",
                "Skill", "Source", "Local Hash", "Upstream Hash"
            );
            println!("{}", "-".repeat(98));

            let taps_file = hermes_config::hermes_home().join("skill_taps.json");
            let subscriptions_file = skills_dir.join("subscriptions.json");
            let merged_taps = effective_skill_taps(&taps_file, &subscriptions_file);
            let client = reqwest::Client::new();

            struct PendingUpdate {
                entry: SkillHubInstalledEntry,
                files: Vec<(String, Bytes)>,
                upstream_hash: String,
            }
            let mut pending: Vec<PendingUpdate> = Vec::new();

            for entry in lock.installed {
                let local_hash = if skills_dir.join(&entry.install_path).exists() {
                    hash_installed_skill_dir(&skills_dir.join(&entry.install_path))
                        .unwrap_or_else(|_| entry.content_hash.clone())
                } else {
                    entry.content_hash.clone()
                };

                match fetch_bundle_for_lock_entry(&client, &entry, &merged_taps).await {
                    Ok(files) => {
                        let upstream_hash = hash_skill_bundle(&files);
                        let status = if local_hash == upstream_hash {
                            "✓ up-to-date"
                        } else {
                            pending.push(PendingUpdate {
                                entry: entry.clone(),
                                files,
                                upstream_hash: upstream_hash.clone(),
                            });
                            "⬆ update available"
                        };
                        println!(
                            "{:28} {:14} {:14} {:16} {}",
                            entry.name,
                            entry.source,
                            &local_hash.chars().take(14).collect::<String>(),
                            &upstream_hash.chars().take(16).collect::<String>(),
                            status
                        );
                    }
                    Err(err) => {
                        println!(
                            "{:28} {:14} {:14} {:16} unavailable ({})",
                            entry.name,
                            entry.source,
                            &local_hash.chars().take(14).collect::<String>(),
                            "-",
                            err
                        );
                    }
                }
            }

            println!();
            if pending.is_empty() {
                println!("All tracked hub skills are up to date.");
            } else {
                println!("{} update(s) available.", pending.len());
                if apply_updates {
                    println!("\nApplying updates...");
                    for update in pending {
                        let install_name = sanitize_skill_install_name(&update.entry.name);
                        let target =
                            install_skill_files(&skills_dir, &install_name, &update.files)?;
                        let prov = SkillInstallProvenance {
                            source: update.entry.source.clone(),
                            identifier: update.entry.identifier.clone(),
                            trust_level: update.entry.trust_level.clone(),
                            metadata: update.entry.metadata.clone(),
                        };
                        record_skill_install_in_hub_lock(
                            &skills_dir,
                            &install_name,
                            &target,
                            &update.files,
                            &prov,
                        )?;
                        println!(
                            "  ✓ {} updated (new hash: {})",
                            install_name,
                            &update.upstream_hash.chars().take(16).collect::<String>()
                        );
                        maybe_run_skill_bootstrap(&install_name, &target, &update.files).await?;
                    }
                } else {
                    println!("Run `hermes skills update --apply` to install updates.");
                }
            }
        }
