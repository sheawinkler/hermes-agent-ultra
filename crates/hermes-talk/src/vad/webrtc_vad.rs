use webrtc_vad::{Vad, VadMode};

use super::EndpointDetector;

const FRAME_MS: u32 = 30;

pub struct WebRtcVad {
    vad: Vad,
    sample_rate: u32,
    frame_samples: usize,
    pending: Vec<i16>,
    in_speech: bool,
    trailing_silence_ms: u32,
    speech_start_flag: bool,
    speech_frames: u32,
    barge_in_threshold: u32,
    barge_in_frames_held: u32,
    barge_in_sustain: u32,
    last_rms: f32,
}

impl WebRtcVad {
    pub fn new(
        sample_rate: u32,
        barge_in_frames: u32,
        barge_in_sustain: u32,
        vad_mode: u8,
    ) -> Self {
        let mut vad = Vad::new();
        let mode = match vad_mode {
            0 => VadMode::Quality,
            1 => VadMode::LowBitrate,
            2 => VadMode::Aggressive,
            _ => VadMode::VeryAggressive,
        };
        vad.set_mode(mode);
        let frame_samples = (sample_rate as u64 * FRAME_MS as u64 / 1000) as usize;
        Self {
            vad,
            sample_rate,
            frame_samples,
            pending: Vec::new(),
            in_speech: false,
            trailing_silence_ms: 0,
            speech_start_flag: false,
            speech_frames: 0,
            barge_in_threshold: barge_in_frames.max(1),
            barge_in_frames_held: 0,
            barge_in_sustain: barge_in_sustain.max(1),
            last_rms: 0.0,
        }
    }
}

impl EndpointDetector for WebRtcVad {
    fn feed(&mut self, samples: &[f32]) {
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            self.pending.push(v);
        }
        while self.pending.len() >= self.frame_samples {
            let frame: Vec<i16> = self.pending.drain(..self.frame_samples).collect();

            let rms = rms_from_i16(&frame);
            self.last_rms = rms;

            let voice = self.vad.is_voice_segment(&frame).unwrap_or(false);

            if voice {
                if !self.in_speech {
                    self.speech_frames += 1;
                    self.barge_in_frames_held += 1;
                    if self.speech_frames >= self.barge_in_threshold {
                        self.speech_start_flag = true;
                        self.in_speech = true;
                        self.speech_frames = 0;
                    }
                } else {
                    self.speech_frames = 0;
                    self.barge_in_frames_held += 1;
                }
                self.trailing_silence_ms = 0;
            } else {
                self.speech_frames = 0;
                self.barge_in_frames_held = 0;
                self.trailing_silence_ms = self.trailing_silence_ms.saturating_add(FRAME_MS);
                if self.in_speech && self.trailing_silence_ms > FRAME_MS * 2 {
                    self.in_speech = false;
                }
            }
        }
        let _ = self.sample_rate;
    }

    fn trailing_silence_ms(&self) -> u32 {
        self.trailing_silence_ms
    }

    fn speech_start(&mut self) -> bool {
        if self.speech_start_flag {
            self.speech_start_flag = false;
            return true;
        }
        false
    }

    fn in_speech(&self) -> bool {
        self.in_speech
    }

    fn last_rms(&self) -> f32 {
        self.last_rms
    }

    fn reset_barge_in_state(&mut self) {
        self.in_speech = false;
        self.trailing_silence_ms = 0;
        self.speech_start_flag = false;
        self.speech_frames = 0;
        self.barge_in_frames_held = 0;
    }

    fn user_speaking_during_playback(&self) -> bool {
        self.in_speech && self.barge_in_frames_held >= self.barge_in_sustain
    }
}

impl WebRtcVad {}

fn rms_from_i16(frame: &[i16]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = frame.iter().map(|&s| (s as f64) * (s as f64)).sum();
    ((sum_sq / frame.len() as f64).sqrt() / i16::MAX as f64) as f32
}
