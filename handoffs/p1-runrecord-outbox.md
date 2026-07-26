# P1: typed run records, durable outbox, and real dispatch

Baseline commit: `c002f8c4efe475c89c9f14d3995d44e6a5df5b27`
State: corrected, pending independent re-review. P1 implementation is blocked until P0 closure.

## Normative relationship

`handoffs/p1-transaction-design.md` R1–R10 is the single storage, migration, delivery, recovery, and export contract. This handoff names the production integration work; it creates no alternate writer, manifest, mode resolver, delivery state machine, or replay-verification path.

## Required production contract

- Extend `ReplayRecorder` through one typed SQLite/WAL ledger. Keep benchmark `hermes_eval::RunRecord` and gateway in-memory run state distinct. JSONL is only a derived per-run compatibility export, never a commit path or independent writer.
- Preserve exactly six semantic outcomes: `success`, `failure`, `timeout`, `cancellation`, `blocked`, and `dead_lettered`; use closed typed terminal reasons for causes. `unknown_non_resumable` is an explicit effect-reconciliation state, not a seventh outcome.
- Use RFC 8785/JCS-v1 for the immutable envelope and canonical required manifest, include all immutable lineage/provenance/hash-chain fields and a manifest digest, and exclude only `event_hash`.
- Before the first ledger call, resolve the closed typed `ExecutionMode` in P1-A from defaults → profile → config → environment → CLI. Only resolved `manual` may return a typed degraded result; `reviewed`, `autonomous_local`, `ci`, and `remote_worker` modes fail closed.
- `LedgerConnectionFactory` is the only ledger connection path and enforces verified WAL, `synchronous=FULL`, foreign keys, and bounded busy timeout. Its one `BEGIN IMMEDIATE` v0→v1 migration atomically validates schema, writes the version and `ledger_primary` markers, and commits cutover. Unknown/mismatched markers refuse startup; rollback is only an offline verified backup/export restore, never an environment toggle.
- `AppendEvent` supplies a stable append key and typed effect intents, not destinations or a manifest. Before any seal, it computes `AppendSemanticIdentity`: canonical public identity plus a versioned domain-separated HMAC/keyed fingerprint, under a ledger-identity key distinct from artifact encryption, over credential-free private canonical JCS and public identity. The reservation persists public identity plus fingerprint/version/key ID only—never private JCS or raw private digest—and returns a receipt/lease only on an exact tuple match; same key/public with a different private fingerprint conflicts before sealing. Unavailable identity key fails closed with no public-only/unkeyed fallback; only a later unavailable artifact-sealing key/store may use `sanitized_no_seal`. Matching contenders wait/reuse the published verified reference or receipt; `commit_reserved_append` persists the manifest/outbox/event transaction.
- A versioned, fail-closed redactor runs before identity/hashing. SQL/outbox retain only an allowlisted projection. The reservation owner encrypts, writes, file-`fsync`s, digest-checks, renames, and directory-`fsync`s the private CAS artifact before publication/reference. Exact reads verify digest, retention, authorized non-revoked key, AEAD, and plaintext JCS before exposure; missing/corrupt/revoked/deleted material atomically becomes audited unavailable with `exact_replay=false`, never a public fallback. Retirement tombstones before key destruction/unlink and recovers as unavailable.
- Every effect-capable hook, tool, model operation, and callback commits a typed intent before invocation. Resumable work has a required outbox row and uses a registered dedupe-capable handler/receipt. A non-dedupe effect is blocked or uses only the policy-allowlisted `NonResumableEffectExecutor`: a precommitted empty-manifest one-shot, never an outbox handler/claim, no retry after `executing`, and recovery as operator-visible `unknown_non_resumable` plus `failure/effect_non_resumable_unknown`.
- Claims atomically allocate an attempt, lease generation, fresh lease token, and expiry. Completion CASes row ID plus token; it persists the downstream receipt/error, internal `DeliveryResult`, outbox transition, and run state in one transaction. The same run-header reconciler serializes completion with `RunExit`: zero/all-ACK required rows finalize the candidate on exit, while required exhaustion latches `dead_lettered` even before exit and cannot be overwritten.
- Outer `run` and `run_stream` wrappers atomically create run/start records, convert every inner return to closed `RunExit`, and append the exit under the serialized finalizer before returning. A finalizer error overrides a normal result in fail-closed modes; stale work resumes only when safe or becomes `failure/process_death`; zero-row, already-resolved, and early-dead-letter runs cannot strand pending.
- P1 chooses real alpha dispatch: `/mission enqueue` creates `Destination::AlphaRuntimeV1`; `/mission replay` claims durable intent through an exhaustive registered router and durable receipt. `loop_queue.jsonl` and status flips cannot execute or claim replay success; missing/invalid variants NACK and deterministically dead-letter.
- `export_run(run_id)` atomically replaces a full per-run export carrying all immutable envelopes, canonical manifests, sealed references, compatibility aliases, source watermark, total/range, and `complete=true`. Session readers select `latest` or `all` explicitly. Every named replay/proof consumer uses the shared JCS-v1 verifier; bounded tails, incomplete files, stale watermarks, and link-only/nonempty checks are invalid.

