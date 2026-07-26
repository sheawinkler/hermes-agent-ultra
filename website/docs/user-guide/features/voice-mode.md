---
sidebar_position: 10
title: "Voice Mode"
description: "What the Rust runtime actually ships for speech: text_to_speech, transcription, and the non-interactive /voice status command"
---

# Voice Mode

Hermes Agent Ultra does **not** currently ship a complete interactive voice mode. There is no working CLI push-to-talk loop, no `/voice on|tts|off` spoken-reply engine, and no live Discord voice-channel bot in this runtime.

What *is* available:

| Surface | Status |
|---------|--------|
| **`text_to_speech` tool** | Shipped. Synthesizes audio via implemented TTS providers and returns a media path the gateway can attach. |
| **`transcription` tool** | Shipped as a callable HTTP tool (OpenAI/Nous-managed, Groq, Mistral). Not auto-run on every attachment. |
| **Incoming messaging audio** | Ingress may prepend `[transcription_candidate]`; the model may then call `transcription`. No deterministic auto-STT. |
| **`/voice` CLI/gateway command** | Fixed status only. Returns that no separate interactive voice engine is active — does **not** inspect TTS/STT provider configuration. |

For provider setup, credentials, Piper, and command providers, use the canonical guide: **[Voice & TTS](tts.md)**.

A short practical checklist lives in [Use Voice Mode with Hermes](/docs/guides/use-voice-mode-with-hermes).

## What is not implemented

Do **not** expect these upstream-style flows in the Rust runtime today:

- `/voice on`, `/voice tts`, `/voice off` enabling mic capture or automatic spoken replies
- `Ctrl+B` (or similar) microphone recording with silence detection
- Streaming spoken assistant replies in the CLI/TUI
- Discord voice-channel join/listen/speak (`/voice join`, live VC conversation)
- Silent fallback from unsupported TTS names to Edge/NeuTTS or any other provider

The historical `voice_mode` tool flag and gateway voice-mixer facades are not a user-facing product surface.

## `text_to_speech`

Ask the agent to speak text, or call the tool directly when tools are enabled. Implemented built-ins:

- `openai` — `VOICE_TOOLS_OPENAI_KEY` / `HERMES_OPENAI_API_KEY` / `OPENAI_API_KEY`
- `elevenlabs` — `ELEVENLABS_API_KEY`
- `minimax` — `MINIMAX_API_KEY`
- `piper` — local `piper` binary + `PIPER_MODEL` (no automatic install/download)
- custom **command** providers under `tts.providers.<name>`

Upstream-only names (`edge`, `xai`, `mistral`, `gemini`, `neutts`, `kittentts`) **fail closed** with an explicit unsupported-provider error.

Minimal config:

```yaml
tts:
  provider: openai   # or elevenlabs | minimax | piper | <command name>
```

```bash
# ~/.hermes/.env — only the providers you use
VOICE_TOOLS_OPENAI_KEY=***
ELEVENLABS_API_KEY=***
MINIMAX_API_KEY=***
# PIPER_MODEL=/absolute/path/to/voice.onnx
# PIPER_BINARY=/usr/local/bin/piper
```

Gateway delivery attaches the tool’s audio file to the current turn. Ordinary built-in output is an audio attachment. Native Telegram voice-bubble delivery is only attempted for command providers with `voice_compatible: true` after successful Opus/OGG conversion. Details: [Voice & TTS](tts.md).

## `transcription`

`transcription` is a **callable** speech-to-text tool (`TranscriptionHandler`). Implemented backends are **OpenAI / Nous-managed OpenAI audio**, **Groq**, and **Mistral** over HTTP. There is no shipped local faster-whisper, whisper CLI, local-command STT, or xAI STT in this handler.

Messaging adapters may label inbound audio with **`[transcription_candidate]`**. That is a model hint only — ingress does **not** auto-download and invoke the handler. A transcript appears only when something (the agent tool loop, or a dedicated consumer such as the Teams meeting pipeline) calls the tool successfully.

STT is independent of interactive “voice mode.” You do not need `/voice on` for the tool.

## `/voice` in the CLI

```text
/voice
```

In this runtime the command returns a **fixed status** that no separate interactive voice engine is active. It does **not** inspect or report TTS/STT provider configuration, start a microphone session, toggle TTS auto-speak, or join a Discord voice channel.

## Related docs

- [Voice & TTS](tts.md) — canonical TTS/STT providers and configuration
- [Use Voice Mode with Hermes](/docs/guides/use-voice-mode-with-hermes) — practical checklist for shipped tools
- [Telegram](../messaging/telegram.md) — messaging voice notes and TTS attachment delivery
- [Configuration](../configuration.md)
