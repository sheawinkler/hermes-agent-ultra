---
title: "Windows Guide"
description: "Run Hermes Agent Ultra on Windows through WSL2, or build the Rust CLI natively from source."
sidebar_label: "Windows"
sidebar_position: 3
---

# Windows Guide

Hermes Agent Ultra's supported Windows path is **WSL2**. Install Ubuntu or
another WSL2 distribution, then run the normal POSIX installer inside the Linux
shell:

```bash
curl -fsSL https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh | bash
source ~/.bashrc
hermes-ultra
```

This keeps Hermes in a real POSIX environment for terminal tools, file watches,
PTY behavior, and shell scripts. It also avoids clobbering an upstream
`hermes` install on Windows because Ultra installs `hermes-agent-ultra` and
`hermes-ultra` by default.

See the [Windows WSL2 guide](./windows-wsl-quickstart.md) for setup details.

## Native Windows Status

Hermes Agent Ultra does **not** ship the upstream Python PowerShell installer.
Do not use upstream `install.ps1` instructions for this repo.

Native Windows is available only as an experimental source-build path today:

```powershell
cargo install --git https://github.com/sheawinkler/hermes-agent-ultra hermes-cli --locked --bin hermes-agent-ultra --bin hermes-ultra
hermes-ultra doctor
```

Use this path only if you already have a working Rust toolchain on Windows and
are prepared to debug platform-specific shell/process behavior. For normal
operators, WSL2 is the supported route.

## Coexistence With Upstream Hermes

Ultra intentionally avoids taking over the legacy `hermes` command:

| Command | Owner |
|---|---|
| `hermes-ultra` | Hermes Agent Ultra |
| `hermes-agent-ultra` | Hermes Agent Ultra canonical binary |
| `hermes` | Left untouched unless `INSTALL_LEGACY_ALIAS=true` is set |

That means you can keep upstream `NousResearch/hermes-agent` and Hermes Agent
Ultra installed on the same machine.

## Recommended WSL2 Layout

Keep active projects inside the WSL filesystem, not under `/mnt/c`, for better
file watching and filesystem performance:

```bash
mkdir -p ~/code
cd ~/code
git clone https://github.com/sheawinkler/hermes-agent-ultra.git
```

Use `/mnt/c/...` only when you explicitly need to operate on Windows-side files.

## Troubleshooting

| Symptom | Action |
|---|---|
| `hermes-ultra: command not found` | Reload your WSL shell or ensure `~/.local/bin` is on `PATH`. |
| Terminal tools behave differently from Linux/macOS | Reproduce inside WSL2 before treating it as an Ultra bug. |
| Need to use Windows Chrome from WSL2 | Prefer an MCP bridge such as `chrome-devtools-mcp` launched through Windows. |
| Native Windows build fails | Use WSL2 unless you are actively working on native Windows support. |
