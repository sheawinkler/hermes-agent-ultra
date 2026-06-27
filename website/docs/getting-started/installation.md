---
sidebar_position: 2
title: "Installation"
description: "Install Hermes Agent Ultra on Linux, macOS, WSL2, or Android via Termux"
---

# Installation

Get Hermes Agent Ultra up and running in under two minutes with the one-line installer.

## Quick Install

### Linux / macOS / WSL2 / Android (Termux)

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | bash
```

That installs `hermes-agent-ultra` plus the shorter `hermes-ultra` command.
It does **not** install or overwrite a legacy `hermes` command unless you opt in
with `INSTALL_LEGACY_ALIAS=true`, so upstream `NousResearch/hermes-agent` and
Hermes Agent Ultra can coexist on the same machine.

Run the first setup flow explicitly when you want it:

```bash
hermes-ultra setup
```

Or opt into setup during install:

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | bash -s -- --setup
```

:::tip Windows users
Hermes Agent Ultra does not ship the upstream Python PowerShell installer.
Use WSL2 and run the POSIX installer above, or install from source with Rust
using the Cargo command below.
:::

### What the Installer Does

The installer prefers the published GitHub Release binary for your platform. If
no matching release asset is available, it falls back to a source build with
Cargo. It then:

- installs the canonical `hermes-agent-ultra` binary
- creates the `hermes-ultra` symlink
- leaves any existing `hermes` command untouched by default
- creates `~/.hermes/SOUL.md` on first install
- runs bounded post-install probes when setup is explicitly requested

#### Install Layout

Where the installer puts binaries depends on whether you're installing as a
normal user, root, or Termux:

| Installer | Binary location | Primary command | Data directory |
|---|---|---|---|
| Per-user (normal) | `~/.local/bin/hermes-agent-ultra` | `~/.local/bin/hermes-ultra` | `~/.hermes/` |
| Root-mode (`sudo curl … \| sudo bash`) | `/usr/local/bin/hermes-agent-ultra` | `/usr/local/bin/hermes-ultra` | `/root/.hermes/` or `$HERMES_HOME` |
| Termux | `$PREFIX/bin/hermes-agent-ultra` | `$PREFIX/bin/hermes-ultra` | `~/.hermes/` |

Set `HERMES_INSTALL_DIR` or pass `--dir` to choose a different binary
directory. Set `HERMES_HOME` or pass `--hermes-home` to choose a different data
directory.

### After Installation

Reload your shell and start chatting:

```bash
source ~/.bashrc   # or: source ~/.zshrc
hermes-ultra       # Start chatting!
```

To reconfigure individual settings later, use the dedicated commands:

```bash
hermes-ultra model          # Choose your LLM provider and model
hermes-ultra tools          # Configure which tools are enabled
hermes-ultra gateway setup  # Set up messaging platforms
hermes-ultra config set     # Set individual config values
hermes-ultra setup          # Or run the full setup wizard to configure everything at once
```

---

## Prerequisites

For release-asset installs, the POSIX installer only needs `curl`, `tar`, and a
normal shell environment. For source-build fallback, install the Rust toolchain
and Git first.

:::info
Hermes Agent Ultra is Rust-first. Do not install the app from PyPI; use the
release installer or Cargo source install.
:::

:::tip Nix users
If you use Nix (on NixOS, macOS, or Linux), there's a dedicated setup path with a Nix flake, declarative NixOS module, and optional container mode. See the **[Nix & NixOS Setup](./nix-setup.md)** guide.
:::

---

## Manual / Developer Installation

If you want to clone the repo and install from source — for contributing,
running from a specific branch, or auditing the full build — use Cargo:

```bash
cargo install --git https://github.com/sheawinkler/hermes-agent-ultra hermes-cli --locked --bin hermes-agent-ultra --bin hermes-ultra
```

For contributor workflows, see the [Development Setup](../developer-guide/contributing.md#development-setup) section in the Contributing guide.

---

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `hermes-ultra: command not found` | Reload your shell (`source ~/.bashrc`) or check PATH |
| `API key not set` | Run `hermes-ultra model` to configure your provider, or `hermes-ultra config set OPENROUTER_API_KEY your_key` |
| Missing config after update | Run `hermes-ultra config check` then `hermes-ultra config migrate` |

For more diagnostics, run `hermes-ultra doctor` — it will tell you exactly what's missing and how to fix it.
