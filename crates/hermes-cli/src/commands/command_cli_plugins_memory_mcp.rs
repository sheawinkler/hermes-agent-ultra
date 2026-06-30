include!("command_cli_plugins_memory_mcp/plugins.rs");

pub async fn handle_cli_plugins(
    action: Option<String>,
    name: Option<String>,
    git_ref: Option<String>,
    allow_untrusted_git_host: bool,
) -> Result<(), hermes_core::AgentError> {
    let plugins_dir = hermes_config::hermes_home().join("plugins");

    match action.as_deref() {
        None => {
            run_plugins_interactive_toggle()?;
        }
        Some("list") => {
            let rows = discover_plugin_surface(true);
            println!("Plugin surface ({} entries):", rows.len());
            println!("{}", render_plugin_surface_table(&rows));
        }
        Some("enable") => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins enable <name>".into(),
                )
            })?;
            let target = resolve_local_plugin_path_by_name(&plugin_name)
                .unwrap_or_else(|| plugins_dir.join(&plugin_name));
            let disabled_marker = target.join(".disabled");
            if disabled_marker.exists() {
                std::fs::remove_file(&disabled_marker).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to enable plugin: {}", e))
                })?;
                println!("Plugin '{}' enabled.", plugin_name);
            } else {
                println!(
                    "Plugin '{}' is already enabled (or not installed).",
                    plugin_name
                );
            }
        }
        Some("disable") => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins disable <name>".into(),
                )
            })?;
            let plugin_dir = resolve_local_plugin_path_by_name(&plugin_name)
                .unwrap_or_else(|| plugins_dir.join(&plugin_name));
            if !plugin_dir.exists() {
                println!("Plugin '{}' not found.", plugin_name);
                return Ok(());
            }
            let disabled_marker = plugin_dir.join(".disabled");
            std::fs::write(&disabled_marker, "").map_err(|e| {
                hermes_core::AgentError::Io(format!("Failed to disable plugin: {}", e))
            })?;
            println!("Plugin '{}' disabled.", plugin_name);
        }
        Some("install") => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins install <name|url>".into(),
                )
            })?;
            println!("Installing plugin: {}...", plugin_name);

            let is_git_url = plugin_name.starts_with("http://")
                || plugin_name.starts_with("https://")
                || plugin_name.starts_with("git@");

            if is_git_url {
                if !plugin_git_host_allowed(&plugin_name, allow_untrusted_git_host) {
                    println!(
                        "  ✗ Git host is not on the default allow-list (github.com, gitlab.com, codeberg.org, …)."
                    );
                    println!(
                        "    Set comma-separated HERMES_PLUGIN_GIT_EXTRA_HOSTS or pass --allow-untrusted-git-host after you trust the source."
                    );
                    return Ok(());
                }
                // Extract repo name from URL for target directory
                let repo_name = plugin_name
                    .trim_end_matches('/')
                    .trim_end_matches(".git")
                    .rsplit('/')
                    .next()
                    .unwrap_or("unknown-plugin")
                    .to_string();

                // Also handle git@ SSH URLs like git@github.com:user/repo.git
                let repo_name = if repo_name.contains(':') {
                    repo_name
                        .rsplit(':')
                        .next()
                        .unwrap_or(&repo_name)
                        .trim_end_matches(".git")
                        .rsplit('/')
                        .next()
                        .unwrap_or(&repo_name)
                        .to_string()
                } else {
                    repo_name
                };

                let target = plugins_dir.join(&repo_name);
                if target.exists() {
                    println!(
                        "Plugin '{}' is already installed at {}",
                        repo_name,
                        target.display()
                    );
                    return Ok(());
                }

                std::fs::create_dir_all(&plugins_dir).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to create plugins dir: {}", e))
                })?;

                println!("  Cloning {} ...", plugin_name);
                let output = tokio::process::Command::new("git")
                    .args([
                        "clone",
                        "--depth",
                        "1",
                        &plugin_name,
                        &target.to_string_lossy(),
                    ])
                    .suppress_windows_console()
                    .output()
                    .await
                    .map_err(|e| hermes_core::AgentError::Io(format!("git clone failed: {}", e)))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    println!("  ✗ git clone failed: {}", stderr.trim());
                    return Ok(());
                }

                if let Some(gr) = git_ref.as_deref() {
                    println!("  Checking out ref: {} ...", gr);
                    if let Err(e) = git_checkout_ref(&target, gr).await {
                        println!("  ✗ {}", e);
                        let _ = std::fs::remove_dir_all(&target);
                        return Ok(());
                    }
                }

                // Verify plugin.yaml exists
                let manifest_path = target.join("plugin.yaml");
                if !manifest_path.exists() {
                    println!("  ✗ No plugin.yaml found in cloned repository.");
                    println!("    Removing {}...", target.display());
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }

                // Parse and display plugin info
                let manifest_content = std::fs::read_to_string(&manifest_path)
                    .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
                let manifest: serde_json::Value =
                    serde_yaml::from_str(&manifest_content).unwrap_or(serde_json::json!({}));

                let p_name = manifest
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&repo_name);
                let p_version = manifest
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let p_desc = manifest
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Security scan of cloned files
                let suspicious = scan_plugin_security(&target);
                let hard_block = suspicious.iter().any(|s| {
                    s.contains("curl piped to shell")
                        || s.contains("wget piped to shell")
                        || s.contains("curl|sh style install")
                });
                if hard_block && !allow_untrusted_git_host {
                    println!("\n  ✗ High-risk install patterns detected — clone removed.");
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                    println!(
                        "\n  If you reviewed the code manually, re-run with --allow-untrusted-git-host."
                    );
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }
                if !suspicious.is_empty() {
                    println!("\n  ⚠ Security warnings found ({}):", suspicious.len());
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                    println!("\n  Review the warnings above before enabling this plugin.");
                }

                println!("  ✓ Plugin installed successfully!");
                println!("    Name:        {}", p_name);
                println!("    Version:     {}", p_version);
                println!("    Description: {}", p_desc);
                println!("    Path:        {}", target.display());
            } else if plugin_name.starts_with("gh:") || plugin_name.contains('/') {
                // Convert gh:user/repo or user/repo to a GitHub HTTPS URL
                let repo_path = plugin_name.trim_start_matches("gh:");
                let git_url = format!("https://github.com/{}.git", repo_path);
                let repo_name = repo_path.rsplit('/').next().unwrap_or("unknown-plugin");
                let target = plugins_dir.join(repo_name);
                if target.exists() {
                    println!("Plugin '{}' is already installed.", repo_name);
                    return Ok(());
                }

                std::fs::create_dir_all(&plugins_dir).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to create plugins dir: {}", e))
                })?;

                println!("  Cloning from GitHub: {}", git_url);
                let output = tokio::process::Command::new("git")
                    .args(["clone", "--depth", "1", &git_url, &target.to_string_lossy()])
                    .suppress_windows_console()
                    .output()
                    .await
                    .map_err(|e| hermes_core::AgentError::Io(format!("git clone failed: {}", e)))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    println!("  ✗ git clone failed: {}", stderr.trim());
                    return Ok(());
                }

                if let Some(gr) = git_ref.as_deref() {
                    println!("  Checking out ref: {} ...", gr);
                    if let Err(e) = git_checkout_ref(&target, gr).await {
                        println!("  ✗ {}", e);
                        let _ = std::fs::remove_dir_all(&target);
                        return Ok(());
                    }
                }

                let manifest_path = target.join("plugin.yaml");
                if !manifest_path.exists() {
                    println!("  ✗ No plugin.yaml found in cloned repository.");
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }

                let manifest_content = std::fs::read_to_string(&manifest_path).unwrap_or_default();
                let manifest: serde_json::Value =
                    serde_yaml::from_str(&manifest_content).unwrap_or(serde_json::json!({}));

                let p_name = manifest
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(repo_name);
                let p_version = manifest
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let p_desc = manifest
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let suspicious = scan_plugin_security(&target);
                let hard_block = suspicious.iter().any(|s| {
                    s.contains("curl piped to shell")
                        || s.contains("wget piped to shell")
                        || s.contains("curl|sh style install")
                });
                if hard_block && !allow_untrusted_git_host {
                    println!("\n  ✗ High-risk install patterns detected — clone removed.");
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                    println!(
                        "\n  If you reviewed the code manually, re-run with --allow-untrusted-git-host."
                    );
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }
                if !suspicious.is_empty() {
                    println!("\n  ⚠ Security warnings found ({}):", suspicious.len());
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                }

                println!("  ✓ Plugin installed successfully!");
                println!("    Name:        {}", p_name);
                println!("    Version:     {}", p_version);
                println!("    Description: {}", p_desc);
                println!("    Path:        {}", target.display());
            } else {
                let target = plugins_dir.join(&plugin_name);
                if target.exists() {
                    println!("Plugin '{}' is already installed.", plugin_name);
                    return Ok(());
                }
                // Registry lookup
                println!("  Looking up '{}' in plugin registry...", plugin_name);
                match reqwest::Client::new()
                    .get(format!(
                        "https://plugins.hermes.run/api/v1/{}",
                        plugin_name
                    ))
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        if let Ok(data) = resp.json::<serde_json::Value>().await {
                            let version = data
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("latest");
                            let git_url = data.get("git_url").and_then(|v| v.as_str());
                            println!("  Found {} v{}", plugin_name, version);

                            if let Some(url) = git_url {
                                if !plugin_git_host_allowed(url, allow_untrusted_git_host) {
                                    println!("  ✗ Registry git_url host is not allow-listed. Use --allow-untrusted-git-host or HERMES_PLUGIN_GIT_EXTRA_HOSTS.");
                                    return Ok(());
                                }
                                std::fs::create_dir_all(&plugins_dir)
                                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

                                let output = tokio::process::Command::new("git")
                                    .args(["clone", "--depth", "1", url, &target.to_string_lossy()])
                                    .suppress_windows_console()
                                    .output()
                                    .await
                                    .map_err(|e| {
                                        hermes_core::AgentError::Io(format!(
                                            "git clone failed: {}",
                                            e
                                        ))
                                    })?;

                                if output.status.success() {
                                    if let Some(gr) = git_ref.as_deref() {
                                        println!("  Checking out ref: {} ...", gr);
                                        if let Err(e) = git_checkout_ref(&target, gr).await {
                                            println!("  ✗ {}", e);
                                            let _ = std::fs::remove_dir_all(&target);
                                            return Ok(());
                                        }
                                    }
                                    let suspicious = scan_plugin_security(&target);
                                    let hard_block = suspicious.iter().any(|s| {
                                        s.contains("curl piped to shell")
                                            || s.contains("wget piped to shell")
                                            || s.contains("curl|sh style install")
                                    });
                                    if hard_block && !allow_untrusted_git_host {
                                        println!("  ✗ High-risk patterns — removed clone.");
                                        let _ = std::fs::remove_dir_all(&target);
                                        return Ok(());
                                    }
                                    if !suspicious.is_empty() {
                                        println!("  ⚠ Security warnings: {}", suspicious.len());
                                        for w in &suspicious {
                                            println!("    - {}", w);
                                        }
                                    }
                                    println!(
                                        "  ✓ Plugin '{}' v{} installed.",
                                        plugin_name, version
                                    );
                                } else {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    println!("  ✗ Clone failed: {}", stderr.trim());
                                }
                            } else {
                                println!("  No git_url in registry response. Cannot install.");
                            }
                        }
                    }
                    _ => {
                        println!("  Plugin '{}' not found in registry.", plugin_name);
                        println!("  Try installing from a URL or GitHub repo instead:");
                        println!("    hermes plugins install https://github.com/user/repo");
                        println!("    hermes plugins install gh:user/repo");
                    }
                }
            }
        }
        Some("remove") | Some("uninstall") => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins remove <name>".into(),
                )
            })?;
            let target = resolve_local_plugin_path_by_name(&plugin_name)
                .unwrap_or_else(|| plugins_dir.join(&plugin_name));
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to remove plugin: {}", e))
                })?;
                println!("Plugin '{}' removed.", plugin_name);
            } else {
                println!("Plugin '{}' not found.", plugin_name);
            }
        }
        Some("update") => {
            let plugin_name = name.as_deref();
            let mut checked = 0u32;
            let mut updated = 0u32;
            if !plugins_dir.exists() {
                println!("No plugins installed.");
                return Ok(());
            }
            if let Ok(entries) = std::fs::read_dir(&plugins_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let dir_name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    if let Some(target) = plugin_name {
                        if dir_name != target {
                            continue;
                        }
                    }
                    let manifest = path.join("plugin.yaml");
                    if manifest.exists() {
                        checked += 1;
                        println!("  Checking updates for '{}'...", dir_name);

                        let git_dir = path.join(".git");
                        if !git_dir.exists() {
                            println!("    Skipped: plugin is not a git checkout.");
                            continue;
                        }

                        let path_s = path.to_string_lossy().to_string();
                        let before = tokio::process::Command::new("git")
                            .args(["-C", &path_s, "rev-parse", "HEAD"])
                            .suppress_windows_console()
                            .output()
                            .await
                            .map_err(|e| {
                                hermes_core::AgentError::Io(format!(
                                    "git rev-parse failed for {}: {}",
                                    dir_name, e
                                ))
                            })?;
                        if !before.status.success() {
                            let stderr = String::from_utf8_lossy(&before.stderr);
                            println!(
                                "    Skipped: cannot read current revision ({})",
                                stderr.trim()
                            );
                            continue;
                        }
                        let before_sha = String::from_utf8_lossy(&before.stdout).trim().to_string();

                        let pull = tokio::process::Command::new("git")
                            .args(["-C", &path_s, "pull", "--ff-only"])
                            .suppress_windows_console()
                            .output()
                            .await
                            .map_err(|e| {
                                hermes_core::AgentError::Io(format!(
                                    "git pull failed for {}: {}",
                                    dir_name, e
                                ))
                            })?;

                        if !pull.status.success() {
                            let stderr = String::from_utf8_lossy(&pull.stderr);
                            println!("    Update failed: {}", stderr.trim());
                            continue;
                        }

                        let after = tokio::process::Command::new("git")
                            .args(["-C", &path_s, "rev-parse", "HEAD"])
                            .suppress_windows_console()
                            .output()
                            .await
                            .map_err(|e| {
                                hermes_core::AgentError::Io(format!(
                                    "git rev-parse failed for {} after update: {}",
                                    dir_name, e
                                ))
                            })?;
                        if !after.status.success() {
                            let stderr = String::from_utf8_lossy(&after.stderr);
                            println!(
                                "    Updated but could not read final revision ({})",
                                stderr.trim()
                            );
                            continue;
                        }
                        let after_sha = String::from_utf8_lossy(&after.stdout).trim().to_string();

                        if before_sha == after_sha {
                            println!("    Up to date ({})", short_sha(&after_sha));
                        } else {
                            updated += 1;
                            println!(
                                "    Updated: {} -> {}",
                                short_sha(&before_sha),
                                short_sha(&after_sha)
                            );
                        }
                    }
                }
            }
            if checked == 0 {
                if let Some(n) = plugin_name {
                    println!("Plugin '{}' not found.", n);
                } else {
                    println!("No plugins to update.");
                }
            } else {
                println!("Checked {} plugin(s); updated {}.", checked, updated);
            }
        }
        Some("inspect") | Some("info") => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins inspect <name>".into(),
                )
            })?;
            let surface_rows = discover_plugin_surface(true);
            if let Some(row) = surface_rows
                .iter()
                .find(|row| row.name.eq_ignore_ascii_case(&plugin_name))
            {
                println!("Plugin: {}", row.name);
                println!("Source: {}", row.source.label());
                println!(
                    "Status: {}",
                    if row.enabled { "enabled" } else { "disabled" }
                );
                let version = if row.version.trim().is_empty() {
                    "unknown"
                } else {
                    row.version.as_str()
                };
                println!("Version: {}", version);
                if let Some(kind) = row.kind.as_deref().filter(|k| !k.trim().is_empty()) {
                    println!("Kind: {}", kind);
                }
                if let Some(path) = row.path.as_deref() {
                    println!("Path: {}", path.display());
                }
                if !row.description.trim().is_empty() {
                    println!("Description: {}", row.description.trim());
                }
            }
            let target = resolve_local_plugin_path_by_name(&plugin_name)
                .unwrap_or_else(|| plugins_dir.join(&plugin_name));
            if !target.exists() {
                if surface_rows
                    .iter()
                    .any(|row| row.name.eq_ignore_ascii_case(&plugin_name))
                {
                    return Ok(());
                }
                println!("Plugin '{}' not found.", plugin_name);
                return Ok(());
            }
            let manifest_path = target.join("plugin.yaml");
            if manifest_path.exists() {
                let content = std::fs::read_to_string(&manifest_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Plugin: {}", plugin_name);
                println!("Path:   {}", target.display());
                let disabled = target.join(".disabled").exists();
                println!("Status: {}", if disabled { "disabled" } else { "enabled" });
                println!("\n--- plugin.yaml ---");
                println!("{}", content);
            } else {
                println!("Plugin '{}' has no plugin.yaml manifest.", plugin_name);
            }
        }
        Some(other) => {
            println!("Plugins action '{}' is not recognized.", other);
            println!("Available: list, install, remove, enable, disable, update, inspect");
        }
    }
    Ok(())
}

