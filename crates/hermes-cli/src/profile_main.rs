//! Profile command handlers extracted from main.rs

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hermes_cli::cli::Cli;
use hermes_config::{hermes_home, load_config, load_user_config_file, save_config_yaml};
use hermes_core::AgentError;

use hermes_cli::gateway_main::prompt_yes_no;

fn profile_aliases_path(profiles_dir: &Path) -> PathBuf {
    profiles_dir.join("aliases.json")
}

fn active_profile_marker_path(profiles_dir: &Path) -> PathBuf {
    profiles_dir.join(".active_profile")
}

fn load_profile_aliases(path: &Path) -> Result<BTreeMap<String, String>, AgentError> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

fn save_profile_aliases(path: &Path, aliases: &BTreeMap<String, String>) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let raw =
        serde_json::to_string_pretty(aliases).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn resolve_profile_name(requested: &str, aliases: &BTreeMap<String, String>) -> String {
    aliases
        .get(requested.trim())
        .cloned()
        .unwrap_or_else(|| requested.trim().to_string())
}

fn resolve_profile_yaml_path(profiles_dir: &Path, name: &str) -> Option<PathBuf> {
    let yaml = profiles_dir.join(format!("{}.yaml", name));
    if yaml.exists() {
        return Some(yaml);
    }
    let yml = profiles_dir.join(format!("{}.yml", name));
    if yml.exists() {
        return Some(yml);
    }
    None
}