## Production integration boundaries

- `crates/hermes-agent/`: ledger types, connection factory, migration, run wrappers/finalizer, transactional append, artifact preparation, outbox claims/completion, and stale-run reconciliation.
- Existing replay/recorder/proof consumers under `crates/hermes-tools/`: one shared JCS-v1 verifier and full run-scoped exports.
- `crates/hermes-cli/src/alpha_runtime/`: typed alpha intent/router/registered production handler; no status-only replay path.
- Gateway and agent-loop hook/tool/model integration points: commit intent before every external effect and close it only through outbox completion.

## Acceptance gates

- Inject every migration, identity-key/HMAC, append-reservation, sealing/publication, exact-artifact read/retirement, claim, completion, finalizer, and export crash boundary. After restart, prove all-or-nothing transaction visibility, one live artifact/reference per semantic append, durable artifact-before-reference ordering, durable unavailable audit/tombstone, no divergent event/effect state, and no accepted partial export. SQL-scan all ledger/reservation/outbox/artifact columns for a unique private sentinel and its raw private SHA-256; both must be absent.
- Prove retry identity under concurrent matching and conflicting requests: one reservation/seal/receipt for equal public identity plus keyed private fingerprint; same key/public with different private JCS gives typed conflict before sealing; unavailable identity key has no fallback; stale reservation/lease tokens cannot commit/ACK. Process death after a resumable side effect yields one durable downstream receipt; a one-shot attempt is never routed through outbox or retried and becomes explicit unknown/non-resumable on uncertainty.
- Race `RunExit` with zero required rows, already-ACKed rows, last ACK, and required dead-letter; prove one exit record, candidate finalization where valid, irreversible dead-letter precedence, and no pending strand. Exercise all normal and early/external exits, stale heartbeat recovery, every `ExecutionMode` resolver conflict, and mandatory-mode ledger failure.
- Exercise real alpha enqueue/replay across restart and missing/invalid routes; prove an actual production consumer receipt or typed dead-letter, never a status-only “replay complete.”
- Verify that every required export consumer rejects tails, incomplete manifests, watermark/count mismatch, non-JCS data, and chain tampering before replay, summary, integrity, or UI success.

- Exercise read-time exact-artifact digest/auth/key/revocation/deletion handling and retention recovery; prove no public-projection fallback or false exact-replay claim.

## Preconditions

Do not start P1 integration before P0 authorization, queue, Discord, and durable-grant closure. Legacy 16-character JSONL chains remain v0 or raw-file-digest evidence; they are not silently upgraded to JCS-v1 provenance.
