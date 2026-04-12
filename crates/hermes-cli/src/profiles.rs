use std::path::Path;

pub fn list_profiles(config_dir: &Path) -> Vec<String> {
    let profiles = config_dir.join("profiles");
    let Ok(entries) = std::fs::read_dir(profiles) else {
        return vec![];
    };
    entries
        .flatten()
        .filter_map(|e| e.path().file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()))
        .collect()
}
