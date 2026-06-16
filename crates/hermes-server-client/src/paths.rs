//! Local persisted paths for server client state.

use std::path::{Path, PathBuf};

pub fn server_state_dir(hermes_home: &Path) -> PathBuf {
    hermes_home.join("server")
}

pub fn profile_cache_path(hermes_home: &Path) -> PathBuf {
    server_state_dir(hermes_home).join("profile.json")
}

pub fn device_state_path(hermes_home: &Path) -> PathBuf {
    server_state_dir(hermes_home).join("device_state.json")
}
