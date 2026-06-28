async fn fetch_bundle_for_lock_entry(
    client: &reqwest::Client,
    entry: &SkillHubInstalledEntry,
    taps: &[String],
) -> Result<Vec<(String, Bytes)>, AgentError> {
    match entry.source.as_str() {
        "official" => {
            let resolved = resolve_official_skill_source(client, &entry.identifier).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "skills.sh" | "skills-sh" => {
            let id = canonicalize_skills_sh_identifier(&entry.identifier);
            let resolved = resolve_skills_sh_source(client, &id).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "lobehub" => fetch_lobehub_skill_files(client, &entry.identifier).await,
        "clawhub" => fetch_clawhub_skill_files(client, &entry.identifier, None).await,
        "claude-marketplace" => {
            let resolved = resolve_claude_marketplace_skill(client, &entry.identifier).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "tap" => {
            if let Some((repo, skill_dir)) = parse_repo_skill_identifier(&entry.identifier) {
                let branch = github_default_branch(client, &repo).await?;
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            let resolved = resolve_skill_via_taps(client, taps, &entry.name).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "github" => {
            if let Some((repo, maybe_branch, skill_dir)) =
                parse_explicit_github_skill(&entry.identifier)
            {
                let branch = if let Some(branch) = maybe_branch {
                    branch
                } else {
                    github_default_branch(client, &repo).await?
                };
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            if let Some((repo, skill_dir)) = parse_repo_skill_identifier(&entry.identifier) {
                let branch = github_default_branch(client, &repo).await?;
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            let resolved =
                resolve_skill_in_repo(client, &entry.identifier, &entry.name, None).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        other => {
            if let Ok(hit) =
                resolve_skill_via_registry_index(client, &entry.identifier, Some(other)).await
            {
                return match hit.install_source {
                    RegistryInstallSource::GitRepo(source) => {
                        let branch = github_default_branch(client, &source.repo).await?;
                        fetch_skill_files_from_github(
                            client,
                            &ResolvedSkillSource { branch, ..source },
                        )
                        .await
                    }
                    RegistryInstallSource::LobeRegistry { slug } => {
                        fetch_lobehub_skill_files(client, &slug).await
                    }
                    RegistryInstallSource::ClawRegistry { slug, version } => {
                        fetch_clawhub_skill_files(client, &slug, version.as_deref()).await
                    }
                };
            }
            Err(AgentError::Config(format!(
                "Unknown hub source '{}' for installed skill '{}'",
                entry.source, entry.name
            )))
        }
    }
}

fn install_skill_files(
    skills_dir: &std::path::Path,
    install_name: &str,
    files: &[(String, Bytes)],
) -> Result<std::path::PathBuf, AgentError> {
    skill_guard_scan_bundle(files)?;

    std::fs::create_dir_all(skills_dir)
        .map_err(|e| AgentError::Io(format!("Failed to create skills dir: {}", e)))?;

    let target = skills_dir.join(install_name);
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .map_err(|e| AgentError::Io(format!("Failed to remove existing skill dir: {}", e)))?;
    }
    std::fs::create_dir_all(&target)
        .map_err(|e| AgentError::Io(format!("Failed to create skill dir: {}", e)))?;

    for (rel_path, bytes) in files {
        ensure_safe_relative_path(rel_path)?;
        let output = target.join(rel_path);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("Failed to create parent dirs: {}", e)))?;
        }
        std::fs::write(&output, bytes)
            .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", output.display(), e)))?;
    }

    let skill_md = target.join("SKILL.md");
    if !skill_md.exists() {
        return Err(AgentError::Config(format!(
            "Installed skill is missing SKILL.md at {}",
            skill_md.display()
        )));
    }

    Ok(target)
}

fn skill_auto_bootstrap_enabled() -> bool {
    !std::env::var("HERMES_SKILL_AUTO_BOOTSTRAP")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
}

fn skill_bootstrap_force_confirmed() -> bool {
    std::env::var("HERMES_SKILL_BOOTSTRAP_ASSUME_YES")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        || std::env::var("HERMES_SKILL_BOOTSTRAP_FORCE")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
}

fn prompt_bootstrap_yes_no(prompt: &str, default_yes: bool) -> bool {
    use std::io::Write as _;
    print!("{}", prompt);
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return default_yes;
    }
    let answer = buf.trim().to_ascii_lowercase();
    if answer.is_empty() {
        return default_yes;
    }
    matches!(answer.as_str(), "y" | "yes")
}

fn push_bootstrap_command_if_present(commands: &mut Vec<String>, raw: Option<&str>) {
    if let Some(raw) = raw {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            commands.push(trimmed.to_string());
        }
    }
}

fn collect_bootstrap_commands_from_value(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => push_bootstrap_command_if_present(out, Some(s)),
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    push_bootstrap_command_if_present(out, Some(s));
                }
            }
        }
        serde_json::Value::Object(map) => {
            push_bootstrap_command_if_present(out, map.get("command").and_then(|v| v.as_str()));
            if let Some(commands) = map.get("commands") {
                collect_bootstrap_commands_from_value(commands, out);
            }
            if let Some(script) = map.get("script").and_then(|v| v.as_str()) {
                let script = script.trim();
                if !script.is_empty() {
                    if script.ends_with(".py") {
                        out.push(format!("python3 {}", script));
                    } else {
                        out.push(format!("bash {}", script));
                    }
                }
            }
            if let Some(scripts) = map.get("scripts").and_then(|v| v.as_array()) {
                for script in scripts {
                    if let Some(script) = script.as_str() {
                        let script = script.trim();
                        if script.is_empty() {
                            continue;
                        }
                        if script.ends_with(".py") {
                            out.push(format!("python3 {}", script));
                        } else {
                            out.push(format!("bash {}", script));
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn parse_skill_bootstrap_plan(
    files: &[(String, Bytes)],
) -> Result<Option<SkillBootstrapPlan>, AgentError> {
    let skill_md = files
        .iter()
        .find_map(|(path, bytes)| {
            if path == "SKILL.md" {
                Some(bytes)
            } else {
                None
            }
        })
        .ok_or_else(|| AgentError::Config("Installed skill payload is missing SKILL.md".into()))?;

    let content = std::str::from_utf8(skill_md)
        .map_err(|e| AgentError::Config(format!("Installed SKILL.md is not valid UTF-8: {}", e)))?;
    let (frontmatter, _body) = hermes_tools::tools::skill_utils::parse_frontmatter(content);

    let mut commands = Vec::new();
    for key in [
        "bootstrap",
        "setup",
        "install",
        "bootstrap_command",
        "setup_command",
        "install_command",
        "bootstrap_commands",
        "setup_commands",
        "install_commands",
    ] {
        if let Some(value) = frontmatter.get(key) {
            collect_bootstrap_commands_from_value(value, &mut commands);
        }
    }

    let mut dedup = HashSet::new();
    let normalized: Vec<String> = commands
        .into_iter()
        .filter_map(|cmd| {
            let trimmed = cmd.trim().to_string();
            if trimmed.is_empty() || !dedup.insert(trimmed.clone()) {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect();

    if normalized.is_empty() {
        Ok(None)
    } else {
        Ok(Some(SkillBootstrapPlan {
            commands: normalized,
        }))
    }
}

fn is_allowed_bootstrap_executable(executable: &str) -> bool {
    let normalized = executable
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(executable)
        .trim()
        .to_ascii_lowercase();
    SKILL_BOOTSTRAP_ALLOWED_EXECUTABLES
        .iter()
        .any(|allowed| *allowed == normalized)
}

fn parse_bootstrap_command(raw: &str) -> Result<ParsedBootstrapCommand, AgentError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AgentError::Config(
            "Bootstrap command cannot be empty".to_string(),
        ));
    }
    if trimmed.len() > 2048 {
        return Err(AgentError::Config(
            "Bootstrap command is too long (>2048 bytes)".to_string(),
        ));
    }

    // Deliberately block shell control operators and substitutions.
    let forbidden = Regex::new(r"[`\n\r;]|&&|\|\||\||>>?|<<?|\$\(").expect("valid regex");
    if forbidden.is_match(trimmed) {
        return Err(AgentError::Config(format!(
            "Blocked bootstrap command (contains forbidden shell operators): {}",
            trimmed
        )));
    }

    let mut tokens = shlex::split(trimmed).ok_or_else(|| {
        AgentError::Config(format!(
            "Unable to parse bootstrap command safely: {}",
            trimmed
        ))
    })?;
    if tokens.is_empty() {
        return Err(AgentError::Config(
            "Bootstrap command parsed to no executable".to_string(),
        ));
    }

    let executable = tokens.remove(0);
    if executable.contains('/') || executable.contains('\\') {
        let path = Path::new(&executable);
        if path.is_absolute() {
            return Err(AgentError::Config(format!(
                "Bootstrap executable must be relative (got absolute path): {}",
                executable
            )));
        }
        ensure_safe_relative_path(&executable)?;
        if executable.ends_with(".sh") {
            let mut args = vec![executable];
            args.extend(tokens);
            return Ok(ParsedBootstrapCommand {
                display: trimmed.to_string(),
                executable: "bash".to_string(),
                args,
            });
        }
        if executable.ends_with(".py") {
            let mut args = vec![executable];
            args.extend(tokens);
            return Ok(ParsedBootstrapCommand {
                display: trimmed.to_string(),
                executable: "python3".to_string(),
                args,
            });
        }
    } else if !is_allowed_bootstrap_executable(&executable) {
        return Err(AgentError::Config(format!(
            "Bootstrap executable '{}' is not in the allowlist",
            executable
        )));
    }

    Ok(ParsedBootstrapCommand {
        display: trimmed.to_string(),
        executable,
        args: tokens,
    })
}

async fn execute_bootstrap_command(
    skill_dir: &Path,
    command: &ParsedBootstrapCommand,
) -> Result<(), AgentError> {
    let exec_path = if command.executable.contains('/') || command.executable.contains('\\') {
        skill_dir.join(&command.executable)
    } else {
        PathBuf::from(&command.executable)
    };

    let mut process = tokio::process::Command::new(&exec_path);
    process
        .args(&command.args)
        .current_dir(skill_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = process.output().await.map_err(|e| {
        AgentError::Io(format!(
            "Failed to execute bootstrap command '{}': {}",
            command.display, e
        ))
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        if !stdout.is_empty() {
            println!(
                "    stdout: {}",
                stdout.lines().take(3).collect::<Vec<_>>().join(" | ")
            );
        }
        Ok(())
    } else {
        Err(AgentError::Config(format!(
            "Bootstrap command failed (exit={}): {}\n{}\n{}",
            output.status,
            command.display,
            if stdout.is_empty() { "" } else { "stdout:" },
            if stdout.is_empty() {
                stderr
            } else if stderr.is_empty() {
                stdout
            } else {
                format!("{}\nstderr:\n{}", stdout, stderr)
            }
        )))
    }
}

async fn maybe_run_skill_bootstrap(
    install_name: &str,
    skill_dir: &Path,
    files: &[(String, Bytes)],
) -> Result<(), AgentError> {
    if !skill_auto_bootstrap_enabled() {
        println!("Skill bootstrap skipped: HERMES_SKILL_AUTO_BOOTSTRAP=0.");
        return Ok(());
    }

    let Some(plan) = parse_skill_bootstrap_plan(files)? else {
        return Ok(());
    };

    let mut runnable: Vec<(ParsedBootstrapCommand, hermes_tools::ApprovalDecision)> = Vec::new();
    let mut blocked: Vec<(String, String)> = Vec::new();
    for raw in plan.commands {
        match parse_bootstrap_command(&raw) {
            Ok(parsed) => {
                let decision = hermes_tools::check_approval(&parsed.display);
                if matches!(decision, hermes_tools::ApprovalDecision::Denied) {
                    blocked.push((
                        parsed.display,
                        "blocked by command approval policy".to_string(),
                    ));
                } else {
                    runnable.push((parsed, decision));
                }
            }
            Err(err) => blocked.push((raw, err.to_string())),
        }
    }

    if runnable.is_empty() && blocked.is_empty() {
        return Ok(());
    }

    println!(
        "Detected bootstrap plan for '{}': {} runnable command(s), {} blocked.",
        install_name,
        runnable.len(),
        blocked.len()
    );
    for (cmd, reason) in &blocked {
        println!("  - blocked: `{}` ({})", cmd, reason);
    }
    if runnable.is_empty() {
        return Ok(());
    }

    let has_confirm = runnable.iter().any(|(_, decision)| {
        matches!(
            decision,
            hermes_tools::ApprovalDecision::RequiresConfirmation
        )
    });
    let force_yes = skill_bootstrap_force_confirmed();
    if has_confirm && !force_yes {
        let proceed = prompt_bootstrap_yes_no(
            "Run bootstrap commands that require installer confirmation now? [Y/n]: ",
            true,
        );
        if !proceed {
            println!("Skipped bootstrap execution.");
            return Ok(());
        }
    }

    for (command, decision) in runnable {
        if matches!(
            decision,
            hermes_tools::ApprovalDecision::RequiresConfirmation
        ) && !force_yes
        {
            println!("  • running (confirmed): {}", command.display);
        } else if matches!(decision, hermes_tools::ApprovalDecision::Approved) {
            println!("  • running: {}", command.display);
        } else if !force_yes {
            println!("  • skipped: {} (confirmation required)", command.display);
            continue;
        } else {
            println!("  • running (forced): {}", command.display);
        }
        execute_bootstrap_command(skill_dir, &command).await?;
    }

    Ok(())
}

