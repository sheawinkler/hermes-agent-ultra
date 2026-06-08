//! Dependency-free PCM voice mixer for gateway voice channels.
//!
//! The mixer produces Discord-native 48 kHz stereo s16le 20 ms frames. It is
//! intentionally independent of Discord client crates: platform adapters can
//! drain `read` into whatever voice transport they own.

use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

pub const SAMPLE_RATE: usize = 48_000;
pub const CHANNELS: usize = 2;
pub const SAMPLE_WIDTH: usize = 2;
pub const FRAME_LENGTH_MS: usize = 20;
pub const SAMPLES_PER_FRAME: usize = SAMPLE_RATE * FRAME_LENGTH_MS / 1000;
pub const FRAME_SIZE: usize = SAMPLES_PER_FRAME * CHANNELS * SAMPLE_WIDTH;
pub const SILENCE_FRAME: [u8; FRAME_SIZE] = [0; FRAME_SIZE];

#[derive(Debug, Clone)]
struct MixerChild {
    pcm: Vec<u8>,
    pos: usize,
    looped: bool,
    gain: f32,
    fade_frames: usize,
    fade_done: usize,
    finished: bool,
}

impl MixerChild {
    fn new(pcm: &[u8], looped: bool, gain: f32, fade_in_ms: usize) -> Self {
        let mut padded = pcm.to_vec();
        let remainder = padded.len() % FRAME_SIZE;
        if remainder != 0 {
            padded.resize(padded.len() + (FRAME_SIZE - remainder), 0);
        }
        Self {
            pcm: padded,
            pos: 0,
            looped,
            gain: sanitize_gain(gain),
            fade_frames: fade_in_ms / FRAME_LENGTH_MS,
            fade_done: 0,
            finished: false,
        }
    }

    fn read_frame(&mut self) -> Option<Vec<f32>> {
        if self.finished {
            return None;
        }
        if self.pos >= self.pcm.len() {
            if self.looped && !self.pcm.is_empty() {
                self.pos = 0;
            } else {
                self.finished = true;
                return None;
            }
        }

        let end = (self.pos + FRAME_SIZE).min(self.pcm.len());
        let mut chunk = self.pcm[self.pos..end].to_vec();
        self.pos += FRAME_SIZE;
        if chunk.len() < FRAME_SIZE {
            chunk.resize(FRAME_SIZE, 0);
        }

        let mut effective_gain = self.gain;
        if self.fade_frames > 0 && self.fade_done < self.fade_frames {
            self.fade_done += 1;
            effective_gain *= self.fade_done as f32 / self.fade_frames as f32;
        }

        Some(
            chunk
                .chunks_exact(2)
                .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]) as f32 * effective_gain)
                .collect(),
        )
    }
}

#[derive(Debug)]
struct VoiceMixerState {
    ambient: Option<MixerChild>,
    speech: Vec<MixerChild>,
    ambient_gain: f32,
    duck_gain: f32,
    speech_gain: f32,
    duck_release_frames: usize,
    duck_release_left: usize,
    speech_active: bool,
    closed: bool,
}

/// Continuous PCM mixer with optional ambient bed and ducked speech overlays.
#[derive(Debug, Clone)]
pub struct VoiceMixer {
    state: Arc<Mutex<VoiceMixerState>>,
}

