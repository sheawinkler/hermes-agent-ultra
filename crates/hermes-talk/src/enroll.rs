//! Voiceprint enrollment from microphone.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::config::Config;
use crate::error::{DemoError, Result};
use crate::speaker::enroll_voiceprint;

/// Record audio from the configured input device and save a voiceprint.
pub fn run_enroll(cfg: &Config, duration_secs: u64) -> Result<()> {
    let spk_cfg = &cfg.speaker;
    let model_path = spk_cfg.resolve_model_path();
    if model_path.is_empty() || !std::path::Path::new(&model_path).exists() {
        return Err(DemoError::Config(format!(
            "speaker model not found: {model_path}. Download a 3dspeaker model first."
        )));
    }

    println!("Recording {duration_secs} seconds of audio for voiceprint enrollment...");
    println!("Please speak clearly into the microphone.");
    println!();

    let host = cpal::default_host();
    let device = if cfg.audio.input_device.is_empty() {
        host.default_input_device()
            .ok_or_else(|| DemoError::Audio("no input device found".into()))?
    } else {
        let name = &cfg.audio.input_device;
        host.input_devices()
            .map_err(|e| DemoError::Audio(format!("list input devices: {e}")))?
            .find(|d| d.name().map(|n| n == *name).unwrap_or(false))
            .ok_or_else(|| DemoError::Audio(format!("input device not found: {name}")))?
    };

    let config = device
        .default_input_config()
        .map_err(|e| DemoError::Audio(format!("input config: {e}")))?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let total_samples = (sample_rate * duration_secs as u32) as usize;

    let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(0);

    let stream = device
        .build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mono: Vec<f32> = if channels == 1 {
                    data.to_vec()
                } else {
                    data.chunks(channels)
                        .map(|ch| ch.iter().sum::<f32>() / channels as f32)
                        .collect()
                };
                let _ = tx.try_send(mono);
            },
            move |err| eprintln!("audio error: {err}"),
            None,
        )
        .map_err(|e| DemoError::Audio(format!("build input stream: {e}")))?;

    stream
        .play()
        .map_err(|e| DemoError::Audio(format!("start capture: {e}")))?;

    let mut recorded: Vec<f32> = Vec::with_capacity(total_samples);
    let start = Instant::now();
    while start.elapsed().as_secs() < duration_secs {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                recorded.extend_from_slice(&chunk);
                if recorded.len() >= total_samples {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    drop(stream);
    recorded.truncate(total_samples);

    if recorded.len() < sample_rate as usize {
        return Err(DemoError::Config(format!(
            "recorded only {} samples ({}s), need at least {}s of speech",
            recorded.len(),
            recorded.len() / sample_rate as usize,
            duration_secs
        )));
    }

    println!(
        "Recorded {:.1}s of audio, extracting voiceprint...",
        recorded.len() as f32 / sample_rate as f32
    );
    enroll_voiceprint(spk_cfg, &recorded, sample_rate).map_err(DemoError::Config)?;

    println!("Voiceprint saved to: {}", spk_cfg.voiceprint_path);
    println!("You can now enable speaker verification in config.toml: [speaker] enabled = true");
    Ok(())
}
