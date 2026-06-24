use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use cpal::SampleFormat;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tracing::info;

use crate::audio::pcm::f32_to_i16_le;
use crate::config::AudioConfig;
use crate::error::{DemoError, Result};

const TARGET_RATE: u32 = 16000;

/// Linear resampler that accepts variable-sized input (cpal callbacks).
pub struct LinearResampler {
    step: f64,
    pos: f64,
    tail: Vec<f32>,
}

impl LinearResampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            step: from_rate as f64 / to_rate as f64,
            pos: 0.0,
            tail: Vec::new(),
        }
    }

    pub fn push(&mut self, input: &[f32]) -> Vec<f32> {
        self.tail.extend_from_slice(input);
        let mut out = Vec::new();
        while self.pos + 1.0 < self.tail.len() as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let s0 = self.tail[i];
            let s1 = self.tail[i + 1];
            out.push(s0 + (s1 - s0) * frac);
            self.pos += self.step;
        }
        let consumed = (self.pos as usize).min(self.tail.len());
        if consumed > 0 {
            self.tail.drain(..consumed);
            self.pos -= consumed as f64;
        }
        out
    }
}

pub struct AudioChunk {
    pub samples_f32: Vec<f32>,
    pub samples_i16_bytes: Vec<u8>,
}

pub struct AudioCapture {
    _thread: JoinHandle<()>,
    rx: Receiver<AudioChunk>,
}

impl AudioCapture {
    pub fn start(audio_cfg: &AudioConfig, chunk_ms: u32) -> Result<Self> {
        let host = cpal::default_host();
        let device = pick_input_device(&host, audio_cfg)?;
        let config = device
            .default_input_config()
            .map_err(|e| DemoError::Audio(e.to_string()))?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        let chunk_samples_16k = (TARGET_RATE as u64 * chunk_ms as u64 / 1000) as usize;

        info!(
            device = %device.name().unwrap_or_default(),
            rate = sample_rate,
            channels,
            format = ?config.sample_format(),
            chunk_ms,
            "audio capture"
        );

        let (tx, rx) = mpsc::sync_channel::<AudioChunk>(64);
        let stream_config: cpal::StreamConfig = config.clone().into();

        let thread = thread::spawn(move || {
            let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
            let buf = buffer.clone();
            let tx = tx.clone();

            let mut resampler = if (sample_rate as f64 - TARGET_RATE as f64).abs() > 1.0 {
                Some(LinearResampler::new(sample_rate, TARGET_RATE))
            } else {
                None
            };

            let err_fn = |e| eprintln!("capture error: {e}");

            let stream = match config.sample_format() {
                SampleFormat::F32 => device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _| {
                        let mono = interleaved_to_mono(data, channels);
                        on_input(&mono, &buf, &tx, chunk_samples_16k, &mut resampler);
                    },
                    err_fn,
                    None,
                ),
                SampleFormat::I16 => device.build_input_stream(
                    &stream_config,
                    move |data: &[i16], _| {
                        let f: Vec<f32> = data
                            .chunks(channels)
                            .map(|c| {
                                c.iter().map(|&s| s as f32 / i16::MAX as f32).sum::<f32>()
                                    / c.len() as f32
                            })
                            .collect();
                        on_input(&f, &buf, &tx, chunk_samples_16k, &mut resampler);
                    },
                    err_fn,
                    None,
                ),
                SampleFormat::U16 => device.build_input_stream(
                    &stream_config,
                    move |data: &[u16], _| {
                        let f: Vec<f32> = data
                            .chunks(channels)
                            .map(|c| {
                                c.iter()
                                    .map(|&s| (s as f32 - 32768.0) / 32768.0)
                                    .sum::<f32>()
                                    / c.len() as f32
                            })
                            .collect();
                        on_input(&f, &buf, &tx, chunk_samples_16k, &mut resampler);
                    },
                    err_fn,
                    None,
                ),
                other => {
                    eprintln!("unsupported format {other:?}");
                    return;
                }
            };

            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("build stream: {e}");
                    return;
                }
            };
            if let Err(e) = stream.play() {
                eprintln!("play stream: {e}");
                return;
            }
            loop {
                thread::sleep(std::time::Duration::from_secs(1));
            }
        });

        Ok(Self {
            _thread: thread,
            rx,
        })
    }

    pub fn try_recv_chunk(&self) -> Option<AudioChunk> {
        self.rx.try_recv().ok()
    }
}

fn pick_input_device(host: &cpal::Host, cfg: &AudioConfig) -> Result<cpal::Device> {
    if cfg.input_device.is_empty() {
        return host
            .default_input_device()
            .ok_or_else(|| DemoError::Audio("no default input device".into()));
    }
    let name = &cfg.input_device;
    host.input_devices()
        .map_err(|e| DemoError::Audio(e.to_string()))?
        .find(|d| d.name().map(|n| n == *name).unwrap_or(false))
        .ok_or_else(|| DemoError::Audio(format!("input device not found: {name}")))
}

fn interleaved_to_mono(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    data.chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

fn on_input(
    mono: &[f32],
    buffer: &Arc<Mutex<Vec<f32>>>,
    tx: &SyncSender<AudioChunk>,
    chunk_samples: usize,
    resampler: &mut Option<LinearResampler>,
) {
    let samples = if let Some(r) = resampler {
        r.push(mono)
    } else {
        mono.to_vec()
    };
    if samples.is_empty() {
        return;
    }

    let mut buf = buffer.lock().unwrap();
    buf.extend_from_slice(&samples);
    while buf.len() >= chunk_samples {
        let chunk: Vec<f32> = buf.drain(..chunk_samples).collect();
        let bytes = f32_to_i16_le(&chunk);
        let _ = tx.try_send(AudioChunk {
            samples_f32: chunk,
            samples_i16_bytes: bytes,
        });
    }
}
