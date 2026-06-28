use std::collections::{BTreeMap, BTreeSet};

use super::{DiscordSlashRegistrationSpec, DISCORD_APPLICATION_COMMAND_LIMIT};

pub fn discord_auto_registered_commands(
    explicit_names: impl IntoIterator<Item = impl AsRef<str>>,
    gateway_specs: impl IntoIterator<Item = DiscordSlashRegistrationSpec>,
    plugin_specs: impl IntoIterator<Item = DiscordSlashRegistrationSpec>,
) -> Vec<DiscordSlashRegistrationSpec> {
    let mut registered = explicit_names
        .into_iter()
        .map(|name| {
            name.as_ref()
                .trim()
                .trim_start_matches('/')
                .to_ascii_lowercase()
        })
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>();
    let remaining_capacity = DISCORD_APPLICATION_COMMAND_LIMIT.saturating_sub(registered.len());
    let mut out = Vec::new();
    for spec in gateway_specs.into_iter().chain(plugin_specs) {
        if out.len() >= remaining_capacity {
            break;
        }
        let key = spec
            .name
            .trim()
            .trim_start_matches('/')
            .to_ascii_lowercase();
        if key.is_empty() || registered.contains(&key) {
            continue;
        }
        registered.insert(key);
        out.push(spec);
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordCommandSyncSummary {
    pub total: usize,
    pub unchanged: usize,
    pub updated: usize,
    pub recreated: usize,
    pub created: usize,
    pub deleted: usize,
    pub mutations: Vec<DiscordCommandSyncMutation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscordCommandSyncMutation {
    Create { name: String },
    Update { name: String },
    Recreate { name: String },
    Delete { name: String },
}

fn json_to_sorted(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted = map
                .iter()
                .map(|(key, value)| (key.clone(), json_to_sorted(value)))
                .collect::<serde_json::Map<_, _>>();
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(json_to_sorted).collect())
        }
        other => other.clone(),
    }
}

fn command_key(payload: &serde_json::Value) -> Option<(String, u64)> {
    let name = payload.get("name")?.as_str()?.trim().to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let command_type = payload.get("type").and_then(|v| v.as_u64()).unwrap_or(1);
    Some((name, command_type))
}

fn normalize_command_payload(payload: &serde_json::Value) -> serde_json::Value {
    let mut normalized = match payload {
        serde_json::Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    for key in [
        "id",
        "application_id",
        "version",
        "name_localizations",
        "description_localizations",
    ] {
        normalized.remove(key);
    }
    normalized.retain(|_, value| !value.is_null());
    normalized
        .entry("type")
        .or_insert_with(|| serde_json::json!(1));
    normalized
        .entry("options")
        .or_insert_with(|| serde_json::json!([]));
    normalized
        .entry("nsfw")
        .or_insert_with(|| serde_json::json!(false));
    normalized
        .entry("dm_permission")
        .or_insert_with(|| serde_json::json!(true));
    json_to_sorted(&serde_json::Value::Object(normalized))
}

fn command_patchable_view(payload: &serde_json::Value) -> serde_json::Value {
    let mut map = match normalize_command_payload(payload) {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    for key in [
        "nsfw",
        "dm_permission",
        "default_member_permissions",
        "contexts",
        "integration_types",
    ] {
        map.remove(key);
    }
    serde_json::Value::Object(map)
}

fn command_requires_recreate(desired: &serde_json::Value, existing: &serde_json::Value) -> bool {
    let desired = normalize_command_payload(desired);
    let existing = normalize_command_payload(existing);
    [
        "nsfw",
        "dm_permission",
        "default_member_permissions",
        "contexts",
        "integration_types",
    ]
    .into_iter()
    .any(|key| desired.get(key) != existing.get(key))
}

pub fn plan_discord_command_sync(
    desired: &[serde_json::Value],
    existing: &[serde_json::Value],
) -> DiscordCommandSyncSummary {
    let mut existing_by_key = existing
        .iter()
        .filter_map(|payload| command_key(payload).map(|key| (key, payload)))
        .collect::<BTreeMap<_, _>>();
    let mut summary = DiscordCommandSyncSummary {
        total: desired.len(),
        ..DiscordCommandSyncSummary::default()
    };

    let desired_keys = desired
        .iter()
        .filter_map(command_key)
        .collect::<BTreeSet<_>>();
    let obsolete_keys = existing_by_key
        .keys()
        .filter(|key| !desired_keys.contains(*key))
        .cloned()
        .collect::<Vec<_>>();

    // Discord rejects upserts that would briefly exceed the 100-command cap,
    // so remove obsolete commands before creating replacement commands.
    for key in obsolete_keys {
        if let Some(existing_payload) = existing_by_key.remove(&key) {
            let name = command_key(existing_payload)
                .map(|(name, _)| name)
                .unwrap_or_else(|| key.0.clone());
            summary.deleted += 1;
            summary
                .mutations
                .push(DiscordCommandSyncMutation::Delete { name });
        }
    }

    for desired_payload in desired {
        let Some((name, command_type)) = command_key(desired_payload) else {
            continue;
        };
        match existing_by_key.remove(&(name.clone(), command_type)) {
            None => {
                summary.created += 1;
                summary
                    .mutations
                    .push(DiscordCommandSyncMutation::Create { name });
            }
            Some(existing_payload)
                if normalize_command_payload(desired_payload)
                    == normalize_command_payload(existing_payload) =>
            {
                summary.unchanged += 1;
            }
            Some(existing_payload)
                if command_requires_recreate(desired_payload, existing_payload) =>
            {
                summary.recreated += 1;
                summary
                    .mutations
                    .push(DiscordCommandSyncMutation::Recreate { name });
            }
            Some(existing_payload)
                if command_patchable_view(desired_payload)
                    != command_patchable_view(existing_payload) =>
            {
                summary.updated += 1;
                summary
                    .mutations
                    .push(DiscordCommandSyncMutation::Update { name });
            }
            Some(_) => {
                summary.unchanged += 1;
            }
        }
    }

    summary
}

pub fn discord_command_fingerprint(commands: &[serde_json::Value]) -> String {
    let mut normalized = commands
        .iter()
        .map(normalize_command_payload)
        .collect::<Vec<_>>();
    normalized.sort_by_key(command_key);
    serde_json::to_string(&normalized).unwrap_or_default()
}
