# P0-A: local-source provenance and fail-closed release truth

Baseline commit: `c002f8c4efe475c89c9f14d3995d44e6a5df5b27`.

## Completed contract

- `scripts/generate-upstream-patch-queue.py` emits one canonical, public-safe `local_source` record in the source ledger and its SHA-256 reference in the queue.
- Schema version `2` has `schema_version`, `head_sha`, aggregate `index`, `worktree`, and `untracked` records, `generated_evidence_exclusion`, `state`, `authority`, `tree_state_digest_algorithm`, `tree_state_digest`, and `observation_errors`.
- Public `index` has only cleanliness, entry count, aggregate entry digest, hidden-flag count, and hidden-flag presence. Public `worktree` has only cleanliness, entry count, and aggregate entry digest. Public `untracked` has only cleanliness, count, and aggregate entry digest.
- Byte-safe paths, stage metadata, Git object IDs, file kinds/modes, and content hashes are transient implementation inputs only. They are never serialized in `docs/parity/upstream-source-ledger.json`, the queue, global proof, or readiness output.
- `generated_evidence_exclusion` publishes a fixed exclusion-policy schema version and aggregate digest, not an in-record path list.
- `tree_state_digest` is SHA-256 over compact, sorted-key ASCII JSON of the schema version, HEAD SHA, public aggregate snapshots, and the exclusion-policy record. The aggregate digests are derived from canonical, byte-sorted private snapshots, so staged, unstaged, untracked, path, or content changes alter the public tree-state digest without exposing those details. Git queries are read-only (`ls-files`, `ls-tree`, `cat-file`); no temporary index, `git add`, checkout, or index refresh is used.
- Dirty source always has `authority.release_eligible: false`. `--reviewed-dirty-source` records `diagnostic_reviewed_dirty_source`, but does not permit source validity, proof, readiness, or release promotion.
- Public artifact path references are canonical repository-relative paths. External input/output paths and local-file remote identities are redacted; path-bearing error, validation, and resample messages are sanitized before serialization.

## Canonical evidence boundary

The only excluded in-tree generated-evidence paths are:

- `.sync-reports/release-readiness-summary.json`
- `.sync-reports/release-readiness-summary.md`
- `docs/parity/adapter-feature-matrix.json`
- `docs/parity/behavioral-similarity-diff.json`
- `docs/parity/deep-problem-solving-diff.json`
- `docs/parity/divergence-validation.json`
- `docs/parity/global-parity-proof.json`
- `docs/parity/global-parity-proof.md`
- `docs/parity/parity-matrix.json`
- `docs/parity/shared-diff-backlog.json`
- `docs/parity/shared-different-classification.json`
- `docs/parity/sota-harness-matrix.json`
- `docs/parity/test-coverage-audit.json`
- `docs/parity/test-intent-mapping.json`
- `docs/parity/upstream-missing-queue.json`
- `docs/parity/upstream-missing-queue.md`
- `docs/parity/upstream-source-ledger.json`
- `docs/parity/workstream-status.json`

All other in-tree output paths are rejected as noncanonical; callers must write them outside the repository. This prevents a generated queue/proof/readiness artifact from invalidating its own source record while retaining source inputs such as reviewed anchors and queue overrides.

## Downstream binding

- Global proof and readiness load the queue generator's canonical local-source contract, validate the ledger record and queue/proof digest references, independently resample the local source, and fail closed on any digest or tree-state mismatch.
- Queue resamples after writing its evidence and rewrites its ledger/queue as failed if the source changed during generation.
- Queue, global proof, readiness, and Markdown serialize no absolute repository or temporary-root path; global `queue_provenance.ledger_path` is always `docs/parity/upstream-source-ledger.json`.
- The source record's HEAD must equal the freshly fetched fork SHA; commit SHA alone cannot authorize release.

## Focused verification

Command run:

```text
uv run --with pytest pytest tests/test_generate_upstream_patch_queue.py tests/test_generate_global_parity_proof.py tests/test_release_readiness_summary.py -q
```

Result: `36 passed` in `22.68s`.

The isolated temporary-repository tests cover stable clean snapshots and clean generate-to-immediate-verify; staged, unstaged, and untracked content; byte-path/order safety (spaces and newline paths); an assume-unchanged tracked-byte mutation; queue authoritative failure for each dirty class; and global-proof/readiness aggregate-resample mismatch propagation.

Privacy assertions prove that public queue, source-ledger, proof, readiness, and Markdown artifacts omit private untracked names, reversible path encodings, bytes, direct content hashes, local-file remote paths, and the absolute temporary/repository-root sentinel.

No broad evidence was regenerated. The existing dirty `docs/parity/global-parity-proof.json` had only its leaked absolute `ledger_path` value replaced with the canonical relative path.

## Files changed for this slice

- `scripts/generate-upstream-patch-queue.py`
- `scripts/generate-global-parity-proof.py`
- `scripts/generate-release-readiness-summary.py`
- `tests/test_generate_upstream_patch_queue.py`
- `tests/test_generate_global_parity_proof.py`
- `tests/test_release_readiness_summary.py`
- `handoffs/p0-a-parity-provenance.md`
- `docs/parity/global-parity-proof.json` (targeted path sanitization only)

## Current diagnostic state and remaining risks

- This intentionally dirty integration worktree is diagnostic only and cannot produce authoritative release evidence.
- The explicit evidence exclusion set must remain narrow; adding source inputs such as reviewed anchors, queue overrides, or policy thresholds would weaken provenance.
- The snapshot intentionally fails closed for unsupported tracked modes/gitlinks, hash/read races, hidden or skip-worktree index flags, malformed Git output, and raw-byte differences introduced by checkout filters.
- Never classify the raw 15,936 historical commits without a reviewed baseline or patch-ID plus tree-content-equivalence map; that semantic-classification work remains out of scope.
