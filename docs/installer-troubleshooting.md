# Installer Troubleshooting

This guide covers common shell-install issues on macOS/Linux.

## `hermes-ultra: command not found`

1. Verify the binary exists and is executable:

```bash
ls -l ~/.local/bin/hermes-agent-ultra
```

2. Ensure your shell PATH includes the install directory:

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
command -v hermes-ultra
command -v hermes-agent-ultra
command -v hermes
```

3. If you installed to another path, substitute that path in the export line.

4. If your shell still resolves an old PATH after sourcing, reload a fresh login shell:

```bash
exec zsh -l
command -v hermes-ultra
```

## Should I `chmod` the entire repo?

No. Do not chmod the whole repository.

Only set execute bits on files that must be executable, for example:

```bash
chmod +x scripts/install.sh
chmod +x ~/.local/bin/hermes-ultra
chmod +x ~/.local/bin/hermes-agent-ultra
```

## Why does `ls` appear in macOS Privacy/Security as an app?

macOS TCC tracks binaries that request protected resources. `ls` is a binary
(`/bin/ls`), and Terminal shells invoke it directly, so it can appear as a
tracked executable in Privacy settings. This is expected behavior.

If `type -a ls` shows an alias, macOS still records the underlying binary
execution (`/bin/ls`).

## `Operation not permitted` when running shell scripts

Common causes:
- Script is not executable (`chmod +x script.sh`).
- Running from a protected directory without granted access.
- A shell profile or script has restrictive ownership/permissions.

Minimal check:

```bash
sh scripts/install.sh
```

If this works while `./scripts/install.sh` fails, fix execute permission on the
script file only.

## Can setup import keys from a Python/OpenClaw install?

Yes. `hermes-ultra setup` (or `hermes setup`) now detects common legacy `.env` locations (for example
`~/Documents/Projects/hermes-agent/.env` and `~/.openclaw/.env`) and offers to
import missing keys into `~/.hermes-agent-ultra/.env`.

You can also point setup at a specific `.env` source:

```bash
HERMES_SETUP_IMPORT_ENV_PATH=/absolute/path/to/.env hermes-ultra setup
```
