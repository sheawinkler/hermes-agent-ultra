mod silero_vad;
mod webrtc_vad;

pub use silero_vad::SileroVad;
pub use webrtc_vad::WebRtcVad;

pub enum VadEngine {
    Silero(SileroVad),
    WebRtc(WebRtcVad),
}

impl EndpointDetector for VadEngine {
    fn feed(&mut self, samples: &[f32]) {
        match self {
            VadEngine::Silero(v) => v.feed(samples),
            VadEngine::WebRtc(v) => v.feed(samples),
        }
    }
    fn trailing_silence_ms(&self) -> u32 {
        match self {
            VadEngine::Silero(v) => v.trailing_silence_ms(),
            VadEngine::WebRtc(v) => v.trailing_silence_ms(),
        }
    }
    fn speech_start(&mut self) -> bool {
        match self {
            VadEngine::Silero(v) => v.speech_start(),
            VadEngine::WebRtc(v) => v.speech_start(),
        }
    }
    fn in_speech(&self) -> bool {
        match self {
            VadEngine::Silero(v) => v.in_speech(),
            VadEngine::WebRtc(v) => v.in_speech(),
        }
    }
    fn user_speaking_during_playback(&self) -> bool {
        match self {
            VadEngine::Silero(v) => v.user_speaking_during_playback(),
            VadEngine::WebRtc(v) => v.user_speaking_during_playback(),
        }
    }
    fn reset_barge_in_state(&mut self) {
        match self {
            VadEngine::Silero(v) => v.reset_barge_in_state(),
            VadEngine::WebRtc(v) => v.reset_barge_in_state(),
        }
    }
    fn last_rms(&self) -> f32 {
        match self {
            VadEngine::Silero(v) => v.last_rms(),
            VadEngine::WebRtc(v) => v.last_rms(),
        }
    }
}

pub trait EndpointDetector {
    fn feed(&mut self, samples: &[f32]);
    fn trailing_silence_ms(&self) -> u32;
    fn speech_start(&mut self) -> bool;
    fn in_speech(&self) -> bool;
    fn user_speaking_during_playback(&self) -> bool {
        self.in_speech()
    }
    fn reset_barge_in_state(&mut self) {}
    fn last_rms(&self) -> f32 {
        0.0
    }
}
