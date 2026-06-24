//! Voice dialog CLI handler (`hermes talk`).

#[cfg(feature = "talk")]
use hermes_talk::Config;

#[cfg(feature = "talk")]
use hermes_talk::{Session, TalkPushBridge};

pub async fn handle_cli_talk(
    action: Option<String>,
    config: Option<String>,
    seconds: u64,
) -> Result<(), hermes_core::AgentError> {
    use std::path::PathBuf;

    use hermes_talk::audio::{list_devices, probe_capture, probe_playback};
    use hermes_talk::{Config, ensure_talk_home, init_talk_home, run_enroll};

    let action = action.as_deref().unwrap_or("run");
    let default_config = config.is_none();
    let cfg_path: PathBuf = config
        .map(PathBuf::from)
        .unwrap_or_else(hermes_config::talk_config_path);
    let base = cfg_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(hermes_config::talk_dir);

    if default_config && !cfg_path.exists() {
        ensure_talk_home().map_err(map_talk_error)?;
    }

    match action {
        "init" => init_talk_home().map_err(map_talk_error),
        "list-devices" => list_devices().map_err(map_talk_error),
        "run" => {
            let cfg = Config::load_with_base(&cfg_path, &base).map_err(map_talk_error)?;
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(4)
                    .thread_name("hermes-talk")
                    .build()
                    .expect("talk runtime");
                rt.block_on(async move { run_talk_session(cfg).await.map_err(map_talk_error) })
            })
            .await
            .map_err(|e| hermes_core::AgentError::Config(format!("talk session task: {e}")))??;
            Ok(())
        }
        "probe-capture" => {
            let cfg = Config::load_with_base(&cfg_path, &base).map_err(map_talk_error)?;
            probe_capture(&cfg.audio, cfg.asr.chunk_ms, seconds).map_err(map_talk_error)?;
            Ok(())
        }
        "probe-playback" => {
            let cfg = Config::load_with_base(&cfg_path, &base).map_err(map_talk_error)?;
            probe_playback(&cfg.audio, cfg.tts.sample_rate).map_err(map_talk_error)?;
            Ok(())
        }
        "enroll" => {
            let cfg = Config::load_with_base(&cfg_path, &base).map_err(map_talk_error)?;
            run_enroll(&cfg, seconds).map_err(map_talk_error)?;
            Ok(())
        }
        other => Err(hermes_core::AgentError::Config(format!(
            "unknown talk action '{other}'. Available: run, init, list-devices, probe-capture, probe-playback, enroll"
        ))),
    }
}

#[cfg(feature = "talk")]
async fn run_talk_session(cfg: Config) -> Result<(), hermes_talk::DemoError> {
    use tokio::sync::mpsc;

    let channel_mode = cfg.llm.tools_enabled && cfg.llm.aipc_talk.uses_channel();
    let session_key = cfg.llm.aipc_talk.session_key.clone();

    let mut session = Session::new(cfg);

    if channel_mode {
        let (msg_tx, msg_rx) = mpsc::channel(128);
        let push_bridge = TalkPushBridge::new(msg_tx.clone());
        let embedded = crate::talk_embedded::bootstrap_talk_embedded(&session_key, push_bridge)
            .await
            .map_err(|e| hermes_talk::DemoError::Config(e.to_string()))?;
        let work_tx = embedded.work_tx.clone();
        let _embedded = embedded;
        session = session
            .with_hermes_work_tx(work_tx)
            .with_hermes_msg_channels(msg_tx, msg_rx);
        session.run().await
    } else {
        session.run().await
    }
}

#[cfg(not(feature = "talk"))]
async fn run_talk_session(_cfg: Config) -> Result<(), hermes_talk::DemoError> {
    Err(hermes_talk::DemoError::Config(
        "talk feature not enabled".to_string(),
    ))
}

fn map_talk_error(e: hermes_talk::DemoError) -> hermes_core::AgentError {
    hermes_core::AgentError::Config(e.to_string())
}
