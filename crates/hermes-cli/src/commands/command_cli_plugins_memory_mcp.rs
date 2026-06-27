// ---------------------------------------------------------------------------
// Plugin discovery / surface rendering
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PluginSurfaceSource {
    User,
    Project,
}

impl PluginSurfaceSource {
    fn label(&self) -> &'static str {
        match self {
            PluginSurfaceSource::User => "user",
            PluginSurfaceSource::Project => "project",
        }
    }
}

#[derive(Debug, Clone)]
struct PluginSurfaceEntry {
    name: String,
    version: String,
    description: String,
    kind: Option<String>,
    source: PluginSurfaceSource,
    path: Option<PathBuf>,
    enabled: bool,
}

fn coerce_memory_provider_kind(path: &Path, kind: Option<String>) -> Option<String> {
    let explicit_kind = kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    if explicit_kind.is_some() {
        return explicit_kind;
    }
    let init_file = path.join("__init__.py");
    let Ok(source) = std::fs::read_to_string(&init_file) else {
        return None;
    };
    let probe = if source.len() > 8192 {
        &source[..8192]
    } else {
        source.as_str()
    };
    if probe.contains("register_memory_provider") || probe.contains("MemoryProvider") {
        Some("exclusive".to_string())
    } else {
        None
    }
}

fn scan_plugin_manifest_root(root: &Path, source: PluginSurfaceSource) -> Vec<PluginSurfaceEntry> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("plugin.yaml");
        if !manifest_path.exists() {
            continue;
        }
        let content = match std::fs::read_to_string(&manifest_path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let manifest: PluginManifest = match serde_yaml::from_str(&content) {
            Ok(manifest) => manifest,
            Err(_) => continue,
        };
        let disabled_marker = path.join(".disabled");
        out.push(PluginSurfaceEntry {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            kind: coerce_memory_provider_kind(&path, manifest.kind.clone()),
            source,
            path: Some(path),
            enabled: !disabled_marker.exists(),
        });
    }
    out
}

fn discover_plugin_surface(_include_entrypoints: bool) -> Vec<PluginSurfaceEntry> {
    let mut rows = Vec::new();
    let user_root = hermes_config::hermes_home().join("plugins");
    rows.extend(scan_plugin_manifest_root(
        &user_root,
        PluginSurfaceSource::User,
    ));

    if hermes_config::env_var_enabled("HERMES_ENABLE_PROJECT_PLUGINS") {
        if let Ok(cwd) = std::env::current_dir() {
            let project_root = cwd.join(".hermes").join("plugins");
            rows.extend(scan_plugin_manifest_root(
                &project_root,
                PluginSurfaceSource::Project,
            ));
        }
    }

    rows.sort_by(|a, b| {
        a.source.cmp(&b.source).then_with(|| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        })
    });
    rows
}

fn resolve_local_plugin_path_by_name(name: &str) -> Option<PathBuf> {
    discover_plugin_surface(false)
        .into_iter()
        .filter_map(|row| {
            if row.name.eq_ignore_ascii_case(name) {
                row.path
            } else {
                None
            }
        })
        .next()
}

fn render_plugin_surface_table(rows: &[PluginSurfaceEntry]) -> String {
    if rows.is_empty() {
        return "  (no plugins discovered)".to_string();
    }
    let mut out = String::new();
    for row in rows {
        let status = if row.enabled { "enabled" } else { "disabled" };
        let mut meta_parts = vec![format!("source={}", row.source.label())];
        if let Some(kind) = row.kind.as_deref().filter(|k| !k.trim().is_empty()) {
            meta_parts.push(format!("kind={}", kind));
        }
        let path = row
            .path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        let version = if row.version.trim().is_empty() {
            "unknown".to_string()
        } else {
            row.version.clone()
        };
        let description = row.description.trim();
        let _ = writeln!(
            out,
            "  • {} v{} [{}; {}; path={}]",
            row.name,
            version,
            status,
            meta_parts.join(", "),
            path
        );
        if !description.is_empty() {
            let _ = writeln!(out, "    {}", description);
        }
    }
    out.trim_end().to_string()
}

fn set_plugin_enabled(path: &Path, enable: bool) -> Result<(), AgentError> {
    let marker = path.join(".disabled");
    if enable {
        if marker.exists() {
            std::fs::remove_file(&marker)
                .map_err(|e| AgentError::Io(format!("Failed to enable plugin: {}", e)))?;
        }
    } else {
        std::fs::write(&marker, "")
            .map_err(|e| AgentError::Io(format!("Failed to disable plugin: {}", e)))?;
    }
    Ok(())
}

