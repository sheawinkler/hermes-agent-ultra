//! Map `hermes_config` tts/stt blocks to gateway [`VoiceConfig`].

use hermes_config::voice::{SttConfig, TtsConfig};

use crate::voice::{SttProvider, TtsProvider, VoiceConfig, VoiceState};

/// Build gateway voice runtime settings from app config.
pub fn voice_config_from_app(
    tts: Option<&TtsConfig>,
    stt: Option<&SttConfig>,
) -> (VoiceConfig, bool) {
    let stt_cfg = stt.cloned().unwrap_or_default();
    let stt_enabled = stt_cfg.is_enabled();
    let stt_provider = match stt_cfg.default_provider() {
        "deepgram" => SttProvider::DeepgramNova,
        "groq" => SttProvider::Groq,
        "mistral" => SttProvider::Mistral,
        "xai" => SttProvider::Xai,
        "local_command" => SttProvider::LocalCommand,
        other if other.starts_with("http") => SttProvider::Custom(other.to_string()),
        _ => SttProvider::Whisper,
    };

    let tts_provider = tts
        .map(|t| match t.default_provider() {
            "elevenlabs" => TtsProvider::ElevenLabs,
            "openai" | "edge" | "edge_tts" | "edge-tts" | "minimax" | "mistral" | "gemini"
            | "xai" | "piper" => TtsProvider::OpenAi,
            other if other.starts_with("http") => TtsProvider::Custom(other.to_string()),
            _ => TtsProvider::OpenAi,
        })
        .unwrap_or(TtsProvider::OpenAi);

    let language = stt_cfg
        .local
        .as_ref()
        .and_then(|l| l.language.clone())
        .filter(|s| !s.is_empty());

    (
        VoiceConfig {
            state: if stt_enabled {
                VoiceState::ListenOnly
            } else {
                VoiceState::Disabled
            },
            stt_provider,
            tts_provider,
            auto_detect_voice: false,
            language,
        },
        stt_enabled,
    )
}
