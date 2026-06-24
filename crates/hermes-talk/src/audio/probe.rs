use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use tracing::info;

use crate::audio::{AudioCapture, AudioPlayback};
use crate::config::AudioConfig;
use crate::error::{DemoError, Result};

pub fn list_devices() -> Result<()> {
    let host = cpal::default_host();
    info!(host = ?host.id(), "audio host");

    println!("Input devices:");
    for (i, dev) in host
        .input_devices()
        .map_err(|e| DemoError::Audio(e.to_string()))?
        .enumerate()
    {
        let name = dev.name().unwrap_or_else(|_| "?".into());
        let cfg = dev
            .default_input_config()
            .map(|c| {
                format!(
                    "{}Hz {}ch {:?}",
                    c.sample_rate().0,
                    c.channels(),
                    c.sample_format()
                )
            })
            .unwrap_or_else(|e| format!("config error: {e}"));
        println!("  [{i}] {name}  ({cfg})");
    }

    println!("Output devices:");
    for (i, dev) in host
        .output_devices()
        .map_err(|e| DemoError::Audio(e.to_string()))?
        .enumerate()
    {
        let name = dev.name().unwrap_or_else(|_| "?".into());
        let cfg = dev
            .default_output_config()
            .map(|c| {
                format!(
                    "{}Hz {}ch {:?}",
                    c.sample_rate().0,
                    c.channels(),
                    c.sample_format()
                )
            })
            .unwrap_or_else(|e| format!("config error: {e}"));
        println!("  [{i}] {name}  ({cfg})");
    }
    Ok(())
}

/// Capture audio for a few seconds and print peak levels (0.0 = silence).
pub fn probe_capture(audio_cfg: &AudioConfig, chunk_ms: u32, seconds: u64) -> Result<()> {
    let capture = AudioCapture::start(audio_cfg, chunk_ms)?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut last_report = Instant::now();
    let mut peak = 0.0f32;
    let mut chunks = 0u64;

    println!("Capturing for {seconds}s — speak into the mic...");
    while Instant::now() < deadline {
        if let Some(chunk) = capture.try_recv_chunk() {
            chunks += 1;
            for &s in &chunk.samples_f32 {
                let a = s.abs();
                if a > peak {
                    peak = a;
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            println!("  peak={peak:.4}  chunks={chunks}");
            peak = 0.0;
            chunks = 0;
            last_report = Instant::now();
        }
    }
    if peak > 0.0 || chunks > 0 {
        println!("  peak={peak:.4}  chunks={chunks}");
    }
    println!("Done. peak > 0.01 while speaking means capture is working.");
    Ok(())
}

/// Play a short test tone through the configured output device.
pub fn probe_playback(audio_cfg: &AudioConfig, source_rate: u32) -> Result<()> {
    let playback = AudioPlayback::start(audio_cfg, source_rate)?;
    let generation = playback.bump_generation();
    let freq = 440.0f32;
    let duration_sec = 2.0f32;
    let n = (source_rate as f32 * duration_sec) as usize;
    let mut tone = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / source_rate as f32;
        tone.push((t * freq * 2.0 * std::f32::consts::PI).sin() * 0.3);
    }
    println!("Playing 440Hz tone for 2s...");
    playback.enqueue_f32(generation, &tone);
    std::thread::sleep(Duration::from_secs(3));
    println!("Done. You should have heard a beep.");
    Ok(())
}
