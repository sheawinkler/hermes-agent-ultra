# Oh My Pi Ultra Feature Gap Analysis

This ledger maps the 15 requested competitive "illegal-seeming" outcomes to first-class Rust runtime surfaces in Hermes Agent Ultra.

| # | Outcome Class | Hermes Surface | Status | Runtime Proof |
|---|---|---|---|---|
| 1 | Ultra feature benchmark ledger | `ultra-feature-1` | Implemented | Built-in tool returns this matrix as JSON |
| 2 | Hash-anchored edits | `ultra-feature-2` | Implemented | SHA-256 prefix/full hash guard, stale rejection, exact/line-trim/whitespace recovery |
| 3 | Unified resource read | `ultra-feature-3` | Implemented | `file://`, `http(s)://`, `pr://`, `issue://`, `skill://`, `session://`, `memory://`, `agent://`, `conflict://` |
| 4 | Unified resource search | `ultra-feature-4` | Implemented | Regex search over the same URI namespaces |
| 5 | Conflict resolver | `ultra-feature-5` | Implemented | Lists/reads/resolves conflict hunks with backup and dry-run support |
| 6 | LSP surface | `ultra-feature-6` | Implemented lightweight | Diagnostics, symbols, workspace symbols, references, rename preview/apply, code-action preview |
| 7 | DAP/debug surface | `ultra-feature-7` | Implemented probe/packet/connect | Adapter availability, DAP initialize packet, optional TCP initialize, launch/breakpoint plans |
| 8 | Preview/accept queue | `ultra-feature-8` | Implemented | Durable preview cards; accepts `file_write` and `content_replace` directly |
| 9 | Structural AST search/edit | `ultra-feature-9` | Implemented lightweight | Symbol extraction over Rust/Python/JS/TS/Go with guarded replace |
| 10 | Mid-stream rules | `ultra-feature-10` | Implemented | Persistent regex rules with warn/inject/retry/abort verdicts |
| 11 | Advisor watcher | `ultra-feature-11` | Implemented | Deterministic blocker/risk/verification findings plus stream-rule evaluation |
| 12 | Subagent workspaces | `ultra-feature-12` | Implemented | Durable `agent://` artifacts and optional real git worktree creation |
| 13 | Persistent eval kernel | `ultra-feature-13` | Implemented | JS/TS/shell session history with Hermes file read/search helpers; Python intentionally unsupported |
| 14 | Output minimizer | `ultra-feature-14` | Implemented | Error/warning/failure/changed-file/tail extraction |
| 15 | First-run inheritance | `ultra-feature-15` | Implemented | Scans/imports Codex, Claude, Cursor, Windsurf, Gemini, Cline, Copilot, VS Code rules |
| 16 | Public ultra-feature proof | `ultra-feature-16`, `scripts/run-ultra-feature-benchmarks.sh` | Implemented | Deterministic smoke suite and cargo test runner |

## Architecture Notes

- All new tools are Rust-native under `crates/hermes-tools/src/tools/ultra_features.rs`.
- The tools share durable state under the Hermes data directory at `ultra_features/`.
- Heavy external runtimes remain optional. DAP adapters and Node are probed or invoked only when requested.
- Python is not a core dependency and is rejected by `eval_kernel`.

## Verification

Run:

```sh
CARGO_TARGET_DIR=/Volumes/wd_black/hermes-agent-ultra/target scripts/run-ultra-feature-benchmarks.sh
```
