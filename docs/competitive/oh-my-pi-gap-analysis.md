# Oh My Pi Magic Harness Gap Analysis

This ledger maps the 15 requested competitive "illegal-seeming" outcomes to first-class Rust runtime surfaces in Hermes Agent Ultra.

| # | Outcome Class | Hermes Surface | Status | Runtime Proof |
|---|---|---|---|---|
| 1 | Magic benchmark ledger | `magic_benchmark_ledger` | Implemented | Built-in tool returns this matrix as JSON |
| 2 | Hash-anchored edits | `hash_edit` | Implemented | SHA-256 prefix/full hash guard, stale rejection, exact/line-trim/whitespace recovery |
| 3 | Unified resources | `read_resource`, `search_resource` | Implemented | `file://`, `http(s)://`, `pr://`, `issue://`, `skill://`, `session://`, `memory://`, `agent://`, `conflict://` |
| 4 | Conflict resolver | `resolve_conflict` | Implemented | Lists/reads/resolves conflict hunks with backup and dry-run support |
| 5 | LSP surface | `lsp_inspect` | Implemented lightweight | Diagnostics, symbols, workspace symbols, references, rename preview/apply, code-action preview |
| 6 | DAP/debug surface | `debug_probe` | Implemented probe/packet/connect | Adapter availability, DAP initialize packet, optional TCP initialize, launch/breakpoint plans |
| 7 | Preview/accept queue | `transaction_preview` | Implemented | Durable preview cards; accepts `file_write` and `content_replace` directly |
| 8 | Structural AST search/edit | `ast_search` | Implemented lightweight | Symbol extraction over Rust/Python/JS/TS/Go with guarded replace |
| 9 | Mid-stream rules | `stream_rule_guard` | Implemented | Persistent regex rules with warn/inject/retry/abort verdicts |
| 10 | Advisor watcher | `advisor_watch` | Implemented | Deterministic blocker/risk/verification findings plus stream-rule evaluation |
| 11 | Subagent workspaces | `subagent_workspace` | Implemented | Durable `agent://` artifacts and optional real git worktree creation |
| 12 | Persistent eval kernel | `eval_kernel` | Implemented | JS/TS/shell session history with Hermes file read/search helpers; Python intentionally unsupported |
| 13 | Output minimizer | `minimize_output` | Implemented | Error/warning/failure/changed-file/tail extraction |
| 14 | First-run inheritance | `first_run_inherit` | Implemented | Scans/imports Codex, Claude, Cursor, Windsurf, Gemini, Cline, Copilot, VS Code rules |
| 15 | Public magic proof | `magic_benchmark`, `scripts/run-magic-benchmarks.sh` | Implemented | Deterministic smoke suite and cargo test runner |

## Architecture Notes

- All new tools are Rust-native under `crates/hermes-tools/src/tools/magic.rs`.
- The tools share durable state under the Hermes data directory at `magic_harness/`.
- Heavy external runtimes remain optional. DAP adapters and Node are probed or invoked only when requested.
- Python is not a core dependency and is rejected by `eval_kernel`.

## Verification

Run:

```sh
CARGO_TARGET_DIR=/Volumes/wd_black/hermes-agent-ultra/target scripts/run-magic-benchmarks.sh
```