fn parse_selection_indices(raw: &str, max: usize) -> Vec<usize> {
    let mut out = Vec::new();
    for token in raw.split(|c: char| c == ',' || c.is_ascii_whitespace()) {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(idx) = trimmed.parse::<usize>() else {
            continue;
        };
        if idx == 0 || idx > max {
            continue;
        }
        out.push(idx - 1);
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn run_plugins_interactive_toggle() -> Result<(), AgentError> {
    let mut rows: Vec<PluginSurfaceEntry> = discover_plugin_surface(false)
        .into_iter()
        .filter(|row| row.path.is_some())
        .collect();
    if rows.is_empty() {
        println!("No plugin bundles discovered.");
        println!("Install one with: hermes plugins install <owner/repo>  (or a trusted git URL)");
        return Ok(());
    }

    rows.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });

    println!("Plugin toggle UI (interactive)");
    println!("------------------------------");
    println!("Source roots:");
    println!(
        "  - user:    {}",
        hermes_config::hermes_home().join("plugins").display()
    );
    if hermes_config::env_var_enabled("HERMES_ENABLE_PROJECT_PLUGINS") {
        if let Ok(cwd) = std::env::current_dir() {
            println!(
                "  - project: {}",
                cwd.join(".hermes").join("plugins").display()
            );
        }
    } else {
        println!("  - project: disabled (set HERMES_ENABLE_PROJECT_PLUGINS=true)");
    }
    println!();

    let mut provider_indices = Vec::new();
    println!("General Plugins");
    for (idx, row) in rows.iter().enumerate() {
        let is_provider = row.kind.as_deref() == Some("exclusive");
        if is_provider {
            provider_indices.push(idx);
            continue;
        }
        let mark = if row.enabled { "✓" } else { " " };
        println!(
            "  {:>2}. [{}] {} (source={})",
            idx + 1,
            mark,
            row.name,
            row.source.label()
        );
    }

    if !provider_indices.is_empty() {
        println!();
        println!("Provider Plugins (single-select recommended)");
        for idx in &provider_indices {
            let row = &rows[*idx];
            let mark = if row.enabled { "✓" } else { " " };
            println!(
                "  {:>2}. [{}] {} (source={}, kind={})",
                idx + 1,
                mark,
                row.name,
                row.source.label(),
                row.kind.clone().unwrap_or_else(|| "provider".to_string())
            );
        }
    }

    use std::io::Write as _;
    print!("\nToggle plugin numbers (comma/space separated, Enter to skip): ");
    let _ = std::io::stdout().flush();
    let mut toggle_buf = String::new();
    std::io::stdin()
        .read_line(&mut toggle_buf)
        .map_err(|e| AgentError::Io(format!("Failed to read selection: {}", e)))?;
    let toggle_indices = parse_selection_indices(&toggle_buf, rows.len());
    for idx in toggle_indices {
        if let Some(path) = rows[idx].path.as_deref() {
            let target = !rows[idx].enabled;
            set_plugin_enabled(path, target)?;
            rows[idx].enabled = target;
        }
    }

    if !provider_indices.is_empty() {
        print!("Activate exactly one provider plugin number (Enter to keep current): ");
        let _ = std::io::stdout().flush();
        let mut provider_buf = String::new();
        std::io::stdin()
            .read_line(&mut provider_buf)
            .map_err(|e| AgentError::Io(format!("Failed to read provider selection: {}", e)))?;
        let selected = parse_selection_indices(&provider_buf, rows.len());
        if let Some(selected_idx) = selected.first().copied() {
            if provider_indices.contains(&selected_idx) {
                for idx in provider_indices {
                    if let Some(path) = rows[idx].path.as_deref() {
                        let should_enable = idx == selected_idx;
                        set_plugin_enabled(path, should_enable)?;
                        rows[idx].enabled = should_enable;
                    }
                }
            } else {
                println!(
                    "Selection {} is not a provider plugin row; keeping provider state unchanged.",
                    selected_idx + 1
                );
            }
        }
    }

    println!("\nUpdated plugin state:");
    println!("{}", render_plugin_surface_table(&rows));
    Ok(())
}

pub async fn handle_cli_external_plugin_subcommand(raw: Vec<String>) -> Result<(), AgentError> {
    if raw.is_empty() {
        return Err(AgentError::Config(
            "Unknown command. Run `hermes --help` for available commands.".to_string(),
        ));
    }
    let command_name = raw[0].trim().to_string();
    Err(AgentError::Config(format!(
        "Unknown command '{}'. Run `hermes --help` for Rust-native commands. Python plugin command dispatch is disabled in Hermes Agent Ultra's Rust-only runtime.",
        command_name
    )))
}

// ---------------------------------------------------------------------------
// Plugin security (remote Git installs)
// ---------------------------------------------------------------------------

fn default_git_host_allowlist() -> Vec<&'static str> {
    vec![
        "github.com",
        "www.github.com",
        "raw.githubusercontent.com",
        "gitlab.com",
        "www.gitlab.com",
        "codeberg.org",
        "www.codeberg.org",
        "gitea.com",
        "bitbucket.org",
    ]
}

