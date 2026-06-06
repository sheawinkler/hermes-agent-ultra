# Contributing to Hermes Agent Ultra

Hermes Agent Ultra is a Rust-first parity and reliability fork of `NousResearch/hermes-agent`. Contributions should preserve that contract: port upstream behavior intentionally, keep Ultra-only extensions explicit, and avoid placeholder runtime paths.

## Getting Started

```bash
git clone https://github.com/sheawinkler/hermes-agent-ultra.git
cd hermes-agent-ultra
cargo build --workspace
cargo test --workspace
```

Install the local CLI while testing end-to-end behavior:

```bash
cargo install --path crates/hermes-cli --force
hermes --version
hermes doctor --deep --snapshot
```

## Development Workflow

- Branch from `main` and open a pull request for repository changes.
- Keep PRs scoped to one feature, fix, parity port, or release-prep concern.
- Follow existing Rust formatting and module layout before introducing new crates or top-level dependencies.
- Use `tracing::{debug, info, warn, error}` for runtime logs; reserve direct stdout for user-facing CLI output.
- Never commit secrets, provider keys, private prompts, or live operator traces.

## Completeness Standard

Do not submit stubs or partially implemented runtime behavior. If a feature cannot be completed in the PR, leave it out or guard it behind a documented, tested error path. Before requesting review, run:

```bash
bash scripts/check-runtime-placeholders.sh
rg -n -i "not yet implemented|TODO: implement|unimplemented!\(|todo!\(|stub-only|placeholder" crates scripts
```

Legitimate future-work notes belong in docs or issues, not in callable runtime branches that pretend to work.

## Required Checks

Run the narrowest relevant tests while developing, then these release-grade checks before merge when the change affects runtime behavior:

```bash
cargo fmt --all --check
bash scripts/clippy-warning-gate.sh --check
bash scripts/check-runtime-placeholders.sh
cargo test --workspace
python3 scripts/run-upstream-slash-parity-gate.py --upstream-ref upstream/main --local-ref HEAD
python3 scripts/run-upstream-surface-coverage-gate.py --repo-root . --upstream-ref upstream/main --local-ref HEAD
```

If your change touches parity governance artifacts, regenerate and commit the affected files under `docs/parity/`.

## Parity Contributions

For upstream parity ports, inspect the upstream source first, then implement the Rust equivalent in the relevant crate. Add or update tests and fixtures when behavior is comparable. Keep intentional divergence documented through the parity queue and surface-coverage allowlists rather than hiding it in code comments.

## Release Contributions

Release PRs should update release notes under `docs/releases/`, keep `Cargo.toml` workspace version accurate, pass the checks above, and tag only from a clean `main` after CI passes.