include!("command_cli_plugins_memory_mcp/memory_setup.rs");
/// Handle `hermes memory [action]`.
pub async fn handle_cli_memory(
    action: Option<String>,
    target: Option<String>,
    yes: bool,
) -> Result<(), hermes_core::AgentError> {
    let hermes_home = hermes_config::hermes_home();
    let memories_dir = hermes_home.join("memories");
    let memory_md = memories_dir.join("MEMORY.md");
    let user_md = memories_dir.join("USER.md");
    let legacy_memory_db = hermes_home.join("memory.db");
    let disabled_marker = hermes_home.join(".memory_disabled");

    match action.as_deref().unwrap_or("status") {
        "status" => {
            println!("{}", render_memory_backend_status(&hermes_home));
        }
        "setup" => {
            if let Some(provider) = target
                .as_deref()
                .map(str::trim)
                .filter(|provider| !provider.is_empty())
            {
                let path = setup_memory_provider_target(provider, yes)?;
                println!("Configured memory provider '{}'.", provider);
                println!("  Config: {}", path.display());
                println!(
                    "Memory provider config is owner-only and activates on subsequent sessions."
                );
                return Ok(());
            }
            println!("Memory Provider Setup");
            println!("---------------------");
            std::fs::create_dir_all(&memories_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            if !memory_md.exists() {
                std::fs::write(
                    &memory_md,
                    "# Hermes MEMORY\n\nStore durable assistant memory entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            if !user_md.exists() {
                std::fs::write(
                    &user_md,
                    "# Hermes USER\n\nStore durable user profile entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            if disabled_marker.exists() {
                let _ = std::fs::remove_file(&disabled_marker);
            }
            println!("Initialized file memory backend.");
            println!("  MEMORY.md: {}", memory_md.display());
            println!("  USER.md:   {}", user_md.display());
            println!("Memory is enabled for subsequent sessions.");
        }
        "off" => {
            std::fs::create_dir_all(&hermes_home)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            std::fs::write(
                &disabled_marker,
                format!(
                    "disabled_at={}\nreason=hermes memory off\n",
                    chrono::Utc::now().to_rfc3339()
                ),
            )
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!("Memory provider disabled.");
            println!("  Marker: {}", disabled_marker.display());
            println!("Run `hermes memory setup` to re-enable.");
        }
        "reset" => {
            if !yes {
                return Err(hermes_core::AgentError::Config(
                    "memory reset requires confirmation flag: use `hermes memory reset [all|memory|user] -y`"
                        .into(),
                ));
            }
            let reset_target = target
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("all")
                .to_ascii_lowercase();
            let reset_memory = reset_target == "all" || reset_target == "memory";
            let reset_user = reset_target == "all" || reset_target == "user";
            if !reset_memory && !reset_user {
                return Err(hermes_core::AgentError::Config(format!(
                    "Unknown memory reset target '{}'. Use all|memory|user",
                    reset_target
                )));
            }
            std::fs::create_dir_all(&memories_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            if reset_memory && memory_md.exists() {
                let _ = std::fs::remove_file(&memory_md);
            }
            if reset_user && user_md.exists() {
                let _ = std::fs::remove_file(&user_md);
            }
            if reset_target == "all" && legacy_memory_db.exists() {
                let _ = std::fs::remove_file(&legacy_memory_db);
            }
            if disabled_marker.exists() {
                let _ = std::fs::remove_file(&disabled_marker);
            }
            if reset_memory {
                std::fs::write(
                    &memory_md,
                    "# Hermes MEMORY\n\nStore durable assistant memory entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            if reset_user {
                std::fs::write(
                    &user_md,
                    "# Hermes USER\n\nStore durable user profile entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            println!(
                "Memory reset complete (target={}). MEMORY.md={} USER.md={}",
                reset_target,
                if memory_md.exists() {
                    "present"
                } else {
                    "absent"
                },
                if user_md.exists() {
                    "present"
                } else {
                    "absent"
                }
            );
        }
        other => {
            println!("Unknown memory action '{}'.", other);
            println!("Available actions: status, setup, off, reset");
        }
    }
    Ok(())
}

include!("command_cli_plugins_memory_mcp/mcp_commands.rs");
