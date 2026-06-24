use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};

use super::EndpointDetector;

pub struct SileroVad {
    inner: VoiceActivityDetector,
    in_speech: bool,
    prev_detected: bool,
    speech_start_flag: bool,
    trailing_silence_ms: u32,
    speech_frames_held: u32,
    barge_in_sustain: u32,
    last_rms: f32,
    chunk_ms: u32,
}

impl SileroVad {
    pub fn create(
        model_path: &str,
        sample_rate: i32,
        threshold: f32,
        min_silence_duration: f32,
        min_speech_duration: f32,
        max_speech_duration: f32,
        barge_in_sustain: u32,
        chunk_ms: u32,
    ) -> Option<Self> {
        let silero = SileroVadModelConfig {
            model: Some(model_path.to_string()),
            threshold,
            min_silence_duration,
            min_speech_duration,
            window_size: 0,
            max_speech_duration,
        };
        let cfg = VadModelConfig {
            silero_vad: silero,
            sample_rate,
            num_threads: 1,
            ..Default::default()
        };
        let inner = VoiceActivityDetector::create(&cfg, 60.0)?;
        Some(Self {
            inner,
            in_speech: false,
            prev_detected: false,
            speech_start_flag: false,
            trailing_silence_ms: 0,
            speech_frames_held: 0,
            barge_in_sustain: barge_in_sustain.max(1),
            last_rms: 0.0,
            chunk_ms,
        })
    }
}

impl EndpointDetector for SileroVad {
    fn feed(&mut self, samples: &[f32]) {
        // Track RMS
        if !samples.is_empty() {
            let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
            self.last_rms = ((sum_sq / samples.len() as f64).sqrt()) as f32;
        }

        self.inner.accept_waveform(samples);
        let detected = self.inner.detected();

        // Track barge-in sustained frames
        if detected {
            self.speech_frames_held += 1;
            self.trailing_silence_ms = 0;
        } else {
            self.speech_frames_held = 0;
            self.trailing_silence_ms = self.trailing_silence_ms.saturating_add(self.chunk_ms);
        }

        // Rising edge
        if detected && !self.prev_detected {
            self.speech_start_flag = true;
            self.in_speech = true;
        }
        // Falling edge handled by Silero's min_silence_duration internally;
        // we track in_speech from detected() directly.
        self.in_speech = detected;

        self.prev_detected = detected;
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

    fn user_speaking_during_playback(&self) -> bool {
        self.in_speech && self.speech_frames_held >= self.barge_in_sustain
    }

    fn reset_barge_in_state(&mut self) {
        self.in_speech = false;
        self.prev_detected = false;
        self.trailing_silence_ms = 0;
        self.speech_start_flag = false;
        self.speech_frames_held = 0;
        self.inner.reset();
    }

    fn last_rms(&self) -> f32 {
        self.last_rms
    }
}
