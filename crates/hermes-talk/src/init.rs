//! Initialize `$HERMES_HOME` (`.hermes-agent-ultra`) and `hermes-talk` layout.

use std::fs;
use std::path::Path;

use crate::error::{DemoError, Result};

const CONFIG_EXAMPLE: &str = include_str!("../config.example.toml");

const SUBDIRS: &[&str] = &[
    "auth",
    "data",
    "frontend_extras",
    "models/vad",
    "models/denoise",
    "models/speaker",
    "models/kws-zh-en",
    "models/rk3588",
    "models/sensevoice",
    "models/kokoro",
];

/// Create Hermes home + talk directory tree and default configs if missing (quiet; auto-init).
pub fn ensure_talk_home() -> Result<()> {
    let hermes_home = hermes_config::ensure_hermes_home_layout(None);
    tracing::debug!(path = %hermes_home.display(), "ensured hermes home layout");

    let home = hermes_config::talk_dir();
    fs::create_dir_all(&home)
        .map_err(|e| DemoError::Config(format!("mkdir {}: {e}", home.display())))?;

    for sub in SUBDIRS {
        let dir = home.join(sub);
        fs::create_dir_all(&dir)
            .map_err(|e| DemoError::Config(format!("mkdir {}: {e}", dir.display())))?;
    }

    seed_bundled_assets(&home)?;
    seed_gateway_config_from_bundle()?;

    let config_path = hermes_config::talk_config_path();
    if !config_path.exists() {
        fs::write(&config_path, CONFIG_EXAMPLE)
            .map_err(|e| DemoError::Config(format!("write {}: {e}", config_path.display())))?;
        tracing::info!("created talk config at {}", config_path.display());
    }

    let example_path = home.join("config.toml.example");
    fs::write(&example_path, CONFIG_EXAMPLE).map_err(|e| {
        DemoError::Config(format!("write {}: {e}", example_path.display()))
    })?;
    tracing::debug!(path = %example_path.display(), "refreshed talk config example");
    Ok(())
}

/// When running from `make package-talk-rockchip` layout, link bundled models into talk home.
fn seed_bundled_assets(talk_home: &Path) -> Result<()> {
    let Some(bundle_root) = bundle_root_for_talk_home() else {
        return Ok(());
    };

    let config_path = talk_home.join("config.toml");
    let bundle_example = bundle_root.join("config.example.toml");
    let needs_talk_config = if !config_path.exists() {
        true
    } else if let Ok(content) = fs::read_to_string(&config_path) {
        content.contains("11888")
            || content.contains("/home/key.lic")
            || content.contains("/root/rktts/")
            || content.contains(r#""license_path": "key.lic""#)
    } else {
        false
    };
    if bundle_example.is_file() && needs_talk_config {
        fs::copy(&bundle_example, &config_path).map_err(|e| {
            DemoError::Config(format!(
                "copy {} -> {}: {e}",
                bundle_example.display(),
                config_path.display()
            ))
        })?;
        tracing::info!(
            "installed talk config from bundle at {}",
            config_path.display()
        );
    }

    for item in ["auth", "data", "models", "frontend_extras"] {
        let src = bundle_root.join(item);
        let dst = talk_home.join(item);
        if !src.exists() || dst.exists() {
            continue;
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&src, &dst).map_err(|e| {
                DemoError::Config(format!("link {} -> {}: {e}", dst.display(), src.display()))
            })?;
            tracing::info!("linked bundled {} into talk home", item);
        }
        #[cfg(not(unix))]
        {
            let _ = (src, dst);
        }
    }
    Ok(())
}

fn seed_gateway_config_from_bundle() -> Result<()> {
    let Some(bundle_root) = bundle_root_for_talk_home() else {
        return Ok(());
    };
    let example = bundle_root.join("config.example.yaml");
    if !example.is_file() {
        return Ok(());
    }

    let dest = hermes_config::config_path();
    let needs_write = if !dest.exists() {
        true
    } else {
        fs::read_to_string(&dest)
            .map(|content| content.contains("11888"))
            .unwrap_or(false)
    };
    if !needs_write {
        return Ok(());
    }

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| DemoError::Config(format!("mkdir {}: {e}", parent.display())))?;
    }
    fs::copy(&example, &dest).map_err(|e| {
        DemoError::Config(format!(
            "copy {} -> {}: {e}",
            example.display(),
            dest.display()
        ))
    })?;
    tracing::info!("installed Hermes config at {}", dest.display());
    Ok(())
}

fn bundle_root_for_talk_home() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("HERMES_TALK_BUNDLE_DIR") {
        let bundle = std::path::PathBuf::from(dir);
        if bundle.join("start.sh").is_file() {
            return Some(bundle);
        }
    }
    None
}

/// Create Hermes home + talk directory tree and default configs if missing.
pub fn init_talk_home() -> Result<()> {
    let talk_config_path = hermes_config::talk_config_path();
    let gateway_config_path = hermes_config::config_path();
    let talk_created = !talk_config_path.exists();
    let gateway_created = !gateway_config_path.exists();
    ensure_talk_home()?;
    let hermes_home = hermes_config::hermes_home();
    let talk_home = hermes_config::talk_dir();
    if gateway_created && gateway_config_path.exists() {
        println!("Created {}", gateway_config_path.display());
    } else if gateway_config_path.exists() {
        println!("Config already exists: {}", gateway_config_path.display());
    }
    if talk_created {
        println!("Created {}", talk_config_path.display());
    } else {
        println!("Config already exists: {}", talk_config_path.display());
        println!(
            "  Full template refreshed at {}",
            talk_home.join("config.toml.example").display()
        );
        println!("  Merge missing sections (e.g. [orchestrator]) from that file, or delete config.toml and re-run init.");
    }
    print_post_init_notes(&hermes_home, &talk_home);
    Ok(())
}

fn print_post_init_notes(hermes_home: &Path, talk_home: &Path) {
    println!();
    println!("Hermes home: {}", hermes_home.display());
    println!("Talk home: {}", talk_home.display());
    println!();
    println!("Next steps:");
    println!(
        "  1. Edit {} with your API keys and backends.",
        hermes_config::talk_config_path().display()
    );
    println!(
        "  2. Edit {} for embedded Hermes / gateway settings (if using channel transport).",
        hermes_config::config_path().display()
    );
    println!("  3. Download sherpa-onnx models: make download-talk-models (into <repo>/.models/).");
    println!(
        "     Then copy or package into {}/models/ (sensevoice, kokoro, kws-zh-en, vad, …).",
        talk_home.display()
    );
    println!("     Docs: https://k2-fsa.github.io/sherpa/onnx/index.html");
    println!(
        "  4. For Rockchip local ASR/TTS, copy SDK data to {}/data, {}/models/rk3588, and licenses to {}/auth/.",
        talk_home.display(),
        talk_home.display(),
        talk_home.display()
    );
    println!("  5. Run `hermes talk list-devices` to verify audio devices.");
    println!("  6. Run `hermes talk` to start the voice dialog loop.");
    println!();
    println!(
        "Note: `call_hermes` uses in-process channel transport by default (transport = \"channel\")."
    );
    println!(
        "      Set transport = \"ws\" and url = \"ws://127.0.0.1:9100\" for remote Hermes bridge."
    );
}
