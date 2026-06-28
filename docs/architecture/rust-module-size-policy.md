# Rust Module Size Policy

Hermes favors small, cohesive Rust modules with explicit ownership boundaries.
Line count is not architecture by itself, but it is a useful smoke alarm when a
file starts collecting unrelated reasons to change.

## Thresholds

| Lines | Policy |
| --- | --- |
| `< 800` | Ideal for most modules. |
| `800-1,500` | Healthy when the file is cohesive. |
| `1,501-2,500` | Reported pressure; consider extracting tests, helpers, or submodules. |
| `2,501-4,000` | Review pressure; new work should avoid growing the file unless it reduces total complexity. |
| `4,001-5,000` | Exceptional; active refactor candidate. |
| `> 5,000` | Hard failure unless generated, vendored, or explicitly allowlisted with a justification and owner path. |

## Enforcement

The Rust governance test `rust_module_size_policy` scans first-party `.rs`
files across the repository. It excludes vendored/generated dependency trees
such as `third_party/` and build artifacts such as `target/`.

Run it directly:

```bash
cargo test -p hermes-source-parity-tests --test rust_module_size_policy -- --nocapture
```

The test always prints a tier report. It fails only when a first-party Rust file
crosses the 5,000-line hard limit without an entry in
`docs/architecture/rust-module-size-allowlist.txt`.

## Refactor Guidance

- Split by responsibility first, not by arbitrary line chunks.
- Prefer extracting test modules, provider-specific code, protocol codecs, and
  pure helpers before changing runtime behavior.
- Keep public module re-exports stable when splitting live surfaces.
- Add targeted tests for moved behavior before claiming a split is semantics-preserving.
- Do not hide a large live file behind an allowlist unless the exception is
  generated, vendored, or has a concrete follow-up plan.