impl VoiceMixer {
    pub fn new(
        ambient_gain: f32,
        duck_gain: f32,
        speech_gain: f32,
        duck_release_ms: usize,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(VoiceMixerState {
                ambient: None,
                speech: Vec::new(),
                ambient_gain: sanitize_gain(ambient_gain),
                duck_gain: sanitize_gain(duck_gain),
                speech_gain: sanitize_gain(speech_gain),
                duck_release_frames: (duck_release_ms / FRAME_LENGTH_MS).max(1),
                duck_release_left: 0,
                speech_active: false,
                closed: false,
            })),
        }
    }

    /// Mirrors Discord AudioSource semantics: adapters should send raw PCM.
    pub fn is_opus(&self) -> bool {
        false
    }

    pub fn set_ambient(&self, pcm: Option<&[u8]>, gain: Option<f32>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let Some(gain) = gain {
            state.ambient_gain = sanitize_gain(gain);
        }
        let Some(pcm) = pcm.filter(|p| !p.is_empty()) else {
            state.ambient = None;
            return;
        };
        let effective_gain = if state.speech_active {
            state.duck_gain
        } else {
            state.ambient_gain
        };
        state.ambient = Some(MixerChild::new(pcm, true, effective_gain, 200));
    }

    pub fn play_speech(&self, pcm: &[u8], gain: Option<f32>, fade_in_ms: usize) {
        if pcm.is_empty() {
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let child = MixerChild::new(
            pcm,
            false,
            gain.map(sanitize_gain).unwrap_or(state.speech_gain),
            fade_in_ms,
        );
        state.speech.push(child);
        state.speech_active = true;
        state.duck_release_left = 0;
        let duck_gain = state.duck_gain;
        if let Some(ambient) = state.ambient.as_mut() {
            ambient.gain = duck_gain;
        }
    }

    pub fn speech_active(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.speech_active)
            .unwrap_or(false)
    }

    pub fn stop_speech(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.speech.clear();
        begin_duck_release(&mut state);
    }

    pub fn cleanup(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.closed = true;
        state.ambient = None;
        state.speech.clear();
        state.speech_active = false;
    }

    /// Read one mixed 20 ms PCM frame.
    pub fn read(&self) -> Vec<u8> {
        let Ok(mut state) = self.state.lock() else {
            return SILENCE_FRAME.to_vec();
        };
        if state.closed {
            return SILENCE_FRAME.to_vec();
        }

        let mut acc: Option<Vec<f32>> = None;
        if !state.speech.is_empty() {
            let mut live = Vec::new();
            let mut speech = std::mem::take(&mut state.speech);
            for mut child in speech.drain(..) {
                if let Some(frame) = child.read_frame() {
                    add_frame(&mut acc, &frame);
                    live.push(child);
                }
            }
            state.speech = live;
            if state.speech.is_empty() && state.speech_active {
                begin_duck_release(&mut state);
            }
        }

        let speech_active = state.speech_active;
        let duck_release_left = state.duck_release_left;
        let duck_release_frames = state.duck_release_frames;
        let duck_gain = state.duck_gain;
        let ambient_gain = state.ambient_gain;
        let mut release_left_after = state.duck_release_left;
        if let Some(ambient) = state.ambient.as_mut() {
            if duck_release_left > 0 && !speech_active {
                release_left_after = duck_release_left - 1;
                let frac = 1.0 - (release_left_after as f32 / duck_release_frames as f32);
                ambient.gain = duck_gain + (ambient_gain - duck_gain) * frac;
            } else if !speech_active && duck_release_left == 0 {
                ambient.gain = ambient_gain;
            }
            if let Some(frame) = ambient.read_frame() {
                add_frame(&mut acc, &frame);
            }
        }
        state.duck_release_left = release_left_after;

        let Some(samples) = acc else {
            return SILENCE_FRAME.to_vec();
        };
        samples_to_bytes(&samples)
    }
}

impl Default for VoiceMixer {
    fn default() -> Self {
        Self::new(0.18, 0.06, 1.0, 400)
    }
}

fn begin_duck_release(state: &mut VoiceMixerState) {
    state.speech_active = false;
    state.duck_release_left = state.duck_release_frames;
}

fn add_frame(acc: &mut Option<Vec<f32>>, frame: &[f32]) {
    match acc {
        Some(existing) => {
            for (dst, src) in existing.iter_mut().zip(frame.iter()) {
                *dst += *src;
            }
        }
        None => *acc = Some(frame.to_vec()),
    }
}

fn samples_to_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_SIZE);
    for sample in samples.iter().take(SAMPLES_PER_FRAME * CHANNELS) {
        let clamped = sample.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        out.extend_from_slice(&clamped.to_le_bytes());
    }
    out.resize(FRAME_SIZE, 0);
    out
}

fn sanitize_gain(gain: f32) -> f32 {
    if gain.is_finite() {
        gain.clamp(0.0, 4.0)
    } else {
        1.0
    }
}

