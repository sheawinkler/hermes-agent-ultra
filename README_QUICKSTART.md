# Hermes Agent Ultra Quickstart

This guide is the fastest path from install to a working interactive session.

## 1) Install

From source:

```bash
cargo install --git https://github.com/sheawinkler/hermes-agent-ultra hermes-cli --locked --bin hermes-agent-ultra --bin hermes-ultra --bin hermes
hash -r
```

## 2) Run Setup Wizard

```bash
hermes-ultra setup
```

Recommended wizard flow:

1. Choose `Quick setup` (or `Full setup` if you want platform/tool tuning immediately).
2. Select provider first.
3. Complete OAuth/API-key auth for that provider.
4. Pick model from the provider model list.
5. Save config when prompted.

Setup writes config under:

- `~/.hermes-agent-ultra/config.yaml`
- `~/.hermes-agent-ultra/auth/`

## 3) Start Interactive Session

```bash
hermes-ultra
```

Useful in-session commands:

- `/help` — command catalog
- `/model` — interactive provider/model switcher
- `/personality list` — built-in personalities with usage guidance
- `/tools` — tool registry view
- `/about` — build + parity + upstream sync snapshot

## 4) Verify Runtime Health

```bash
hermes-ultra doctor --deep --snapshot
```

Optional support bundle:

```bash
hermes-ultra doctor --deep --snapshot --bundle
```

## 5) Gateway Mode (Optional)

```bash
hermes-ultra gateway --live
```

## 6) Upstream/Parity Visibility

Refresh parity and README sync status:

```bash
python3 scripts/generate-parity-dashboard.py
python3 scripts/generate-readme-sync-status.py
```

Artifacts:

- `docs/parity/PARITY_DASHBOARD.md`
- `README.md` auto-generated live sync block
