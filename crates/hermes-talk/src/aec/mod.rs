use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tracing::info;

use crate::config::AecConfig;

/// Shared ring buffer of recent playback audio at 16kHz f32, used as AEC reference.
pub type AecRefBuf = Arc<Mutex<VecDeque<f32>>>;

pub fn create_ref_buf(sample_rate: u32, max_delay_ms: u32) -> AecRefBuf {
    let cap = (sample_rate as usize).saturating_mul(max_delay_ms.max(200) as usize / 1000);
    Arc::new(Mutex::new(VecDeque::with_capacity(cap)))
}

pub fn push_ref(ref_buf: &AecRefBuf, samples: &[f32], sample_rate: u32, max_delay_ms: u32) {
    let cap = (sample_rate as usize).saturating_mul(max_delay_ms.max(200) as usize / 1000);
    let mut buf = ref_buf.lock().unwrap();
    for &s in samples {
        if buf.len() >= cap {
            buf.pop_front();
        }
        buf.push_back(s);
    }
}

/// Engine running on a dedicated std::thread. Aec is !Send so it must stay put.
pub struct AecEngine {
    inner: Option<aec_rs::Aec>,
    frame_size: usize,
    mic_buf: Vec<i16>,
    ref_buf_shared: AecRefBuf,
    max_ref_samples: usize,
    enabled: bool,
}

impl AecEngine {
    pub fn new(cfg: &AecConfig, ref_buf: AecRefBuf) -> Self {
        let enabled = cfg.enabled;
        let inner = if enabled {
            let config = aec_rs::AecConfig {
                frame_size: cfg.frame_size,
                filter_length: cfg.filter_length,
                sample_rate: 16000,
                enable_preprocess: cfg.enable_preprocess,
            };
            let aec = aec_rs::Aec::new(&config);
            info!(
                frame_size = cfg.frame_size,
                filter_length = cfg.filter_length,
                "aec engine started"
            );
            Some(aec)
        } else {
            None
        };
        Self {
            inner,
            frame_size: cfg.frame_size,
            mic_buf: Vec::new(),
            ref_buf_shared: ref_buf,
            max_ref_samples: cfg.filter_length as usize + cfg.frame_size,
            enabled,
        }
    }

    /// Process a mic chunk through AEC. Returns cleaned f32 samples.
    /// If AEC is disabled, returns the input unchanged.
    pub fn process(&mut self, mic: &[f32]) -> Vec<f32> {
        if !self.enabled || self.inner.is_none() {
            return mic.to_vec();
        }

        let aec = self.inner.as_ref().unwrap();

        // Convert f32 → i16 and accumulate
        for &s in mic {
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            self.mic_buf.push(v);
        }

        // Process full frames
        let mut out_f32 = Vec::with_capacity(self.mic_buf.len());
        while self.mic_buf.len() >= self.frame_size {
            let mic_frame: Vec<i16> = self.mic_buf.drain(..self.frame_size).collect();

            // Get reference frame from ring buffer
            let ref_frame: Vec<i16> = {
                let buf = self.ref_buf_shared.lock().unwrap();
                if buf.len() < self.frame_size {
                    // Not enough reference history; pass through raw mic
                    out_f32.extend(mic_frame.iter().map(|&s| s as f32 / i16::MAX as f32));
                    continue;
                }
                let start = buf.len().saturating_sub(self.frame_size);
                buf.iter()
                    .skip(start)
                    .take(self.frame_size)
                    .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                    .collect()
            };

            let mut out_frame = vec![0i16; self.frame_size];
            aec.cancel_echo(&mic_frame, &ref_frame, &mut out_frame);

            out_f32.extend(out_frame.iter().map(|&s| s as f32 / i16::MAX as f32));
        }

        // Trim reference buffer (keep at most filter_length + frame_size)
        {
            let mut buf = self.ref_buf_shared.lock().unwrap();
            while buf.len() > self.max_ref_samples {
                buf.pop_front();
            }
        }

        out_f32
    }
}
