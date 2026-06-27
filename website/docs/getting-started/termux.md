---
sidebar_position: 3
title: "Android / Termux"
description: "Run Hermes Agent Ultra directly on an Android phone with Termux"
---

# Hermes Agent Ultra on Android with Termux

Hermes Agent Ultra can run as a phone-native Rust CLI through
[Termux](https://termux.dev/). The supported path is the same POSIX installer
used on Linux and macOS; on Termux it installs binaries into `$PREFIX/bin`.

## One-line Installer

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | bash
```

The installer detects Termux and:

- installs `hermes-agent-ultra` into `$PREFIX/bin`
- creates the shorter `hermes-ultra` command
- leaves any existing `hermes` command untouched unless `INSTALL_LEGACY_ALIAS=true` is set
- creates `~/.hermes/SOUL.md` on first install

After the installer finishes:

```bash
hermes-ultra doctor
hermes-ultra
```

## Manual Source Build

Use this when you want to test a branch or debug release-asset issues.

```bash
pkg update
pkg install -y git rust clang make pkg-config openssl ripgrep ffmpeg

git clone https://github.com/sheawinkler/hermes-agent-ultra.git
cd hermes-agent-ultra
cargo install --path crates/hermes-cli --locked --bin hermes-agent-ultra --bin hermes-ultra
```

Make sure Cargo's bin directory is on your PATH:

```bash
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.profile
source ~/.profile
```

## Recommended Follow-up Setup

Configure a model:

```bash
hermes-ultra model
```

Re-run the full setup wizard later:

```bash
hermes-ultra setup
```

Set keys directly when you already know the provider values:

```bash
hermes-ultra config set OPENROUTER_API_KEY sk-or-...
```

## Optional Packages

Some tools are more useful when Termux packages are present:

```bash
pkg install ripgrep ffmpeg nodejs-lts
```

Node/browser automation on Android is experimental. Docker-based isolation is
not available inside Termux, and Android may suspend long-running background
jobs.

## Troubleshooting

| Symptom | Action |
|---|---|
| `hermes-ultra: command not found` | Confirm `$PREFIX/bin` or `$HOME/.cargo/bin` is on `PATH`. |
| Source build cannot find OpenSSL | Run `pkg install openssl pkg-config` and retry. |
| `hermes-ultra doctor` reports missing `rg` or `ffmpeg` | Run `pkg install ripgrep ffmpeg`. |
| Gateway stops when the phone sleeps | Disable battery optimization for Termux; Android background persistence is best-effort. |

If you hit a new Android-specific issue, include:

- Android version
- `termux-info`
- `rustc --version`
- `hermes-ultra doctor`
- exact install command and full error output
