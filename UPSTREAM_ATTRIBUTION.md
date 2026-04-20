# Upstream Attribution and Fork Ownership

This repository is maintained as a fork of:
- Upstream: `Lumio-Research/hermes-agent-rs`
- Fork: `sheawinkler/hermes-agent-rs`

## Provenance Anchors
- Upstream remote used locally: `upstream`
- Fork remote used locally: `origin`
- Baseline upstream commit before current parity patching: `40272c9`
- Fork merge commit for parity integration: `fb79b98`

## Ownership Model
1. Upstream history remains credited to upstream contributors.
2. Fork history remains credited to contributors on this fork.
3. The fork contribution license in [`LICENSE`](LICENSE) applies only to
   fork-authored changes.
4. As of 2026-04-20, upstream repository metadata did not include a root
   `LICENSE` file. This fork does not claim to relicense upstream material.

## Audit Commands
Use these commands to separate upstream and fork deltas:

```bash
git log --oneline upstream/main..main
git log --oneline main..upstream/main
git diff --name-status upstream/main...main
```

## Intent
This document preserves clear credit to upstream while distinguishing fork-owned
engineering work and release responsibility.