fn plugin_git_host_allowed(url: &str, allow_untrusted: bool) -> bool {
    if allow_untrusted {
        return true;
    }
    let extra = std::env::var("HERMES_PLUGIN_GIT_EXTRA_HOSTS").unwrap_or_default();
    let mut hosts: Vec<String> = default_git_host_allowlist()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    for part in extra.split(',') {
        let p = part.trim();
        if !p.is_empty() {
            hosts.push(p.to_lowercase());
        }
    }
    let lower = url.to_lowercase();
    let host_part = if lower.contains("://") {
        lower.split("://").nth(1).unwrap_or("")
    } else if lower.starts_with("git@") {
        lower
            .trim_start_matches("git@")
            .split(':')
            .next()
            .unwrap_or("")
    } else {
        return false;
    };
    let host = host_part
        .split('/')
        .next()
        .unwrap_or(host_part)
        .split('@')
        .next_back()
        .unwrap_or(host_part);
    let host = host.split(':').next().unwrap_or(host).to_lowercase();
    hosts
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{}", h)))
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// Static scan of a cloned plugin tree: risky patterns in scripts/config.
fn scan_plugin_security(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let manifest = root.join("plugin.yaml");
    if manifest.exists() {
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            if text.contains("post_install") || text.contains("postInstall") {
                out.push(
                    "plugin.yaml declares post_install / postInstall — review before running the plugin"
                        .into(),
                );
            }
            if Regex::new(r"(?i)curl\s+[^|\n]*\|\s*(ba)?sh")
                .ok()
                .and_then(|re| re.find(&text))
                .is_some()
            {
                out.push("plugin.yaml references curl|sh style install — high risk".into());
            }
        }
    }

    let risky_file_patterns: &[(&str, &[(&str, &str)])] = &[(
        r"\.(sh|bash|zsh|py|rb|ps1|fish)$",
        &[
            (r"(?i)\bcurl\s+[^|\n]*\|\s*(ba)?sh", "curl piped to shell"),
            (r"(?i)\bwget\s+[^|\n]*\|\s*(ba)?sh", "wget piped to shell"),
            (r"(?i)\beval\s*\(", "eval("),
            (r"(?i)\bexec\s*\(", "exec("),
            (r"(?i)(base64[._-]?decode|atob)\s*\(", "base64 decode"),
            (r"(?i)\brm\s+-rf\s+/", "rm -rf on absolute path"),
        ],
    )];

    fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if dir.is_dir() && (name == ".git" || name == "target" || name == "node_modules") {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, files);
                } else if p.is_file() {
                    files.push(p);
                }
            }
        }
    }

    let mut files = Vec::new();
    walk(root, &mut files);

    for fp in files {
        let fname = fp.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if fname == ".DS_Store" {
            continue;
        }
        let rel = fp.strip_prefix(root).unwrap_or(&fp).display().to_string();
        let Ok(content) = std::fs::read_to_string(&fp) else {
            continue;
        };
        for (ext_re, rules) in risky_file_patterns {
            if let Ok(re_ext) = Regex::new(ext_re) {
                if !re_ext.is_match(fname) {
                    continue;
                }
                for (pat, label) in *rules {
                    if let Ok(re) = Regex::new(pat) {
                        if re.is_match(&content) {
                            out.push(format!("{}: {}", rel, label));
                        }
                    }
                }
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

async fn git_checkout_ref(repo_dir: &std::path::Path, git_ref: &str) -> Result<(), String> {
    let dir = repo_dir.to_string_lossy().to_string();
    let fetch = tokio::process::Command::new("git")
        .args(["-C", &dir, "fetch", "--depth", "1", "origin", git_ref])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !fetch.status.success() {
        let err = String::from_utf8_lossy(&fetch.stderr);
        return Err(format!("git fetch origin {}: {}", git_ref, err.trim()));
    }
    let co = tokio::process::Command::new("git")
        .args(["-C", &dir, "checkout", git_ref])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !co.status.success() {
        let err = String::from_utf8_lossy(&co.stderr);
        return Err(format!("git checkout {}: {}", git_ref, err.trim()));
    }
    Ok(())
}

/// Handle `hermes plugins [action] [name]`.
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

fn prompt_memory_setup_value(
    label: &str,
    default: Option<&str>,
    yes: bool,
) -> Result<String, AgentError> {
    if yes {
        return Ok(default.unwrap_or_default().to_string());
    }
    match default {
        Some(value) if !value.is_empty() && memory_setup_label_is_secret(label) => {
            print!("{label} [set]: ");
        }
        Some(value) if !value.is_empty() => {
            print!("{label} [{value}]: ");
        }
        _ => {
            print!("{label}: ");
        }
    }
    std::io::stdout()
        .flush()
        .map_err(|e| AgentError::Io(format!("flush stdout: {e}")))?;
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| AgentError::Io(format!("read setup input: {e}")))?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default.unwrap_or_default().to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn memory_setup_label_is_secret(label: &str) -> bool {
    let lower = label.to_ascii_lowercase();
    lower.contains("api key")
        || lower.contains("jwt")
        || lower.contains("token")
        || lower.contains("secret")
}

fn active_honcho_host_key_for_cli() -> String {
    if let Ok(explicit) = std::env::var("HERMES_HONCHO_HOST") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return explicit.to_string();
        }
    }
    let profile = std::env::var("HERMES_PROFILE").unwrap_or_default();
    let profile = profile.trim();
    if profile.is_empty() || matches!(profile, "default" | "custom") {
        "hermes".to_string()
    } else {
        let sanitized = profile
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .to_string();
        format!(
            "hermes_{}",
            if sanitized.is_empty() {
                "profile"
            } else {
                sanitized.as_str()
            }
        )
    }
}

fn legacy_honcho_host_key_for_cli(host: &str) -> Option<String> {
    let suffix = host.strip_prefix("hermes_")?;
    if suffix.trim().is_empty() {
        None
    } else {
        Some(format!("hermes.{suffix}"))
    }
}

fn honcho_host_value_has_oauth_grant(block: &serde_json::Value) -> bool {
    let Some(api_key) = block.get("apiKey").and_then(serde_json::Value::as_str) else {
        return false;
    };
    if !api_key.starts_with("hch-at-") {
        return false;
    }
    let Some(oauth) = block.get("oauth").and_then(serde_json::Value::as_object) else {
        return false;
    };
    ["refreshToken", "clientId", "tokenEndpoint"]
        .iter()
        .all(|key| {
            oauth
                .get(*key)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
}

fn honcho_config_has_oauth_grant(path: &Path, host: &str) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(hosts) = parsed.get("hosts").and_then(serde_json::Value::as_object) else {
        return honcho_host_value_has_oauth_grant(&parsed);
    };
    hosts
        .get(host)
        .or_else(|| {
            legacy_honcho_host_key_for_cli(host)
                .as_deref()
                .and_then(|legacy| hosts.get(legacy))
        })
        .is_some_and(honcho_host_value_has_oauth_grant)
}

fn honcho_ai_peer_for_host(host: &str) -> String {
    host.strip_prefix("hermes.")
        .or_else(|| host.strip_prefix("hermes_"))
        .filter(|profile| !profile.trim().is_empty())
        .unwrap_or(host)
        .to_string()
}

fn parse_honcho_aliases(raw: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut aliases = serde_json::Map::new();
    for entry in raw.split(',') {
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if !key.is_empty() && !value.is_empty() {
            aliases.insert(
                key.to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }
    aliases
}

struct HonchoSetupConfigInput<'a> {
    host: &'a str,
    deployment: &'a str,
    api_key: &'a str,
    base_url: &'a str,
    peer_name: &'a str,
    shape: &'a str,
    runtime_peer_prefix: &'a str,
    aliases: &'a serde_json::Map<String, serde_json::Value>,
}

fn build_honcho_setup_config(input: HonchoSetupConfigInput<'_>) -> serde_json::Value {
    let mut root = serde_json::Map::new();
    let mut host_block = serde_json::Map::new();

    root.insert("enabled".to_string(), serde_json::Value::Bool(true));
    if input.deployment == "local" {
        root.insert(
            "baseUrl".to_string(),
            serde_json::Value::String(input.base_url.to_string()),
        );
        if !input.api_key.trim().is_empty() {
            host_block.insert(
                "apiKey".to_string(),
                serde_json::Value::String(input.api_key.to_string()),
            );
        }
    } else if !input.api_key.trim().is_empty() {
        root.insert(
            "apiKey".to_string(),
            serde_json::Value::String(input.api_key.to_string()),
        );
    }

    host_block.insert("enabled".to_string(), serde_json::Value::Bool(true));
    host_block.insert(
        "workspace".to_string(),
        serde_json::Value::String("hermes".to_string()),
    );
    host_block.insert(
        "aiPeer".to_string(),
        serde_json::Value::String(honcho_ai_peer_for_host(input.host)),
    );
    if !input.peer_name.trim().is_empty() {
        host_block.insert(
            "peerName".to_string(),
            serde_json::Value::String(input.peer_name.to_string()),
        );
    }

    match input.shape {
        "single" => {
            host_block.insert("pinUserPeer".to_string(), serde_json::Value::Bool(true));
        }
        "hybrid" => {
            host_block.insert("pinUserPeer".to_string(), serde_json::Value::Bool(false));
            if !input.aliases.is_empty() {
                host_block.insert(
                    "userPeerAliases".to_string(),
                    serde_json::Value::Object(input.aliases.clone()),
                );
            }
            if !input.runtime_peer_prefix.trim().is_empty() {
                host_block.insert(
                    "runtimePeerPrefix".to_string(),
                    serde_json::Value::String(input.runtime_peer_prefix.to_string()),
                );
            }
        }
        _ => {
            host_block.insert("pinUserPeer".to_string(), serde_json::Value::Bool(false));
            if !input.runtime_peer_prefix.trim().is_empty() {
                host_block.insert(
                    "runtimePeerPrefix".to_string(),
                    serde_json::Value::String(input.runtime_peer_prefix.to_string()),
                );
            }
        }
    }

    let mut hosts = serde_json::Map::new();
    hosts.insert(
        input.host.to_string(),
        serde_json::Value::Object(host_block),
    );
    root.insert("hosts".to_string(), serde_json::Value::Object(hosts));
    serde_json::Value::Object(root)
}

fn setup_mem0_provider(yes: bool) -> Result<PathBuf, AgentError> {
    let api_key_default = std::env::var("MEM0_API_KEY").unwrap_or_default();
    let user_id_default =
        std::env::var("MEM0_USER_ID").unwrap_or_else(|_| "hermes-user".to_string());
    let agent_id_default = std::env::var("MEM0_AGENT_ID").unwrap_or_else(|_| "hermes".to_string());
    let base_url_default =
        std::env::var("MEM0_BASE_URL").unwrap_or_else(|_| "https://api.mem0.ai/v1".to_string());

    let api_key = prompt_memory_setup_value("Mem0 API key", Some(&api_key_default), yes)?;
    if api_key.trim().is_empty() {
        return Err(AgentError::Config(
            "Mem0 setup requires MEM0_API_KEY or an API key entered at the prompt.".into(),
        ));
    }
    let user_id = prompt_memory_setup_value("Mem0 user_id", Some(&user_id_default), yes)?;
    let agent_id = prompt_memory_setup_value("Mem0 agent_id", Some(&agent_id_default), yes)?;
    let base_url = prompt_memory_setup_value("Mem0 base_url", Some(&base_url_default), yes)?;

    let config = serde_json::json!({
        "api_key": api_key,
        "user_id": user_id,
        "agent_id": agent_id,
        "base_url": base_url,
        "rerank": true
    });
    hermes_agent::memory_plugins::mem0::Mem0MemoryPlugin::new()
        .save_config(&config)
        .map_err(AgentError::Config)?;
    Ok(hermes_config::hermes_home().join("mem0.json"))
}

fn setup_honcho_provider(yes: bool) -> Result<PathBuf, AgentError> {
    let env_api_key = std::env::var("HONCHO_API_KEY").unwrap_or_default();
    let env_base_url = std::env::var("HONCHO_BASE_URL").unwrap_or_default();
    let default_deployment =
        if env_base_url.trim().is_empty() || env_base_url.contains("api.honcho.dev") {
            "cloud"
        } else {
            "local"
        };
    let deployment = prompt_memory_setup_value(
        "Honcho deployment (cloud|local)",
        Some(default_deployment),
        yes,
    )?
    .to_ascii_lowercase();
    let deployment = if deployment == "local" {
        "local"
    } else {
        "cloud"
    };
    let host = active_honcho_host_key_for_cli();
    let existing_oauth_grant =
        honcho_config_has_oauth_grant(&hermes_config::hermes_home().join("honcho.json"), &host);

    let base_url_default = if deployment == "local" {
        if env_base_url.trim().is_empty() {
            "http://localhost:8000".to_string()
        } else {
            env_base_url.clone()
        }
    } else {
        env_base_url.clone()
    };
    let base_url = if deployment == "local" {
        prompt_memory_setup_value("Honcho local baseUrl", Some(&base_url_default), yes)?
    } else {
        base_url_default
    };
    let api_label = if deployment == "local" {
        "Honcho local JWT/API key (blank for no-auth local)"
    } else {
        "Honcho API key"
    };
    let api_key = prompt_memory_setup_value(api_label, Some(&env_api_key), yes)?;
    if deployment == "cloud" && api_key.trim().is_empty() && !existing_oauth_grant {
        return Err(AgentError::Config(
            "Honcho cloud setup requires HONCHO_API_KEY or an API key entered at the prompt."
                .into(),
        ));
    }

    let peer_default = std::env::var("HERMES_USER").unwrap_or_default();
    let peer_name = prompt_memory_setup_value("Honcho peerName", Some(&peer_default), yes)?;
    let shape_input = prompt_memory_setup_value(
        "Deployment shape (single|multi|hybrid)",
        Some("single"),
        yes,
    )?
    .to_ascii_lowercase();
    let shape = match shape_input.as_str() {
        "single" | "hybrid" => shape_input,
        _ => "multi".to_string(),
    };
    let runtime_peer_prefix = if shape == "multi" || shape == "hybrid" {
        prompt_memory_setup_value("Runtime peer prefix", Some(""), yes)?
    } else {
        String::new()
    };
    let alias_raw = if shape == "hybrid" {
        prompt_memory_setup_value(
            "Runtime aliases (comma key=peer, blank for none)",
            Some(""),
            yes,
        )?
    } else {
        String::new()
    };
    let aliases = parse_honcho_aliases(&alias_raw);
    let config = build_honcho_setup_config(HonchoSetupConfigInput {
        host: &host,
        deployment,
        api_key: &api_key,
        base_url: &base_url,
        peer_name: &peer_name,
        shape: &shape,
        runtime_peer_prefix: &runtime_peer_prefix,
        aliases: &aliases,
    });

    hermes_agent::memory_plugins::honcho::HonchoMemoryPlugin::new()
        .save_config(&config)
        .map_err(AgentError::Config)?;
    Ok(hermes_config::hermes_home().join("honcho.json"))
}

fn normalize_openviking_setup_endpoint(raw: &str) -> String {
    let trimmed = raw.trim();
    let endpoint = if trimmed.is_empty() {
        "http://127.0.0.1:1933".to_string()
    } else if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    endpoint.trim_end_matches('/').to_string()
}

fn openviking_setup_endpoint_is_local(endpoint: &str) -> bool {
    endpoint.starts_with("http://127.0.0.1:")
        || endpoint.starts_with("http://localhost:")
        || endpoint == "http://127.0.0.1"
        || endpoint == "http://localhost"
}

fn normalize_openviking_setup_key_type(raw: &str, endpoint: &str, api_key: &str) -> String {
    let normalized = match raw.trim().to_ascii_lowercase().as_str() {
        "root" | "root_api_key" | "root-api-key" => "root",
        "none" | "dev" | "local" | "no_api_key" | "no-api-key" => "none",
        "user" | "user_api_key" | "user-api-key" => "user",
        "" if openviking_setup_endpoint_is_local(endpoint) && api_key.trim().is_empty() => "none",
        _ => "user",
    };
    normalized.to_string()
}

struct OpenVikingSetupConfigInput<'a> {
    endpoint: &'a str,
    api_key: &'a str,
    api_key_type: &'a str,
    account: &'a str,
    user: &'a str,
    agent: &'a str,
}

fn build_openviking_setup_config(
    input: OpenVikingSetupConfigInput<'_>,
) -> Result<serde_json::Value, AgentError> {
    let endpoint = normalize_openviking_setup_endpoint(input.endpoint);
    let api_key_type =
        normalize_openviking_setup_key_type(input.api_key_type, &endpoint, input.api_key);
    let api_key = input.api_key.trim();
    if api_key_type != "none" && api_key.is_empty() {
        return Err(AgentError::Config(format!(
            "OpenViking {api_key_type} setup requires an API key."
        )));
    }
    let account = input.account.trim();
    let user = input.user.trim();
    if api_key_type == "root" && (account.is_empty() || user.is_empty()) {
        return Err(AgentError::Config(
            "OpenViking root API key setup requires account and user.".into(),
        ));
    }
    let account = if account.is_empty() {
        "default"
    } else {
        account
    };
    let user = if user.is_empty() { "default" } else { user };
    let agent = if input.agent.trim().is_empty() {
        "hermes"
    } else {
        input.agent.trim()
    };

    Ok(serde_json::json!({
        "enabled": true,
        "endpoint": endpoint,
        "api_key": api_key,
        "api_key_type": api_key_type,
        "account": account,
        "user": user,
        "agent": agent,
        "setup_mode": "manual"
    }))
}

fn setup_openviking_provider(yes: bool) -> Result<PathBuf, AgentError> {
    let endpoint_default = std::env::var("OPENVIKING_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:1933".to_string());
    let endpoint = normalize_openviking_setup_endpoint(&prompt_memory_setup_value(
        "OpenViking server URL",
        Some(&endpoint_default),
        yes,
    )?);
    let api_key_default = std::env::var("OPENVIKING_API_KEY").unwrap_or_default();
    let key_type_default = std::env::var("OPENVIKING_API_KEY_TYPE").unwrap_or_else(|_| {
        if openviking_setup_endpoint_is_local(&endpoint) && api_key_default.trim().is_empty() {
            "none".to_string()
        } else {
            "user".to_string()
        }
    });
    let key_type = normalize_openviking_setup_key_type(
        &prompt_memory_setup_value(
            "OpenViking API key type (none|user|root)",
            Some(&key_type_default),
            yes,
        )?,
        &endpoint,
        &api_key_default,
    );
    let api_key = if key_type == "none" {
        String::new()
    } else {
        let label = if key_type == "root" {
            "OpenViking root API key"
        } else {
            "OpenViking user API key"
        };
        prompt_memory_setup_value(label, Some(&api_key_default), yes)?
    };
    let account_default = std::env::var("OPENVIKING_ACCOUNT").unwrap_or_else(|_| "default".into());
    let user_default = std::env::var("OPENVIKING_USER").unwrap_or_else(|_| "default".into());
    let account = if key_type == "root" || key_type == "none" {
        prompt_memory_setup_value("OpenViking account", Some(&account_default), yes)?
    } else {
        account_default
    };
    let user = if key_type == "root" || key_type == "none" {
        prompt_memory_setup_value("OpenViking user", Some(&user_default), yes)?
    } else {
        user_default
    };
    let agent_default = std::env::var("OPENVIKING_AGENT").unwrap_or_else(|_| "hermes".into());
    let agent = prompt_memory_setup_value("OpenViking agent", Some(&agent_default), yes)?;
    let config = build_openviking_setup_config(OpenVikingSetupConfigInput {
        endpoint: &endpoint,
        api_key: &api_key,
        api_key_type: &key_type,
        account: &account,
        user: &user,
        agent: &agent,
    })?;

    hermes_agent::memory_plugins::openviking::OpenVikingMemoryPlugin::new()
        .save_config(&config)
        .map_err(AgentError::Config)?;
    Ok(hermes_config::hermes_home().join("openviking.json"))
}

fn setup_memory_provider_target(provider: &str, yes: bool) -> Result<PathBuf, AgentError> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "mem0" => setup_mem0_provider(yes),
        "honcho" => setup_honcho_provider(yes),
        "openviking" | "ov" => setup_openviking_provider(yes),
        other => Err(AgentError::Config(format!(
            "Unsupported memory provider setup target '{other}'. Supported: honcho, mem0, openviking"
        ))),
    }
}

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

