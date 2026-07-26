---
sidebar_position: 8
title: "Use Voice Mode with Hermes"
description: "Practical checklist for the shipped text_to_speech and transcription tools — not a full interactive voice product"
---

# Use Voice Mode with Hermes

This guide matches what Hermes Agent Ultra’s **Rust runtime actually ships**. It is the practical companion to [Voice Mode](/docs/user-guide/features/voice-mode) and the canonical [Voice & TTS](/docs/user-guide/features/tts) page.

Interactive “voice mode” is **not complete** here: there is no working mic push-to-talk loop, no `/voice on|tts|off` spoken-reply engine, and no live Discord voice-channel bot. Use the shipped tools instead.

## What you can do today

1. **Synthesize speech** with the `text_to_speech` tool (OpenAI, ElevenLabs, MiniMax, Piper, or a custom command provider).
2. **Transcribe audio** by calling the `transcription` tool (OpenAI/Nous-managed, Groq, or Mistral). Messaging only hints with `[transcription_candidate]` — it does not auto-STT.
3. **Check `/voice`** in the CLI — status only; it reports that no separate voice engine is active.

## Step 1: confirm text chat works

```bash
hermes-ultra
```

Fix provider/auth issues before touching audio tools.

## Step 2: configure a real TTS provider

Implemented built-ins only — see [Voice & TTS](/docs/user-guide/features/tts):

| Provider | Requirement |
|----------|-------------|
| `openai` | `VOICE_TOOLS_OPENAI_KEY`, `HERMES_OPENAI_API_KEY`, or `OPENAI_API_KEY` |
| `elevenlabs` | `ELEVENLABS_API_KEY` |
| `minimax` | `MINIMAX_API_KEY` |
| `piper` | Self-installed `piper` + `PIPER_MODEL` (optional `PIPER_BINARY`) |
| command provider | `tts.providers.<name>` local CLI |

Upstream-only names (`edge`, `xai`, `mistral`, `gemini`, `neutts`, `kittentts`) **fail closed**. Hermes does not silently substitute another provider and does not auto-install Piper or download models.

```yaml
# ~/.hermes/config.yaml
tts:
  provider: openai
  openai:
    voice: alloy
```

```bash
# ~/.hermes/.env
VOICE_TOOLS_OPENAI_KEY=***
# ELEVENLABS_API_KEY=***
# MINIMAX_API_KEY=***
# PIPER_MODEL=/absolute/path/to/voice.onnx
```

Optional local free path after you install Piper yourself:

```yaml
tts:
  provider: piper
```

## Step 3: use `text_to_speech`

With tools enabled, ask the agent to generate speech, or invoke `text_to_speech` with text (and optional `provider` / `voice` / `speed` / `output_path`).

The tool writes an audio file and returns a structured result with a media path. Gateways attach that file on the current turn. Ordinary built-in output is an **audio attachment**, not an automatic “always speak every reply” mode.

For Telegram voice-bubble delivery, use a **command** provider with `voice_compatible: true` and working `ffmpeg` Opus conversion — built-ins do not claim native Opus by default. Details: [Voice & TTS](/docs/user-guide/features/tts).

## Step 4: transcription (STT)

Call the **`transcription`** tool with an **absolute** `audio_path` that already lives under an approved media root. The handler does **not** read `stt:` from `config.yaml`. Providers: **openai** (including Nous-managed OpenAI audio), **groq**, **mistral**. See [Transcription tool](/docs/user-guide/features/tts#transcription-tool-stt).

Tool parameters: `audio_path` (required), optional `provider`, `model`, `response_format`, `language`.

**Path security:** the file must canonicalize to an existing regular file under Hermes media cache roots, `HERMES_MEDIA_CACHE_DIR` (default `/tmp/hermes-media-cache`), or operator `HERMES_MEDIA_ALLOW_DIRS`. Arbitrary workspace paths and symlink escapes are rejected **before** credentials or network. Workspace files are not allowed by default — copy/drop audio into an approved root or extend `HERMES_MEDIA_ALLOW_DIRS`.

```bash
# Optional default when the tool omits provider
# HERMES_STT_PROVIDER=openai   # or STT_PROVIDER=groq | mistral

VOICE_TOOLS_OPENAI_KEY=***
# GROQ_API_KEY=***
# MISTRAL_API_KEY=***

# Approved roots (examples)
# Linux/macOS:
# HERMES_MEDIA_ALLOW_DIRS=/var/hermes/inbound-audio:/mnt/shared/voice-drop
# Windows:
# HERMES_MEDIA_ALLOW_DIRS=D:\hermes\inbound-audio;E:\voice-drop
# HERMES_MEDIA_CACHE_DIR=/var/cache/hermes-media

# Optional: STT_OPENAI_MODEL, STT_GROQ_MODEL, STT_MISTRAL_MODEL,
# STT_OPENAI_BASE_URL, STT_GROQ_BASE_URL, STT_MISTRAL_BASE_URL
```

Inbound messaging audio is **not** automatically transcribed. Platforms may attach audio and prepend `[transcription_candidate]`; the model must call `transcription` (or another explicit consumer must). The Teams pipeline may trust only the private per-run directory it creates beneath its configured temp root; that does not open the shared temp root or workspace. No local faster-whisper / whisper-CLI / `HERMES_LOCAL_STT_COMMAND` / xAI STT path.

You do **not** enable this with `/voice on`.

## Step 5: `/voice` status only

```text
/voice
```

Expect a status line that no separate TUI/CLI voice engine is active. Do not document or rely on:

- `/voice on`, `/voice tts`, `/voice off`
- `Ctrl+B` mic capture / silence detection / continuous listen loops
- automatic spoken replies for every chat message
- Discord `/voice join` or live VC listen/speak

## Optional system packages

Only if a path you use needs them:

```bash
# macOS
brew install ffmpeg        # voice-compatible command TTS / some delivery conversions
brew install portaudio     # only if you bring your own external mic tooling

# Ubuntu/Debian
sudo apt install ffmpeg
# sudo apt install portaudio19-dev
```

`opus` / `espeak-ng` are **not** required for the shipped `text_to_speech` built-ins.

## Common failure modes

| Symptom | Check |
|---------|--------|
| TTS errors naming `edge` / `neutts` / etc. | Switch `tts.provider` to openai, elevenlabs, minimax, piper, or a command provider |
| Missing audio file from the agent | Confirm `text_to_speech` ran and the provider key/model path is valid |
| Piper fails immediately | Set `PIPER_MODEL` (and `PIPER_BINARY` if not on `PATH`); install Piper yourself |
| `/voice` does nothing useful | Expected — status only; no interactive engine |
| Wanted Discord VC bot | Not shipped in this runtime |

## Where to read next

- [Voice Mode feature reference](/docs/user-guide/features/voice-mode)
- [Voice & TTS](/docs/user-guide/features/tts)
- [Telegram setup](/docs/user-guide/messaging/telegram)
- [Configuration](/docs/user-guide/configuration)
