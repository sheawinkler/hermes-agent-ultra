# Parity Artifacts

This directory stores generated parity reports against the upstream target
(`upstream/main`).

## Regenerate

From repository root:

```bash
python3 scripts/generate-parity-matrix.py
```

By default this command fetches upstream directly from GitHub
(`git fetch upstream --prune`) before computing metrics.

## Outputs

- `parity-matrix.json`: machine-readable summary and top path buckets
- `parity-matrix.md`: human-readable report for planning/review