pub fn synth_ambient_pcm(seconds: f32) -> Vec<u8> {
    let seconds = seconds.clamp(0.1, 30.0);
    let raw_samples = (SAMPLE_RATE as f32 * seconds).round() as usize;
    let frame_count = (raw_samples / SAMPLES_PER_FRAME).max(1);
    let samples = frame_count * SAMPLES_PER_FRAME;
    let seconds = samples as f32 / SAMPLE_RATE as f32;

    let whole_cycle_freq = |target: f32| -> f32 { (target * seconds).round().max(1.0) / seconds };
    let f1 = whole_cycle_freq(110.0);
    let f2 = whole_cycle_freq(110.5);
    let trem = whole_cycle_freq(0.5);

    let mut mono = Vec::with_capacity(samples);
    let mut peak = 0.0_f32;
    for i in 0..samples {
        let t = i as f32 / SAMPLE_RATE as f32;
        let pad = 0.55 * (2.0 * PI * f1 * t).sin() + 0.45 * (2.0 * PI * f2 * t).sin();
        let tremolo = 0.6 + 0.4 * (0.5 * (1.0 + (2.0 * PI * trem * t).sin()));
        let air = 0.03 * (2.0 * PI * 880.0 * t).sin();
        let sample = pad * tremolo + air;
        peak = peak.max(sample.abs());
        mono.push(sample);
    }
    let scale = if peak > f32::EPSILON { 0.5 / peak } else { 0.0 };

    let mut out = Vec::with_capacity(samples * CHANNELS * SAMPLE_WIDTH);
    for sample in mono {
        let s = (sample * scale * i16::MAX as f32).round() as i16;
        for _ in 0..CHANNELS {
            out.extend_from_slice(&s.to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn max_abs_i16(frame: &[u8]) -> i16 {
        frame
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]).unsigned_abs())
            .max()
            .unwrap_or(0)
            .min(i16::MAX as u16) as i16
    }

    fn constant_frame(sample: i16, frames: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(FRAME_SIZE * frames);
        for _ in 0..(SAMPLES_PER_FRAME * CHANNELS * frames) {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    #[test]
    fn frame_geometry_matches_discord_pcm() {
        assert_eq!(SAMPLES_PER_FRAME, 960);
        assert_eq!(FRAME_SIZE, 3840);
        assert_eq!(SILENCE_FRAME.len(), FRAME_SIZE);
        assert!(!VoiceMixer::default().is_opus());
    }

    #[test]
    fn empty_mixer_returns_silence_frames() {
        let mixer = VoiceMixer::default();
        for _ in 0..5 {
            assert_eq!(mixer.read(), SILENCE_FRAME.to_vec());
        }
    }

    #[test]
    fn ambient_loops_and_stays_quiet() {
        let mixer = VoiceMixer::new(0.2, 0.05, 1.0, 200);
        let ambient = synth_ambient_pcm(0.5);
        assert_eq!(ambient.len() % FRAME_SIZE, 0);
        mixer.set_ambient(Some(&ambient), None);
        let peaks: Vec<_> = (0..100).map(|_| max_abs_i16(&mixer.read())).collect();
        assert!(peaks.iter().skip(10).any(|p| *p > 0));
        assert!(peaks.into_iter().max().unwrap_or(0) < (i16::MAX as f32 * 0.5) as i16);
    }

    #[test]
    fn speech_layers_over_ambient_then_releases() {
        let mixer = VoiceMixer::new(0.2, 0.05, 1.0, 200);
        let ambient = synth_ambient_pcm(0.5);
        mixer.set_ambient(Some(&ambient), None);
        let base = (0..10).map(|_| max_abs_i16(&mixer.read())).max().unwrap();
        mixer.play_speech(&constant_frame(20_000, 20), None, 0);
        assert!(mixer.speech_active());
        let speech_peak = (0..15).map(|_| max_abs_i16(&mixer.read())).max().unwrap();
        assert!(speech_peak > base);
        for _ in 0..40 {
            mixer.read();
        }
        assert!(!mixer.speech_active());
    }

    #[test]
    fn clipping_prevents_i16_wraparound() {
        let mixer = VoiceMixer::default();
        let loud = constant_frame(30_000, 1);
        mixer.play_speech(&loud, None, 0);
        mixer.play_speech(&loud, None, 0);
        let out = mixer.read();
        let samples: Vec<i16> = out
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        assert_eq!(samples.iter().copied().max(), Some(i16::MAX));
        assert!(samples.iter().all(|s| *s >= 0));
    }

    #[test]
    fn stop_speech_clears_in_flight() {
        let mixer = VoiceMixer::default();
        mixer.play_speech(&constant_frame(10_000, 20), None, 40);
        assert!(mixer.speech_active());
        mixer.stop_speech();
        mixer.read();
        assert!(!mixer.speech_active());
    }

    #[test]
    fn set_ambient_none_clears_and_cleanup_silences() {
        let mixer = VoiceMixer::default();
        let ambient = synth_ambient_pcm(0.5);
        mixer.set_ambient(Some(&ambient), None);
        mixer.set_ambient(None, None);
        assert_eq!(mixer.read(), SILENCE_FRAME.to_vec());

        mixer.set_ambient(Some(&ambient), None);
        mixer.cleanup();
        assert_eq!(mixer.read(), SILENCE_FRAME.to_vec());
    }

    #[test]
    fn non_frame_aligned_pcm_is_padded() {
        let mixer = VoiceMixer::default();
        mixer.play_speech(&[1, 2, 3], None, 0);
        assert_eq!(mixer.read().len(), FRAME_SIZE);
    }

    #[test]
    fn synthesized_ambient_is_stereo_and_frame_aligned() {
        let pcm = synth_ambient_pcm(1.0);
        assert_eq!(pcm.len() % (CHANNELS * SAMPLE_WIDTH), 0);
        assert_eq!(pcm.len() % FRAME_SIZE, 0);
        assert!(max_abs_i16(&pcm[..FRAME_SIZE]) > 0);
    }
}
