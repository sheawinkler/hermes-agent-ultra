---
sidebar_position: 9
title: "Voice & TTS"
description: "text_to_speech media generation and the callable transcription tool — not auto-STT across every platform"
---

# Voice & TTS

Hermes Agent Ultra ships **`text_to_speech`** (validated audio file generation) and a callable **`transcription`** HTTP tool. Messaging does **not** auto-run speech-to-text on every voice note.

:::tip Nous Subscribers
If you have a paid [Nous Portal](https://portal.nousresearch.com) subscription, OpenAI TTS is available through the **[Tool Gateway](tool-gateway.md)** without a separate OpenAI API key. Run `hermes model` or `hermes tools` to enable it.
:::

## Text-to-Speech

Hermes Agent Ultra registers `text_to_speech` in the Rust tool runtime. The tool writes an audio file and returns a structured result containing a `MEDIA:<absolute-path>` marker; the active gateway can then deliver that file with the current reply.

> **Rust runtime support:** OpenAI, ElevenLabs, MiniMax, Piper, and user-defined command providers are implemented. Upstream-only names (`edge`, `xai`, `mistral`, `gemini`, `neutts`, and `kittentts`) currently fail closed with an explicit unsupported-provider error. Hermes does not silently substitute another provider.

### Tool contract

| Parameter | Required | Meaning |
|-----------|----------|---------|
| `text` | Yes | Text to synthesize. Provider-specific character limits are enforced. |
| `provider` | No | One implemented built-in or a name under `tts.providers`. Omit it to use `tts.provider`. |
| `voice` | No | Per-call voice or Piper model override. |
| `speed` | No | Per-call speed override. OpenAI accepts `0.25`–`4.0`; MiniMax accepts `0.5`–`2.0`. |
| `output_path` | No | Destination path. Relative paths are resolved to absolute paths before the gateway receives them. Newlines are rejected. |

Provider precedence is:

1. the tool call's `provider`;
2. `tts.provider`;
3. ElevenLabs when `ELEVENLABS_API_KEY` is available;
4. OpenAI otherwise.

Configure `tts.provider` when deterministic provider selection matters.

### Implemented providers

| Provider | Credential or local requirement | Runtime behavior |
|----------|---------------------------------|------------------|
| `openai` | Managed Nous OpenAI-audio access, `VOICE_TOOLS_OPENAI_KEY`, `HERMES_OPENAI_API_KEY`, or `OPENAI_API_KEY` | Calls `/audio/speech` with model `tts-1`; default voice is `alloy`. |
| `elevenlabs` | `ELEVENLABS_API_KEY` | Calls the ElevenLabs HTTP API; `tts.elevenlabs.voice_id` and `model_id` configure the defaults (Rachel / `eleven_multilingual_v2` when omitted). |
| `minimax` | `MINIMAX_API_KEY` | Uses MiniMax `t2a_v2` by default, including configured voice/audio settings and optional `GroupId`. Explicit legacy `.../text_to_speech` endpoints retain their flat request/raw-audio contract. |
| `piper` | A `piper` executable plus `PIPER_MODEL`, or a per-call `voice` model | Runs Piper locally and writes WAV output. |
| custom command | A configured local command on Unix | Runs the configured process tree with bounded time, structural output validation, and quoted placeholder paths. |

All HTTP and command providers enforce the 96 MiB audio ceiling, validate the declared container structure (`mp3`, `wav`, `ogg`, or `flac`), and atomically replace the requested destination only after validation. New artifacts are owner-only; replacement preserves existing restrictive platform security metadata. Error bodies and JSON responses are read through separate 64 MiB bounds. Invalid, oversized, truncated, or mislabeled output fails without replacing an existing destination.

### Basic configuration

Configuration lives under `tts` in `~/.hermes/config.yaml`:

```yaml
tts:
  provider: minimax
  speed: 1.0
  minimax:
    voice_id: English_expressive_narrator
    model: speech-02-hd
    speed: 1.0
    vol: 1.0
    pitch: 0
    # Optional: omit to let MiniMax select its API default emotion.
    # emotion: happy  # happy | sad | angry | fearful | disgusted | surprised | calm | fluent | whisper
    sample_rate: 32000
    bitrate: 128000
    format: mp3
    channel: 1
    # group_id: your-minimax-group
    # base_url: https://api.minimax.io/v1/t2a_v2
```

For `t2a_v2`, `emotion` is optional and omitted from the request unless configured. Valid values are exactly `happy`, `sad`, `angry`, `fearful`, `disgusted`, `surprised`, `calm`, `fluent`, and `whisper`; `neutral` is not a valid `t2a_v2` value and fails before a provider request.

ElevenLabs accepts the same nested configuration shape:

```yaml
tts:
  provider: elevenlabs
  elevenlabs:
    voice_id: 21m00Tcm4TlvDq8ikWAM
    model_id: eleven_multilingual_v2
    # base_url: https://api.elevenlabs.io/v1
    # max_text_length: 10000
```

The `tts_premium` tool and `text_to_speech` use this configured default voice unless a tool call supplies a non-empty `voice`.

Set credentials in the environment:

```bash
export MINIMAX_API_KEY="..."
# Optional alternative to tts.minimax.group_id:
export MINIMAX_GROUP_ID="..."
```

MiniMax applies the explicit tool `voice` and `speed` first, then `tts.minimax.*`, then the global `tts.speed`. A configured `GroupId` is URL-encoded and added only when the endpoint does not already contain one. Known MiniMax chat roots are converted to the current `/v1/t2a_v2` path; an explicit custom proxy path and its query parameters are preserved.

For the legacy endpoint, pin the full URL:

```yaml
tts:
  provider: minimax
  minimax:
    base_url: https://api.minimax.chat/v1/text_to_speech
```

The legacy path uses `{model, text, voice_id}` and expects raw audio. Other MiniMax paths use the current nested `voice_setting` / `audio_setting` request and hex-audio JSON response.

### Piper

Install the Piper CLI and a voice model separately, then point Hermes at them:

```bash
export PIPER_BINARY="/usr/local/bin/piper"   # optional; defaults to piper on PATH
export PIPER_MODEL="/absolute/path/to/en_US-lessac-medium.onnx"
```

Optional environment variables map directly to Piper CLI flags:

| Variable | Piper flag |
|----------|------------|
| `PIPER_CONFIG` | `--config` |
| `PIPER_SPEAKER` | `--speaker` |
| `PIPER_LENGTH_SCALE` | `--length_scale` |
| `PIPER_NOISE_SCALE` | `--noise_scale` |
| `PIPER_NOISE_W` | `--noise_w` |

Hermes does not install Piper or download voice models implicitly.

Piper runs as a local subprocess and is currently unavailable on Windows until its private workspace containment can be guaranteed.

### Custom command providers

Use a command provider for a local or third-party synthesizer that accepts direct argv input and output paths:

```yaml
tts:
  provider: local-custom
  providers:
    local-custom:
      type: command
      command: "/usr/local/bin/my-synth --input {input_path} --output {output_path}"
      output_format: wav
      timeout: 120
      max_text_length: 5000
      voice_compatible: false
```

Hermes writes the input text to a private UTF-8 file, expands the template into direct process arguments, launches the command in its own process group on Unix, and validates the produced file. No shell is invoked. Timeout or cancellation terminates the process tree. Temporary input and intermediate output are removed on both success and failure.

Custom command providers are intentionally unavailable on Windows. The direct-argv template runtime is implemented for Unix; configure an HTTP provider there. Hermes fails closed rather than invoking a shell.

#### Placeholders

| Placeholder | Meaning |
|-------------|---------|
| `{input_path}` | Temporary UTF-8 input file |
| `{text_path}` | Alias for `{input_path}` |
| `{output_path}` | File the command must produce |
| `{format}` | `mp3`, `wav`, `ogg`, or `flac` |
| `{voice}` | Configured/per-call voice, empty when unset |
| `{model}` | Configured model, empty when unset |
| `{speed}` | Resolved speed multiplier |

Templates are whitespace-separated argv words. Placeholder values are direct argv data, not shell-quoted text. Quotes, escapes, substitutions, redirects, pipes, heredocs, newlines, and literal braces (`{{` or `}}`) are rejected; keep the executable a fixed literal.

#### Command-provider options

| Key | Default | Meaning |
|-----|---------|---------|
| `timeout` / `timeout_seconds` | `60` | Whole process-tree deadline in seconds |
| `output_format` | `mp3` | Expected `mp3`, `wav`, `ogg`, or `flac` output |
| `max_text_length` | `15000` | Input cap before template execution |
| `voice` / `model` | empty | Placeholder values |
| `voice_compatible` | `false` | Convert output to Opus/OGG through bounded `ffmpeg` and request native voice delivery |
| `ffmpeg_path` | `ffmpeg` | Converter executable used for voice-compatible output |

Built-in names are reserved and cannot be shadowed by command providers.

### Input limits

Defaults are 4,096 characters for OpenAI, 10,000 for MiniMax, 5,000 for Piper, 15,000 for command providers, and model-specific limits for ElevenLabs. Set `tts.<provider>.max_text_length` (or the command provider's `max_text_length`) to a positive integer to override the default.

### Gateway delivery

- The normalized tool result carries an absolute media path; gateways only scan tool results from the current turn, so old audio is not replayed.
- Ordinary provider output is delivered as an audio attachment.
- A command provider with `voice_compatible: true` adds the voice directive only after successful Opus/OGG conversion.
- File markers inside fenced code are ignored.
- Duplicate media paths in the same turn are delivered once.

## Transcription tool (STT)

`TranscriptionHandler` is a **callable tool** (and is also used directly by the Teams meeting pipeline). It is **not** an automatic ingress pipeline that downloads every messaging voice note and injects a transcript before the model runs.

### Messaging audio attachments

When a platform delivers an audio path, messaging ingress may prepend a **`[transcription_candidate]`** hint so the model knows audio is available. That hint alone does **not** download the file or invoke `transcription`. A transcript appears only if the model (or another caller such as the Teams pipeline) actually calls the tool with reachable audio and a configured provider.

Do **not** expect deterministic “send a voice note → always get text” behavior on Telegram, Discord, Slack, WhatsApp, Signal, or Matrix from the Rust runtime alone.

### Implemented HTTP providers

| Provider | Credential | Notes |
|----------|------------|--------|
| **OpenAI / Nous-managed OpenAI audio** | `VOICE_TOOLS_OPENAI_KEY`, `HERMES_OPENAI_API_KEY`, `OPENAI_API_KEY`, or managed Nous tool-gateway access | OpenAI-compatible speech-to-text HTTP API |
| **Groq** | `GROQ_API_KEY` | Groq Whisper-compatible endpoint |
| **Mistral** | `MISTRAL_API_KEY` | Mistral speech-to-text HTTP API |

There is **no** shipped local `faster-whisper` runtime, no auto-detected local `whisper` CLI fallback, no `HERMES_LOCAL_STT_COMMAND` local-command STT surface, and no xAI Grok STT provider in this Rust `TranscriptionHandler`.

### Tool parameters

`TranscriptionHandler` does **not** read `stt:` from `config.yaml`. Selection is per tool call plus environment variables.

| Parameter | Required | Meaning |
|-----------|----------|---------|
| `audio_path` | Yes | **Absolute** path to an existing regular audio file under an approved media root (see below). Extensions: `wav`, `mp3`, `m4a`, `webm`, `ogg`, `oga`, `flac` |
| `provider` | No | `openai`, `groq`, or `mistral` / `voxtral`. If omitted: `HERMES_STT_PROVIDER` / `STT_PROVIDER`, else first available credential (managed OpenAI audio → OpenAI key → Groq → Mistral) |
| `model` | No | Provider-specific model override (else `STT_*_MODEL` defaults) |
| `response_format` | No | `text`, `json`, or `verbose_json` for OpenAI-compatible providers when sent |
| `language` | No | Optional ISO language hint |

### `audio_path` security (approved roots only)

Before any STT credential lookup or network upload, `TranscriptionHandler` runs `validate_external_media_upload_path`:

1. Path must be **absolute** (after `~` expansion).
2. Path must resolve via **canonicalize** to an **existing regular file** (symlinks are resolved; symlink escapes out of an allow root fail).
3. Canonical path must sit under one of:
   - Hermes media cache roots under `HERMES_HOME` / `~/.hermes` (`audio_cache`, `cache/audio`, and the other built-in `*_cache` / `cache/*` media dirs)
   - `HERMES_MEDIA_CACHE_DIR` (default `/tmp/hermes-media-cache` when unset)
   - Operator **`HERMES_MEDIA_ALLOW_DIRS`** — absolute directories only
4. Deny-listed system/credential locations never become uploadable even if listed as a root.

**Workspace / project files are not allowed by default.** A readable path under the cwd or home directory is rejected unless that directory is an approved root (or you add it via `HERMES_MEDIA_ALLOW_DIRS`). Failures return before provider selection (`Refusing to upload audio outside configured media roots`).

Platform-separated allow-dir examples (also accepts comma-separated entries):

```bash
# Linux / macOS — colon-separated
export HERMES_MEDIA_ALLOW_DIRS="/var/hermes/inbound-audio:/mnt/shared/voice-drop"

# Windows — semicolon-separated
set HERMES_MEDIA_ALLOW_DIRS=D:\hermes\inbound-audio;E:\voice-drop

# Optional cache override
export HERMES_MEDIA_CACHE_DIR=/var/cache/hermes-media
```

**Teams meeting pipeline:** its dedicated consumer creates a private per-run directory beneath `config.tmp_dir` (or `~/.hermes/tmp/teams_pipeline` by default) and calls `execute_with_trusted_roots` with only that run directory. Ordinary agent `transcription` tool calls do **not** inherit the shared temp root or arbitrary workspace trust from Teams.

### Environment variables

```bash
# Default provider when the tool omits `provider`
# HERMES_STT_PROVIDER=openai   # or STT_PROVIDER=groq | mistral

# Credentials (set only what you use)
VOICE_TOOLS_OPENAI_KEY=***     # also HERMES_OPENAI_API_KEY / OPENAI_API_KEY
# GROQ_API_KEY=***
# MISTRAL_API_KEY=***

# Optional model / base URL overrides
# STT_OPENAI_MODEL=whisper-1
# STT_OPENAI_BASE_URL=https://api.openai.com/v1   # or OPENAI_BASE_URL
# STT_GROQ_MODEL=whisper-large-v3-turbo
# STT_GROQ_BASE_URL=https://api.groq.com/openai/v1  # or GROQ_BASE_URL
# STT_MISTRAL_MODEL=voxtral-mini-latest
# STT_MISTRAL_BASE_URL=https://api.mistral.ai/v1    # or MISTRAL_BASE_URL
```

OpenAI audio may also route through the Nous-managed `openai-audio` tool gateway when that path is enabled. Missing credentials / unknown providers fail closed on the tool call; messaging does not invent a local STT backend. See also [Configuration](../configuration.md#speech-to-text-stt) and [environment variables](/docs/reference/environment-variables).

### Related consumers

- **Agent tool calls** — model invokes `transcription` when it chooses to act on a `[transcription_candidate]` or other **approved-root** audio path.
- **Teams meeting pipeline** — may call the same handler for recording fallback when Graph transcripts are missing, passing only its private per-run directory as an extra trusted root for files it wrote (see [Teams Meetings](../messaging/teams-meetings.md)). That exception is pipeline-scoped only.
