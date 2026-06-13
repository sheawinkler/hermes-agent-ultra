# RustSec Patch Overrides

This directory contains temporary local crate overrides used by the workspace
`[patch.crates-io]` section. They exist only to keep the resolved dependency
graph clear under `cargo audit --deny warnings` while upstream releases catch up.

## Current Overrides

- `matrix-pickle-derive-0.2.2`: removes `proc-macro-error2` and uses `syn::Error`.
- `matrix-sdk-crypto-0.18.0`: removes the doc-only `aquamarine` dependency.
- `tungstenite-0.29.0`: replaces direct `rand` usage with `getrandom`.
- `vodozemac-0.10.0`: replaces `rand 0.8` usage with `rand_core::OsRng`.

The canonical local patch files are in `patches/`. The checked-in crate trees are
trimmed source copies used by Cargo path patches.

## Refresh Flow

Verify the checked-in patched files still match a fresh crates.io download plus
our patch files:

```bash
scripts/refresh-rustsec-patches.sh check
```

Refresh one current patch directory from crates.io:

```bash
scripts/refresh-rustsec-patches.sh refresh tungstenite
```

Trial a newer upstream crate release:

```bash
scripts/refresh-rustsec-patches.sh refresh tungstenite 0.30.0
```

When trialing a new version, update the root `Cargo.toml` `[patch.crates-io]`
path to the new directory before running verification.

## Resolution Criteria

A local override is resolved when the workspace passes without the override:

1. Remove the matching root `Cargo.toml` `[patch.crates-io]` entry.
2. Update the dependency to the upstream version that contains the fix.
3. Run:

```bash
cargo audit --deny warnings
CARGO_TARGET_DIR=/Volumes/wd_black/cargo-targets cargo check --all-targets --all-features
CARGO_TARGET_DIR=/Volumes/wd_black/cargo-targets cargo test --workspace --all-features --no-fail-fast -j 1
```

If those pass, delete the matching crate directory and patch file in this folder.
If any gate fails, keep the override and refresh/rebase the local patch.