fn read_active_profile_name(profiles_dir: &Path) -> Option<String> {
    std::fs::read_to_string(active_profile_marker_path(profiles_dir))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(crate) fn write_active_profile_name(profiles_dir: &Path, name: &str) -> Result<(), AgentError> {
    let path = active_profile_marker_path(profiles_dir);
    std::fs::create_dir_all(profiles_dir)
        .map_err(|e| AgentError::Io(format!("mkdir {}: {}", profiles_dir.display(), e)))?;
    std::fs::write(&path, format!("{}\n", name.trim()))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn load_profile_yaml(path: &Path) -> Result<serde_yaml::Value, AgentError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_yaml::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

fn save_profile_yaml(path: &Path, value: &serde_yaml::Value) -> Result<(), AgentError> {
    let raw = serde_yaml::to_string(value).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

pub(crate) fn validate_profile_name(name: &str) -> Result<String, AgentError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AgentError::Config("profile name cannot be empty".into()));
    }
    if trimmed == "." || trimmed == ".." || trimmed.contains('/') || trimmed.contains('\\') {
        return Err(AgentError::Config(format!(
            "invalid profile name '{}': path separators are not allowed",
            trimmed
        )));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(AgentError::Config(format!(
            "invalid profile name '{}': use letters, numbers, '-', '_' or '.'",
            trimmed
        )));
    }
    Ok(trimmed.to_string())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_profile(
    cli: Cli,
    action: Option<String>,
    name: Option<String>,
    secondary: Option<String>,
    output: Option<String>,
    import_name: Option<String>,
    alias_name: Option<String>,
    remove: bool,
    yes: bool,
    clone: bool,
    clone_all: bool,
    clone_from: Option<String>,
    no_alias: bool,
    no_skills: bool,
) -> Result<(), AgentError> {
    let config_dir = cli
        .config_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(hermes_home);
    let profiles_dir = config_dir.join("profiles");
    let aliases_path = profile_aliases_path(&profiles_dir);
    let mut aliases = load_profile_aliases(&aliases_path)?;

    match action.as_deref().unwrap_or("show") {
        "show" => {
            if let Some(requested) = name {
                let resolved = resolve_profile_name(&requested, &aliases);
                let Some(path) = resolve_profile_yaml_path(&profiles_dir, &resolved) else {
                    return Err(AgentError::Config(format!(
                        "profile '{}' not found (resolved to '{}')",
                        requested, resolved
                    )));
                };
                let raw = std::fs::read_to_string(&path)
                    .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
                println!("{}", raw);
                return Ok(());
            }
            let config = load_config(cli.config_dir.as_deref())
                .map_err(|e| AgentError::Config(e.to_string()))?;
            let active =
                read_active_profile_name(&profiles_dir).unwrap_or_else(|| "(none)".to_string());
            println!("Current profile:");
            println!("  Active:      {}", active);
            println!(
                "  Model:       {}",
                config.model.as_deref().unwrap_or("gpt-4o")
            );
            println!(
                "  Personality: {}",
                config.personality.as_deref().unwrap_or("default")
            );
            println!("  Max turns:   {}", config.max_turns);
            println!("\nUse `hermes profile list` to see all profiles.");
        }
        "list" => {
            if !profiles_dir.exists() {
                println!("No profiles directory found. Run `hermes setup` first.");
                return Ok(());
            }
            let active = read_active_profile_name(&profiles_dir);
            let mut entries: Vec<String> = std::fs::read_dir(&profiles_dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| {
                            e.path()
                                .extension()
                                .map(|ext| ext == "yaml" || ext == "yml")
                                .unwrap_or(false)
                        })
                        .filter_map(|e| {
                            e.path()
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                        })
                        .collect()
                })
                .unwrap_or_default();
            entries.sort();

            if entries.is_empty() {
                println!("No profiles found. Create one with `hermes profile create <name>`.");
            } else {
                println!("Available profiles:");
                for name in &entries {
                    let marker = if active.as_deref() == Some(name.as_str()) {
                        "*"
                    } else {
                        " "
                    };
                    println!("{} {}", marker, name);
                }
                if !aliases.is_empty() {
                    println!("\nAliases:");
                    for (alias, target) in &aliases {
                        println!("  {} -> {}", alias, target);
                    }
                }
            }
        }
        "create" => {
            let profile_name = name.ok_or_else(|| {
                AgentError::Config(
                    "Missing profile name. Usage: hermes profile create <name>".into(),
                )
            })?;
            let profile_name = validate_profile_name(&profile_name)?;

            std::fs::create_dir_all(&profiles_dir)
                .map_err(|e| AgentError::Io(format!("Failed to create profiles dir: {}", e)))?;

            let profile_path = profiles_dir.join(format!("{}.yaml", profile_name));
            if profile_path.exists() {
                return Err(AgentError::Config(format!(
                    "Profile '{}' already exists at {}",
                    profile_name,
                    profile_path.display()
                )));
            }

            let source_name = clone_from
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| resolve_profile_name(s, &aliases))
                .or_else(|| read_active_profile_name(&profiles_dir));
            let source_value = if clone || clone_all {
                let src = source_name.clone().ok_or_else(|| {
                    AgentError::Config(
                        "profile create --clone/--clone-all requires --clone-from or an active profile"
                            .into(),
                    )
                })?;
                let src_path = resolve_profile_yaml_path(&profiles_dir, &src).ok_or_else(|| {
                    AgentError::Config(format!("clone source profile '{}' not found", src))
                })?;
                Some(load_profile_yaml(&src_path)?)
            } else {
                None
            };

            let mut out_map = serde_yaml::Mapping::new();
            out_map.insert(
                serde_yaml::Value::String("name".to_string()),
                serde_yaml::Value::String(profile_name.clone()),
            );

            if let Some(src) = source_value {
                if let Some(src_map) = src.as_mapping() {
                    if clone_all {
                        out_map = src_map.clone();
                        out_map.insert(
                            serde_yaml::Value::String("name".to_string()),
                            serde_yaml::Value::String(profile_name.clone()),
                        );
                    } else {
                        for key in ["model", "personality", "max_turns"] {
                            let k = serde_yaml::Value::String(key.to_string());
                            if let Some(v) = src_map.get(&k) {
                                out_map.insert(k, v.clone());
                            }
                        }
                    }
                }
            }

            if no_skills {
                let skills_key = serde_yaml::Value::String("skills".to_string());
                let overrides_key = serde_yaml::Value::String("skill_overrides".to_string());
                out_map.remove(&skills_key);
                out_map.remove(&overrides_key);
            }

            out_map
                .entry(serde_yaml::Value::String("model".to_string()))
                .or_insert_with(|| serde_yaml::Value::String("openai:gpt-4o".to_string()));
            out_map
                .entry(serde_yaml::Value::String("personality".to_string()))
                .or_insert_with(|| serde_yaml::Value::String("default".to_string()));
            out_map
                .entry(serde_yaml::Value::String("max_turns".to_string()))
                .or_insert_with(|| serde_yaml::Value::Number(serde_yaml::Number::from(50u64)));

            save_profile_yaml(&profile_path, &serde_yaml::Value::Mapping(out_map))?;
            println!(
                "Created profile '{}' at {}",
                profile_name,
                profile_path.display()
            );

            if !no_alias {
                if let Some(alias) = alias_name
                    .or(secondary)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    aliases.insert(alias.clone(), profile_name.clone());
                    save_profile_aliases(&aliases_path, &aliases)?;
                    println!("Added alias '{}' -> '{}'.", alias, profile_name);
                }
            }
        }
        "use" | "switch" => {
            let requested = name.ok_or_else(|| {
                AgentError::Config("Missing profile name. Usage: hermes profile use <name>".into())
            })?;
            let resolved = resolve_profile_name(&requested, &aliases);
            let path = resolve_profile_yaml_path(&profiles_dir, &resolved).ok_or_else(|| {
                AgentError::Config(format!(
                    "Profile '{}' not found (resolved to '{}')",
                    requested, resolved
                ))
            })?;
            let value = load_profile_yaml(&path)?;
            let mut disk = load_user_config_file(&config_dir.join("config.yaml"))
                .map_err(|e| AgentError::Config(e.to_string()))?;
            if let Some(map) = value.as_mapping() {
                if let Some(v) = map
                    .get(&serde_yaml::Value::String("model".to_string()))
                    .and_then(|v| v.as_str())
                {
                    disk.model = Some(v.to_string());
                }
                if let Some(v) = map
                    .get(&serde_yaml::Value::String("personality".to_string()))
                    .and_then(|v| v.as_str())
                {
                    disk.personality = Some(v.to_string());
                }
                if let Some(v) = map
                    .get(&serde_yaml::Value::String("max_turns".to_string()))
                    .and_then(|v| v.as_u64())
                {
                    disk.max_turns = v.min(u32::MAX as u64) as u32;
                }
            }
            save_config_yaml(&config_dir.join("config.yaml"), &disk)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            write_active_profile_name(&profiles_dir, &resolved)?;
            println!(
                "Activated profile '{}' (requested '{}').",
                resolved, requested
            );
        }
        "delete" => {
            let requested = name.ok_or_else(|| {
                AgentError::Config(
                    "Missing profile name. Usage: hermes profile delete <name>".into(),
                )
            })?;
            let resolved = resolve_profile_name(&requested, &aliases);
            let path = resolve_profile_yaml_path(&profiles_dir, &resolved).ok_or_else(|| {
                AgentError::Config(format!(
                    "Profile '{}' not found (resolved to '{}')",
                    requested, resolved
                ))
            })?;
            if !yes
                && !prompt_yes_no(
                    &format!("Delete profile '{}' ({})?", resolved, path.display()),
                    false,
                )
                .await?
            {
                println!("Aborted.");
                return Ok(());
            }
            std::fs::remove_file(&path)
                .map_err(|e| AgentError::Io(format!("remove {}: {}", path.display(), e)))?;
            aliases.retain(|alias, target| alias != &requested && target != &resolved);
            save_profile_aliases(&aliases_path, &aliases)?;
            if read_active_profile_name(&profiles_dir).as_deref() == Some(resolved.as_str()) {
                let _ = std::fs::remove_file(active_profile_marker_path(&profiles_dir));
            }
            println!("Deleted profile '{}' ({})", resolved, path.display());
        }
        "alias" => {
            if remove {
                let alias = alias_name
                    .or(name)
                    .or(secondary)
                    .ok_or_else(|| AgentError::Config("profile alias --remove <alias>".into()))?;
                if aliases.remove(alias.trim()).is_some() {
                    save_profile_aliases(&aliases_path, &aliases)?;
                    println!("Removed alias '{}'.", alias.trim());
                } else {
                    println!("Alias '{}' not found.", alias.trim());
                }
                return Ok(());
            }
            let target = name.ok_or_else(|| {
                AgentError::Config(
                    "profile alias usage: hermes profile alias <target> --name <alias>".into(),
                )
            })?;
            let alias = alias_name.or(secondary).ok_or_else(|| {
                AgentError::Config(
                    "profile alias usage: hermes profile alias <target> --name <alias>".into(),
                )
            })?;
            let resolved_target = resolve_profile_name(&target, &aliases);
            if resolve_profile_yaml_path(&profiles_dir, &resolved_target).is_none() {
                return Err(AgentError::Config(format!(
                    "Alias target profile '{}' not found",
                    resolved_target
                )));
            }
            aliases.insert(alias.trim().to_string(), resolved_target.clone());
            save_profile_aliases(&aliases_path, &aliases)?;
            println!("Alias '{}' -> '{}'", alias.trim(), resolved_target);
        }
        "rename" => {
            let old_requested = name.ok_or_else(|| {
                AgentError::Config("profile rename usage: hermes profile rename <old> <new>".into())
            })?;
            let new_name = secondary.ok_or_else(|| {
                AgentError::Config("profile rename usage: hermes profile rename <old> <new>".into())
            })?;
            let new_name = validate_profile_name(&new_name)?;
            let old_resolved = resolve_profile_name(&old_requested, &aliases);
            let old_path =
                resolve_profile_yaml_path(&profiles_dir, &old_resolved).ok_or_else(|| {
                    AgentError::Config(format!("Profile '{}' not found", old_resolved))
                })?;
            let new_path = profiles_dir.join(format!("{}.yaml", new_name));
            if new_path.exists() {
                return Err(AgentError::Config(format!(
                    "Target profile '{}' already exists",
                    new_name
                )));
            }
            std::fs::rename(&old_path, &new_path).map_err(|e| {
                AgentError::Io(format!(
                    "rename {} -> {}: {}",
                    old_path.display(),
                    new_path.display(),
                    e
                ))
            })?;
            if let Ok(mut value) = load_profile_yaml(&new_path) {
                if let Some(map) = value.as_mapping_mut() {
                    map.insert(
                        serde_yaml::Value::String("name".to_string()),
                        serde_yaml::Value::String(new_name.clone()),
                    );
                    let _ = save_profile_yaml(&new_path, &value);
                }
            }
            for target in aliases.values_mut() {
                if target == &old_resolved {
                    *target = new_name.clone();
                }
            }
            if let Some(v) = aliases.remove(&old_requested) {
                aliases.insert(
                    new_name.clone(),
                    if v == old_resolved {
                        new_name.clone()
                    } else {
                        v
                    },
                );
            }
            save_profile_aliases(&aliases_path, &aliases)?;
            if read_active_profile_name(&profiles_dir).as_deref() == Some(old_resolved.as_str()) {
                write_active_profile_name(&profiles_dir, &new_name)?;
            }
            println!("Renamed profile '{}' -> '{}'", old_resolved, new_name);
        }
        "export" => {
            let target = if let Some(n) = name {
                resolve_profile_name(&n, &aliases)
            } else {
                read_active_profile_name(&profiles_dir).ok_or_else(|| {
                    AgentError::Config(
                        "profile export: no active profile and no name provided".into(),
                    )
                })?
            };
            let source = resolve_profile_yaml_path(&profiles_dir, &target)
                .ok_or_else(|| AgentError::Config(format!("Profile '{}' not found", target)))?;
            let out = output.unwrap_or_else(|| format!("{}.profile.yaml", target));
            std::fs::copy(&source, &out).map_err(|e| {
                AgentError::Io(format!("copy {} -> {}: {}", source.display(), out, e))
            })?;
            println!("Exported profile '{}' to {}", target, out);
        }
        "import" => {
            let source = name.ok_or_else(|| {
                AgentError::Config("profile import usage: hermes profile import <path>".into())
            })?;
            let source_path = PathBuf::from(&source);
            if !source_path.exists() {
                return Err(AgentError::Config(format!(
                    "profile import source not found: {}",
                    source_path.display()
                )));
            }
            let mut value = load_profile_yaml(&source_path)?;
            let target_name_raw = import_name.unwrap_or_else(|| {
                source_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
            let target_name = validate_profile_name(&target_name_raw)?;
            std::fs::create_dir_all(&profiles_dir)
                .map_err(|e| AgentError::Io(format!("mkdir {}: {}", profiles_dir.display(), e)))?;
            let target_path = profiles_dir.join(format!("{}.yaml", target_name));
            if target_path.exists() {
                let metadata = std::fs::metadata(&target_path).map_err(|e| {
                    AgentError::Io(format!("stat {}: {}", target_path.display(), e))
                })?;
                if metadata.is_dir() {
                    return Err(AgentError::Config(format!(
                        "Refusing to import profile: target path is a directory ({})",
                        target_path.display()
                    )));
                }
                if !yes {
                    return Err(AgentError::Config(format!(
                        "Target profile exists at {} (re-run with -y to overwrite)",
                        target_path.display()
                    )));
                }
            }
            if let Some(map) = value.as_mapping_mut() {
                map.insert(
                    serde_yaml::Value::String("name".to_string()),
                    serde_yaml::Value::String(target_name.clone()),
                );
            }
            let staged_path = profiles_dir.join(format!(
                ".{}.import-{}.yaml.tmp",
                target_name,
                uuid::Uuid::new_v4()
            ));
            save_profile_yaml(&staged_path, &value)?;
            if target_path.exists() {
                std::fs::remove_file(&target_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", target_path.display(), e))
                })?;
            }
            if let Err(err) = std::fs::rename(&staged_path, &target_path) {
                let _ = std::fs::remove_file(&staged_path);
                return Err(AgentError::Io(format!(
                    "rename {} -> {}: {}",
                    staged_path.display(),
                    target_path.display(),
                    err
                )));
            }
            if !no_alias {
                if let Some(alias) = alias_name
                    .or(secondary)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    aliases.insert(alias.clone(), target_name.clone());
                    save_profile_aliases(&aliases_path, &aliases)?;
                    println!("Added alias '{}' -> '{}'.", alias, target_name);
                }
            }
            println!(
                "Imported profile '{}' from {}",
                target_name,
                source_path.display()
            );
        }
        other => {
            println!(
                "Unknown profile action: '{}'. Use list|show|create|use|delete|alias|rename|export|import.",
                other
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_profile_command(
    cli: Cli,
    action: Option<String>,
    name: Option<String>,
    secondary: Option<String>,
    output: Option<String>,
    import_name: Option<String>,
    alias_name: Option<String>,
    remove: bool,
    yes: bool,
    clone: bool,
    clone_all: bool,
    clone_from: Option<String>,
    no_alias: bool,
    no_skills: bool,
) -> Result<(), AgentError> {
    run_profile(
        cli,
        action,
        name,
        secondary,
        output,
        import_name,
        alias_name,
        remove,
        yes,
        clone,
        clone_all,
        clone_from,
        no_alias,
        no_skills,
    )
    .await
}