/// Handle `hermes mcp [action] [--server ...]`.
pub async fn handle_cli_mcp(
    action: Option<String>,
    name: Option<String>,
    server: Option<String>,
    url: Option<String>,
    command: Option<String>,
    parallel_tools: bool,
) -> Result<(), hermes_core::AgentError> {
    let config_dir = hermes_config::hermes_home();
    let mcp_config_path = config_dir.join("mcp_servers.json");
    let mcp_auth_path = config_dir.join("mcp_auth.json");
    let selected = name.clone().or(server.clone());

    match action.as_deref().unwrap_or("list") {
        "sentrux" | "setup-sentrux" | "sentrux-setup" => {
            let sentrux_present = upsert_sentrux_mcp_profile(&config_dir)?;
            if sentrux_present {
                println!(
                    "Detected '{}' on PATH. Configuring {} MCP profile...",
                    SENTRUX_MCP_COMMAND, SENTRUX_MCP_SERVER_NAME
                );
            } else {
                println!(
                    "Warning: '{}' is not currently on PATH. Adding MCP config anyway.",
                    SENTRUX_MCP_COMMAND
                );
                println!(
                    "Install sentrux, then run `hermes mcp test {}` to verify transport reachability.",
                    SENTRUX_MCP_SERVER_NAME
                );
            }

            println!(
                "Configured MCP server '{}' in:\n  - {}\n  - {}",
                SENTRUX_MCP_SERVER_NAME,
                mcp_config_path.display(),
                config_dir.join("config.yaml").display()
            );
            println!(
                "Runtime hint: use `/mcp` in-session to confirm, and `hermes mcp test {}` for transport checks.",
                SENTRUX_MCP_SERVER_NAME
            );
        }
        "sentrux-status" => {
            let (binary_on_path, from_json, from_yaml) = sentrux_mcp_status(&config_dir);
            println!(
                "Sentrux MCP status:\n  - binary_on_path: {}\n  - in_mcp_servers.json: {}\n  - in_config.yaml: {}",
                if binary_on_path { "yes" } else { "no" },
                yes_no(from_json),
                yes_no(from_yaml)
            );
        }
        "sentrux-remove" => {
            remove_sentrux_mcp_profile(&config_dir)?;
            println!(
                "Removed '{}' MCP profile from JSON + YAML config surfaces.",
                SENTRUX_MCP_SERVER_NAME
            );
        }
        "unreal-engine" | "setup-unreal" | "unreal-setup" => {
            upsert_unreal_mcp_profile(&config_dir)?;
            println!(
                "Configured MCP server '{}' in:\n  - {}\n  - {}",
                UNREAL_MCP_SERVER_NAME,
                mcp_config_path.display(),
                config_dir.join("config.yaml").display()
            );
            println!(
                "Before connecting: open Unreal Editor 5.8+, enable Epic's Unreal MCP plugin, and start its local server at {}.",
                UNREAL_MCP_URL
            );
            println!(
                "Runtime hint: use `/mcp` in-session to confirm, and `hermes mcp test {}` after the editor server is running.",
                UNREAL_MCP_SERVER_NAME
            );
        }
        "unreal-engine-status" => {
            let (from_json, from_yaml) = unreal_mcp_status(&config_dir);
            println!(
                "Unreal Engine MCP status:\n  - url: {}\n  - in_mcp_servers.json: {}\n  - in_config.yaml: {}\n  - parallel_tool_calls: off",
                UNREAL_MCP_URL,
                yes_no(from_json),
                yes_no(from_yaml)
            );
        }
        "unreal-engine-remove" => {
            remove_unreal_mcp_profile(&config_dir)?;
            println!(
                "Removed '{}' MCP profile from JSON + YAML config surfaces.",
                UNREAL_MCP_SERVER_NAME
            );
        }
        "list" => {
            let Some(config) = load_mcp_config_if_exists(&mcp_config_path)? else {
                println!("No MCP servers configured ({})", mcp_config_path.display());
                println!("Add one with `hermes mcp add --server <name-or-url>`.");
                return Ok(());
            };
            if config.servers.is_empty() {
                println!("No MCP servers configured.");
            } else {
                for warning in config.warnings() {
                    println!("Warning: {warning}");
                }
                println!("MCP servers ({}):", mcp_config_path.display());
                for entry in &config.servers {
                    println!(
                        "  • {} — {}  [{}; enabled:{}; parallel_tool_calls:{}; keepalive:{}]",
                        entry.name,
                        entry.transport_display(),
                        entry.transport_kind().as_str(),
                        if entry.enabled { "on" } else { "off" },
                        if entry.supports_parallel_tool_calls {
                            "on"
                        } else {
                            "off"
                        },
                        entry
                            .keepalive_interval
                            .map(|secs| format!("{secs}s"))
                            .unwrap_or_else(|| "default".to_string())
                    );
                }
            }
        }
        "add" => {
            let (entry_name, entry, yaml_command, yaml_url, yaml_parallel) = if let Some(name) =
                name.as_deref().map(str::trim).filter(|s| !s.is_empty())
            {
                let entry = if let Some(url) = url.clone().filter(|v| !v.trim().is_empty()) {
                    serde_json::json!({
                        "url": url,
                        "enabled": true,
                        "supports_parallel_tool_calls": parallel_tools
                    })
                } else if let Some(command) = command.clone().filter(|v| !v.trim().is_empty()) {
                    serde_json::json!({
                        "command": command,
                        "enabled": true,
                        "supports_parallel_tool_calls": parallel_tools
                    })
                } else {
                    return Err(hermes_core::AgentError::Config(
                        "mcp add with positional name requires --url or --command".into(),
                    ));
                };
                (
                    name.to_string(),
                    entry,
                    command.clone().filter(|v| !v.trim().is_empty()),
                    url.clone().filter(|v| !v.trim().is_empty()),
                    parallel_tools,
                )
            } else {
                let srv = server
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing server. Usage: hermes mcp add <name> --url <url> | --command <cmd> [--parallel-tools] (legacy: --server <name-or-url>)".into(),
                        )
                    })?;
                let entry = serde_json::json!({
                    "url": srv,
                    "enabled": true,
                    "supports_parallel_tool_calls": parallel_tools
                });
                let yaml_url = Some(srv.to_string());
                (srv.to_string(), entry, None, yaml_url, parallel_tools)
            };
            println!("Adding MCP server: {}", entry_name);
            let mut servers: serde_json::Value = if mcp_config_path.exists() {
                let content = std::fs::read_to_string(&mcp_config_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
            } else {
                serde_json::json!({})
            };
            if let Some(obj) = servers.as_object_mut() {
                obj.insert(entry_name.clone(), entry);
            }
            let json = serde_json::to_string_pretty(&servers)
                .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
            std::fs::write(&mcp_config_path, json)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            update_yaml_mcp_server(
                &config_dir,
                &entry_name,
                yaml_command,
                yaml_url,
                yaml_parallel,
                None,
                false,
            )?;
            println!(
                "MCP server '{}' added to {}",
                entry_name,
                mcp_config_path.display()
            );
            println!(
                "Synced MCP server '{}' into {}",
                entry_name,
                config_dir.join("config.yaml").display()
            );
        }
        "remove" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp remove <name>".into(),
                )
            })?;
            if !mcp_config_path.exists() {
                println!("No MCP config to modify.");
                return Ok(());
            }
            let content = std::fs::read_to_string(&mcp_config_path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let mut servers: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            if let Some(obj) = servers.as_object_mut() {
                if obj.remove(&srv).is_some() {
                    let json = serde_json::to_string_pretty(&servers)
                        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
                    std::fs::write(&mcp_config_path, json)
                        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                    update_yaml_mcp_server(&config_dir, &srv, None, None, false, None, true)?;
                    println!("MCP server '{}' removed.", srv);
                    if mcp_auth_path.exists() {
                        let raw = std::fs::read_to_string(&mcp_auth_path).unwrap_or_default();
                        let mut auth: serde_json::Value =
                            serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
                        if let Some(auth_obj) = auth.as_object_mut() {
                            auth_obj.remove(&srv);
                            let out = serde_json::to_string_pretty(&auth).unwrap_or_default();
                            let _ = std::fs::write(&mcp_auth_path, out);
                        }
                    }
                } else {
                    println!("MCP server '{}' not found.", srv);
                }
            }
        }
        "serve" => {
            use hermes_skills::{FileSkillStore, SkillManager};
            use hermes_tools::ToolRegistry;

            eprintln!("Starting Hermes as MCP server on stdio...");

            let config = hermes_config::load_config(None)
                .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
            let tool_registry = Arc::new(ToolRegistry::new());
            let terminal_backend = crate::terminal_backend::build_terminal_backend(&config);
            let skill_store = Arc::new(FileSkillStore::new(hermes_config::skills_dir()));
            let skill_provider: Arc<dyn hermes_core::SkillProvider> =
                Arc::new(SkillManager::new(skill_store));
            hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);

            let mcp_server = hermes_mcp::McpServer::new(tool_registry);
            let transport = Box::new(hermes_mcp::ServerStdioTransport::new());
            mcp_server
                .start(transport)
                .await
                .map_err(|e| hermes_core::AgentError::Io(format!("MCP server error: {}", e)))?;
        }
        "test" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp test <name>".into(),
                )
            })?;
            println!("Testing MCP server: {}...", srv);
            let Some(config) = load_mcp_config_if_exists(&mcp_config_path)? else {
                println!("No MCP config found.");
                return Ok(());
            };
            match config.get(&srv) {
                Some(entry) => {
                    for warning in &entry.warnings {
                        println!("Warning: {warning}");
                    }
                    println!("  Server: {}", srv);
                    println!("  Transport: {}", entry.transport_kind().as_str());
                    println!("  Target: {}", entry.transport_display());
                    println!("  Enabled: {}", entry.enabled);
                    println!(
                        "  Parallel tool calls: {}",
                        if entry.supports_parallel_tool_calls {
                            "on"
                        } else {
                            "off"
                        }
                    );
                    println!(
                        "  Keepalive interval: {}",
                        entry
                            .keepalive_interval
                            .map(|secs| format!("{secs}s"))
                            .unwrap_or_else(|| "default".to_string())
                    );
                    if entry.transport_kind() == McpTransportKind::Http {
                        let url = entry.url.as_deref().unwrap_or_default();
                        match reqwest::Client::new()
                            .get(url)
                            .timeout(std::time::Duration::from_secs(5))
                            .send()
                            .await
                        {
                            Ok(resp) => println!("  Status: {} (reachable)", resp.status()),
                            Err(e) => println!("  Status: unreachable ({})", e),
                        }
                    } else {
                        println!("  Status: stdio transport (not testable via HTTP)");
                    }
                }
                None => println!("Server '{}' not found in MCP config.", srv),
            }
        }
        "configure" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp configure <name>".into(),
                )
            })?;
            let Some(config) = load_mcp_config_if_exists(&mcp_config_path)? else {
                println!("No MCP config found. Add a server first with `hermes mcp add`.");
                return Ok(());
            };
            match config.get(&srv) {
                Some(entry) => {
                    for warning in &entry.warnings {
                        println!("Warning: {warning}");
                    }
                    println!("Current config for '{}':", srv);
                    println!(
                        "{}",
                        serde_json::to_string_pretty(entry).unwrap_or_default()
                    );
                    println!("\nEdit {} to modify settings.", mcp_config_path.display());
                }
                None => println!("Server '{}' not found.", srv),
            }
        }
        "login" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp login <name>".into(),
                )
            })?;
            if !mcp_config_path.exists() {
                return Err(hermes_core::AgentError::Config(format!(
                    "No MCP config found at {}",
                    mcp_config_path.display()
                )));
            }
            let configured = load_mcp_config(&mcp_config_path)?.get(&srv).is_some();
            if !configured {
                return Err(hermes_core::AgentError::Config(format!(
                    "MCP server '{}' is not configured",
                    srv
                )));
            }

            let env_key = format!("MCP_{}_TOKEN", srv.to_uppercase().replace('-', "_"));
            let token_from_env = std::env::var(&env_key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            let token = if let Some(v) = token_from_env {
                v
            } else {
                use std::io::{self, Write};
                print!("Token for '{}': ", srv);
                let _ = io::stdout().flush();
                let mut buf = String::new();
                io::stdin()
                    .read_line(&mut buf)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                buf.trim().to_string()
            };
            if token.is_empty() {
                return Err(hermes_core::AgentError::Config(
                    "Empty token; aborting mcp login".into(),
                ));
            }
            let mut auth: serde_json::Value = if mcp_auth_path.exists() {
                let raw = std::fs::read_to_string(&mcp_auth_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
            } else {
                serde_json::json!({})
            };
            if let Some(obj) = auth.as_object_mut() {
                obj.insert(
                    srv.clone(),
                    serde_json::json!({
                        "token": token,
                        "updated_at": chrono::Utc::now().to_rfc3339(),
                    }),
                );
            }
            std::fs::write(
                &mcp_auth_path,
                serde_json::to_string_pretty(&auth)
                    .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?,
            )
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!(
                "Stored MCP auth token for '{}' in {}",
                srv,
                mcp_auth_path.display()
            );
        }
        other => {
            println!("MCP action '{}' is not recognized.", other);
            println!(
                "Available actions: list, add, remove, serve, test, configure, login, sentrux, sentrux-status, sentrux-remove, unreal-engine, unreal-engine-status, unreal-engine-remove"
            );
        }
    }
    Ok(())
}

