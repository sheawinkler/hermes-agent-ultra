fn gateway_profiles_dir() -> PathBuf {
    hermes_config::hermes_home().join("profiles")
}

fn load_gateway_profile_aliases(profiles_dir: &Path) -> BTreeMap<String, String> {
    let path = profiles_dir.join("aliases.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<BTreeMap<String, String>>(&raw).unwrap_or_default()
}

fn resolve_gateway_profile_name(
    requested: &str,
    aliases: &BTreeMap<String, String>,
) -> Result<String, String> {
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return Err("profile name cannot be empty".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(format!(
            "invalid profile name '{}': path separators are not allowed",
            trimmed
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(format!(
            "invalid profile name '{}': use letters, numbers, '-', '_' or '.'",
            trimmed
        ));
    }
    Ok(aliases
        .get(trimmed)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .unwrap_or(trimmed)
        .to_string())
}

fn resolve_gateway_profile_path(profiles_dir: &Path, name: &str) -> Option<PathBuf> {
    let yaml = profiles_dir.join(format!("{name}.yaml"));
    if yaml.exists() {
        return Some(yaml);
    }
    let yml = profiles_dir.join(format!("{name}.yml"));
    yml.exists().then_some(yml)
}

fn yaml_string(map: &serde_yaml::Mapping, key: &str) -> Option<String> {
    map.get(serde_yaml::Value::String(key.to_string()))
        .and_then(serde_yaml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn load_gateway_profile_overlay(requested: &str) -> Result<GatewayProfileOverlay, String> {
    let profiles_dir = gateway_profiles_dir();
    let aliases = load_gateway_profile_aliases(&profiles_dir);
    let name = resolve_gateway_profile_name(requested, &aliases)?;
    let path = resolve_gateway_profile_path(&profiles_dir, &name).ok_or_else(|| {
        format!(
            "profile '{}' not found under {}",
            name,
            profiles_dir.display()
        )
    })?;
    let raw =
        std::fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(&raw).map_err(|err| format!("parse {}: {err}", path.display()))?;
    let Some(map) = value.as_mapping() else {
        return Err(format!("profile '{}' must be a YAML mapping", name));
    };

    let model = yaml_string(map, "model");
    let provider = yaml_string(map, "provider").or_else(|| {
        model
            .as_deref()
            .and_then(|value| value.split_once(':').map(|(provider, _)| provider.trim()))
            .filter(|provider| !provider.is_empty())
            .map(str::to_string)
    });
    let personality = yaml_string(map, "personality");
    let home = yaml_string(map, "home_dir").or_else(|| yaml_string(map, "home"));

    Ok(GatewayProfileOverlay {
        name,
        path,
        model,
        provider,
        personality,
        home,
    })
}

fn apply_gateway_profile_overlay(state: &mut SessionRuntimeState, overlay: &GatewayProfileOverlay) {
    state.profile = Some(overlay.name.clone());
    if let Some(model) = &overlay.model {
        state.model = Some(model.clone());
    }
    if let Some(provider) = &overlay.provider {
        state.provider = Some(provider.clone());
    }
    if let Some(personality) = &overlay.personality {
        state.personality = Some(personality.clone());
    }
    if let Some(home) = &overlay.home {
        state.home = Some(home.clone());
    }
}

fn render_profile_overlay_reply(requested: &str, overlay: &GatewayProfileOverlay) -> String {
    let mut applied = Vec::new();
    if let Some(model) = &overlay.model {
        applied.push(format!("model={model}"));
    }
    if let Some(provider) = &overlay.provider {
        applied.push(format!("provider={provider}"));
    }
    if let Some(personality) = &overlay.personality {
        applied.push(format!("personality={personality}"));
    }
    if let Some(home) = &overlay.home {
        applied.push(format!("home={home}"));
    }
    let applied = if applied.is_empty() {
        "metadata only".to_string()
    } else {
        applied.join(", ")
    };
    format!(
        "👤 Profile switched to: {} (requested '{}'; {}; {})",
        overlay.name,
        requested.trim(),
        applied,
        overlay.path.display()
    )
}
