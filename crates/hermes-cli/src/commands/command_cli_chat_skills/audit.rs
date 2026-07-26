{
            println!("Verified security audit of installed skills");
            println!("========================================\n");
            if !skills_dir.exists() {
                println!("No skills installed.");
                return Ok(());
            }

            let enforce = skills_extra_has_flag(extra.as_deref(), "--enforce");
            let revocations = load_cli_skill_revocations(&skills_dir)?;
            let mut lock = load_v2_skill_lock(&skills_dir)?;
            if lock.packages.is_empty() {
                println!("No v2 verified skill packages are installed.");
                return Ok(());
            }
            let requires_remote_trust = lock
                .packages
                .iter()
                .any(|entry| entry.manifest.source == hermes_skills::SkillPackageSource::Remote);
            let verifier = load_cli_skill_trust_verifier(
                &skills_dir,
                authority_dir.as_deref(),
                requires_remote_trust,
            )?;

            let mut denied = 0usize;
            let mut changed = false;
            println!(
                "{:28} {:18} {:22} {:10} Status",
                "Skill", "Revision", "Trust", "Verdict"
            );
            println!("{}", "-".repeat(112));
            let admissions = verifier
                .verify_lock(&skills_dir, &lock, &revocations, false)
                .map_err(|error| trust_error("Skill lock verification failed", error))?;
            if admissions.len() != lock.packages.len() {
                return Err(AgentError::Config(
                    "Skill verifier returned an incomplete lock audit".to_string(),
                ));
            }
            for (entry, admission) in lock.packages.iter_mut().zip(admissions) {
                let status = if admission.is_allowed() {
                    format!("admitted effects_allowed={}", admission.effects_allowed)
                } else {
                    denied += 1;
                    let quarantine = admission.quarantine.unwrap_or(hermes_skills::SkillQuarantine {
                        reason: hermes_skills::SkillQuarantineReason::InvalidScan,
                        detail: "verifier denied package without typed evidence".to_string(),
                    });
                    if enforce && entry.quarantine.as_ref() != Some(&quarantine) {
                        entry.quarantine = Some(quarantine.clone());
                        changed = true;
                    }
                    format!("QUARANTINED {:?}: {}", quarantine.reason, quarantine.detail)
                };
                println!(
                    "{:28} {:18} {:22} {:10} {}",
                    entry.manifest.identity.name,
                    entry.manifest.identity.immutable_source_revision,
                    skill_package_trust_label(&entry.manifest),
                    admission.scan.verdict.as_str(),
                    status
                );
            }

            if enforce && changed {
                hermes_skills::store_lock(&skills_dir, &lock)
                    .map_err(|error| trust_error("Failed to atomically store enforced audit lock", error))?;
            }
            println!(
                "\nAudited {} package(s): {} admitted, {} quarantined.",
                lock.packages.len(),
                lock.packages.len().saturating_sub(denied),
                denied
            );
            if enforce {
                if denied > 0 {
                    return Err(AgentError::Config(format!(
                        "Audit enforcement quarantined {denied} skill package(s); they are refused by the v2 lock"
                    )));
                }
                println!("Audit enforcement completed without quarantines.");
            }
        }