fn command_on_path(command: &str) -> bool {
    if command.trim().is_empty() {
        return false;
    }
    if is_node_family_command(command) && find_node_executable(command).is_some() {
        return true;
    }
    let candidate = Path::new(command);
    if candidate.components().count() > 1 {
        return candidate.exists();
    }
    std::env::var_os("PATH").is_some_and(|path_var| {
        std::env::split_paths(&path_var)
            .map(|p| p.join(command))
            .any(|p| p.exists())
    })
}

fn is_node_family_command(command: &str) -> bool {
    let base = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .trim()
        .to_ascii_lowercase();
    matches!(
        base.as_str(),
        "node" | "node.exe" | "npm" | "npm.cmd" | "npm.exe" | "npx" | "npx.cmd" | "npx.exe"
    )
}

fn sentrux_entry() -> serde_json::Value {
    serde_json::json!({
        "command": SENTRUX_MCP_COMMAND,
        "args": [SENTRUX_MCP_ARG],
        "enabled": true,
        "supports_parallel_tool_calls": true
    })
}

fn unreal_mcp_entry() -> serde_json::Value {
    serde_json::json!({
        "url": UNREAL_MCP_URL,
        "enabled": true,
        "supports_parallel_tool_calls": false,
        "keepalive_interval": 10
    })
}

