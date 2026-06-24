use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use cpal::SampleFormat;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tracing::info;

use crate::config::AudioConfig;
use crate::error::{DemoError, Result};

pub struct AudioPlayback {
    queue: Arc<Mutex<PlaybackState>>,
    generation: Arc<AtomicU64>,
    source_rate: u32,
    _thread: JoinHandle<()>,
}

struct PlaybackState {
    buffer: VecDeque<f32>,
    /// Fractional read position into `buffer` (in source-rate samples).
    playhead: f64,
    active_generation: u64,
    stopped: bool,
    source_rate: u32,
    device_rate: u32,
    /// DC blocker state (first-order high-pass filter)
    dc_prev_in: f32,
    dc_prev_out: f32,
}

impl AudioPlayback {
    pub fn start(audio_cfg: &AudioConfig, source_rate: u32) -> Result<Self> {
        let host = cpal::default_host();
        let device = pick_output_device(&host, audio_cfg)?;
        let config = device
            .default_output_config()
            .map_err(|e| DemoError::Audio(e.to_string()))?;
        let device_rate = config.sample_rate().0;
        let channels = config.channels() as usize;

        info!(
            device = %device.name().unwrap_or_default(),
            device_rate,
            source_rate,
            "audio playback (resampling source -> device)"
        );

        if device_rate != source_rate {
            info!(
                "TTS PCM is {source_rate} Hz; output device is {device_rate} Hz — resampling in playback"
            );
        }

        let queue = Arc::new(Mutex::new(PlaybackState {
            buffer: VecDeque::new(),
            playhead: 0.0,
            active_generation: 0,
            stopped: false,
            source_rate,
            device_rate,
            dc_prev_in: 0.0,
            dc_prev_out: 0.0,
        }));
        let generation = Arc::new(AtomicU64::new(0));
        let q = queue.clone();

        let stream_config: cpal::StreamConfig = config.clone().into();
        let thread = thread::spawn(move || {
            let err_fn = |e| eprintln!("playback error: {e}");
            let stream = match config.sample_format() {
                SampleFormat::F32 => device.build_output_stream(
                    &stream_config,
                    move |out: &mut [f32], _| {
                        fill_output(out, channels, &q);
                    },
                    err_fn,
                    None,
                ),
                SampleFormat::I16 => device.build_output_stream(
                    &stream_config,
                    move |out: &mut [i16], _| {
                        let mut tmp = vec![0.0f32; out.len()];
                        fill_output(&mut tmp, channels, &q);
                        for (o, s) in out.iter_mut().zip(tmp.iter()) {
                            *o = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                        }
                    },
                    err_fn,
                    None,
                ),
                SampleFormat::U16 => device.build_output_stream(
                    &stream_config,
                    move |out: &mut [u16], _| {
                        let mut tmp = vec![0.0f32; out.len()];
                        fill_output(&mut tmp, channels, &q);
                        for (o, s) in out.iter_mut().zip(tmp.iter()) {
                            *o = ((s.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16;
                        }
                    },
                    err_fn,
                    None,
                ),
                other => {
                    eprintln!("unsupported playback format {other:?}");
                    return;
                }
            };
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("build playback: {e}");
                    return;
                }
            };
            if let Err(e) = stream.play() {
                eprintln!("play playback: {e}");
            }
            loop {
                thread::sleep(std::time::Duration::from_secs(1));
            }
        });

        Ok(Self {
            queue,
            generation,
            source_rate,
            _thread: thread,
        })
    }

    pub fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn bump_generation(&self) -> u64 {
        let g = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut st = self.queue.lock().unwrap();
        st.active_generation = g;
        st.stopped = false;
        st.playhead = 0.0;
        g
    }

    pub fn enqueue_pcm_i16(&self, generation: u64, pcm: &[u8]) {
        let samples = crate::audio::pcm::i16_le_to_f32(pcm);
        self.enqueue_f32(generation, &samples);
    }

    pub fn enqueue_f32(&self, generation: u64, samples: &[f32]) {
        let mut st = self.queue.lock().unwrap();
        if st.stopped || generation < st.active_generation {
            return;
        }
        st.buffer.extend(samples);
    }

    pub fn stop_clear(&self) {
        let g = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut st = self.queue.lock().unwrap();
        st.buffer.clear();
        st.playhead = 0.0;
        st.active_generation = g;
        st.stopped = true;
    }

    pub fn resume_playback(&self) {
        let mut st = self.queue.lock().unwrap();
        st.stopped = false;
    }

    pub fn sample_rate(&self) -> u32 {
        self.source_rate
    }

    pub fn buffered_samples(&self) -> usize {
        self.queue.lock().unwrap().buffer.len()
    }

    /// Wait until the play queue is nearly empty or timeout.
    pub async fn wait_drain(&self, timeout: std::time::Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.buffered_samples() < self.source_rate as usize / 50 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}

fn pick_output_device(host: &cpal::Host, cfg: &AudioConfig) -> Result<cpal::Device> {
    if cfg.output_device.is_empty() {
        return host
            .default_output_device()
            .ok_or_else(|| DemoError::Audio("no default output device".into()));
    }
    let name = &cfg.output_device;
    host.output_devices()
        .map_err(|e| DemoError::Audio(e.to_string()))?
        .find(|d| d.name().map(|n| n == *name).unwrap_or(false))
        .ok_or_else(|| DemoError::Audio(format!("output device not found: {name}")))
}

/// Resample from `source_rate` buffer to `device_rate` output using linear interpolation.
fn fill_output(out: &mut [f32], channels: usize, queue: &Arc<Mutex<PlaybackState>>) {
    let frames = out.len() / channels;
    let mut st = queue.lock().unwrap();

    if st.stopped {
        for s in out.iter_mut() {
            *s = 0.0;
        }
        return;
    }

    // Source samples consumed per one device output frame.
    let step = st.source_rate as f64 / st.device_rate as f64;

    let mut pos = st.playhead;

    for frame in 0..frames {
        let idx = pos as usize;
        let frac = (pos - idx as f64) as f32;
        let s0 = st.buffer.get(idx).copied().unwrap_or(0.0);
        let s1 = st.buffer.get(idx + 1).copied().unwrap_or(0.0);
        let mut sample = s0 + (s1 - s0) * frac;

        // DC blocker: first-order high-pass filter at ~20Hz
        // y[n] = x[n] - x[n-1] + R * y[n-1],  R ≈ 0.995 at 44100Hz
        let r = 1.0 - (100.0 / st.device_rate as f32);
        let dc_out = sample - st.dc_prev_in + r * st.dc_prev_out;
        st.dc_prev_in = sample;
        st.dc_prev_out = dc_out;
        sample = dc_out;

        for ch in 0..channels {
            out[frame * channels + ch] = sample;
        }
        pos += step;
    }

    st.playhead = pos;

    // Drop fully consumed source samples from the front of the queue.
    let drain = st.playhead as usize;
    if drain > 0 {
        for _ in 0..drain {
            st.buffer.pop_front();
        }
        st.playhead -= drain as f64;
    }
}