fn update_yaml_mcp_server(
    config_dir: &Path,
    name: &str,
    command: Option<String>,
    url: Option<String>,
    supports_parallel_tool_calls: bool,
    keepalive_interval: Option<u64>,
    remove: bool,
) -> Result<(), hermes_core::AgentError> {
    let cfg_path = config_dir.join("config.yaml");
    let mut cfg = hermes_config::load_user_config_file(&cfg_path)
        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
    cfg.mcp_servers.retain(|entry| entry.name != name);
    if !remove {
        cfg.mcp_servers.push(hermes_config::McpServerEntry {
            name: name.to_string(),
            command,
            url,
            supports_parallel_tool_calls,
            keepalive_interval,
        });
        cfg.mcp_servers.sort_by(|a, b| a.name.cmp(&b.name));
    }
    hermes_config::save_config_yaml(&cfg_path, &cfg)
        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))
}

fn upsert_sentrux_mcp_profile(config_dir: &Path) -> Result<bool, hermes_core::AgentError> {
    let mcp_config_path = config_dir.join("mcp_servers.json");
    let mut servers: serde_json::Value = if mcp_config_path.exists() {
        let content = std::fs::read_to_string(&mcp_config_path)
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    if let Some(obj) = servers.as_object_mut() {
        obj.insert(SENTRUX_MCP_SERVER_NAME.to_string(), sentrux_entry());
    }
    let json = serde_json::to_string_pretty(&servers)
        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
    std::fs::write(&mcp_config_path, json)
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    update_yaml_mcp_server(
        config_dir,
        SENTRUX_MCP_SERVER_NAME,
        Some(format!("{SENTRUX_MCP_COMMAND} {SENTRUX_MCP_ARG}")),
        None,
        true,
        None,
        false,
    )?;
    Ok(command_on_path(SENTRUX_MCP_COMMAND))
}

fn upsert_unreal_mcp_profile(config_dir: &Path) -> Result<(), hermes_core::AgentError> {
    let mcp_config_path = config_dir.join("mcp_servers.json");
    let mut servers: serde_json::Value = if mcp_config_path.exists() {
        let content = std::fs::read_to_string(&mcp_config_path)
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    if let Some(obj) = servers.as_object_mut() {
        obj.insert(UNREAL_MCP_SERVER_NAME.to_string(), unreal_mcp_entry());
    }
    let json = serde_json::to_string_pretty(&servers)
        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
    std::fs::write(&mcp_config_path, json)
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    update_yaml_mcp_server(
        config_dir,
        UNREAL_MCP_SERVER_NAME,
        None,
        Some(UNREAL_MCP_URL.to_string()),
        false,
        Some(10),
        false,
    )
}

fn remove_sentrux_mcp_profile(config_dir: &Path) -> Result<(), hermes_core::AgentError> {
    let mcp_config_path = config_dir.join("mcp_servers.json");
    if mcp_config_path.exists() {
        let content = std::fs::read_to_string(&mcp_config_path)
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
        let mut servers: serde_json::Value =
            serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
        if let Some(obj) = servers.as_object_mut() {
            obj.remove(SENTRUX_MCP_SERVER_NAME);
        }
        let json = serde_json::to_string_pretty(&servers)
            .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
        std::fs::write(&mcp_config_path, json)
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    }
    update_yaml_mcp_server(
        config_dir,
        SENTRUX_MCP_SERVER_NAME,
        None,
        None,
        false,
        None,
        true,
    )
}

fn remove_unreal_mcp_profile(config_dir: &Path) -> Result<(), hermes_core::AgentError> {
    let mcp_config_path = config_dir.join("mcp_servers.json");
    if mcp_config_path.exists() {
        let content = std::fs::read_to_string(&mcp_config_path)
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
        let mut servers: serde_json::Value =
            serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
        if let Some(obj) = servers.as_object_mut() {
            obj.remove(UNREAL_MCP_SERVER_NAME);
        }
        let json = serde_json::to_string_pretty(&servers)
            .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
        std::fs::write(&mcp_config_path, json)
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    }
    update_yaml_mcp_server(
        config_dir,
        UNREAL_MCP_SERVER_NAME,
        None,
        None,
        false,
        None,
        true,
    )
}

fn sentrux_mcp_status(config_dir: &Path) -> (bool, bool, bool) {
    let mcp_config_path = config_dir.join("mcp_servers.json");
    let from_json = std::fs::read_to_string(&mcp_config_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get(SENTRUX_MCP_SERVER_NAME).cloned())
        .is_some();
    let from_yaml = hermes_config::load_user_config_file(&config_dir.join("config.yaml"))
        .ok()
        .map(|cfg| {
            cfg.mcp_servers
                .iter()
                .any(|entry| entry.name == SENTRUX_MCP_SERVER_NAME)
        })
        .unwrap_or(false);
    (command_on_path(SENTRUX_MCP_COMMAND), from_json, from_yaml)
}

fn unreal_mcp_status(config_dir: &Path) -> (bool, bool) {
    let mcp_config_path = config_dir.join("mcp_servers.json");
    let from_json = std::fs::read_to_string(&mcp_config_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get(UNREAL_MCP_SERVER_NAME).cloned())
        .is_some();
    let from_yaml = hermes_config::load_user_config_file(&config_dir.join("config.yaml"))
        .ok()
        .map(|cfg| {
            cfg.mcp_servers
                .iter()
                .any(|entry| entry.name == UNREAL_MCP_SERVER_NAME)
        })
        .unwrap_or(false);
    (from_json, from_yaml)
}
