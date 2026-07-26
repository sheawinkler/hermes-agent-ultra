from __future__ import annotations

import base64
import hashlib
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "generate-upstream-patch-queue.py"


def _load_queue_module():
    spec = importlib.util.spec_from_file_location("patch_queue_local_source", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    return module


def _local_source(repo: Path, *, reviewed_dirty_source: bool = False) -> dict[str, object]:
    local_source, errors = _load_queue_module().build_local_source_record(
        repo,
        reviewed_dirty_source=reviewed_dirty_source,
    )
    assert not errors, errors
    return local_source


def _git(cwd: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=cwd,
        text=True,
        capture_output=True,
        check=check,
    )


def _configure_repo(repo: Path) -> None:
    _git(repo, "config", "user.email", "test@example.com")
    _git(repo, "config", "user.name", "Parity Test")


def _commit(repo: Path, message: str) -> str:
    _git(repo, "add", ".")
    _git(repo, "commit", "-qm", message)
    return _git(repo, "rev-parse", "HEAD").stdout.strip()


def _common_history(tmp_path: Path) -> tuple[Path, Path, Path, Path]:
    seed = tmp_path / "seed"
    seed.mkdir()
    _git(seed, "init", "-q")
    _configure_repo(seed)
    (seed / "base.txt").write_text("base\n", encoding="utf-8")
    _commit(seed, "base")
    _git(seed, "branch", "-M", "main")

    fork = tmp_path / "fork.git"
    upstream = tmp_path / "upstream.git"
    _git(tmp_path, "clone", "-q", "--bare", str(seed), str(fork))
    _git(tmp_path, "clone", "-q", "--bare", str(seed), str(upstream))
    local = tmp_path / "local"
    upstream_work = tmp_path / "upstream-work"
    _git(tmp_path, "clone", "-q", str(fork), str(local))
    _git(tmp_path, "clone", "-q", str(upstream), str(upstream_work))
    _configure_repo(local)
    _configure_repo(upstream_work)
    return local, upstream_work, fork, upstream


def _write_reviewed_anchor(local: Path, upstream: Path, anchor_sha: str | None = None) -> None:
    anchor_sha = anchor_sha or _git(local, "rev-list", "--max-parents=0", "HEAD").stdout.strip()
    docs = local / "docs" / "parity"
    docs.mkdir(parents=True, exist_ok=True)
    evidence_path = docs / "anchor-evidence.json"
    evidence_path.write_text(
        json.dumps(
            {
                "commits": [
                    {
                        "sha": anchor_sha,
                        "disposition": "superseded",
                        "classification_rule": "reviewed_anchor_test_evidence",
                    }
                ]
            }
        ),
        encoding="utf-8",
    )
    evidence_commit = _commit(local, "record anchor evidence")
    (docs / "reviewed-upstream-anchor.json").write_text(
        json.dumps(
            {
                "schema_version": 1,
                "canonical_upstream_url": upstream.resolve().as_uri(),
                "anchor_sha": anchor_sha,
                "reviewed_at_utc": "2026-01-01T00:00:00+00:00",
                "review_basis": {
                    "kind": "test",
                    "semantic_evidence": "fixture evidence",
                },
                "evidence": {
                    "fork_commit": evidence_commit,
                    "artifacts": [
                        {
                            "path": "docs/parity/anchor-evidence.json",
                            "anchor_classification": {
                                "sha": anchor_sha,
                                "disposition": "superseded",
                                "classification_rule": "reviewed_anchor_test_evidence",
                            },
                        }
                    ],
                },
            }
        ),
        encoding="utf-8",
    )
    _commit(local, "record reviewed upstream anchor")
    _git(local, "push", "-q", "origin", "main")


def _run_queue(
    local: Path,
    fork: Path,
    upstream: Path,
    *extra: str,
    anchor_sha: str | None = None,
    prepare_anchor: bool = True,
    canonical_outputs: bool = False,
) -> tuple[subprocess.CompletedProcess[str], dict[str, object], dict[str, object]]:
    if prepare_anchor:
        _write_reviewed_anchor(local, upstream, anchor_sha)
    if canonical_outputs:
        output_dir = local / "docs" / "parity"
        out_json = output_dir / "upstream-missing-queue.json"
        out_md = output_dir / "upstream-missing-queue.md"
        out_ledger = output_dir / "upstream-source-ledger.json"
    else:
        output_dir = local.parent / "queue-output"
        out_json = output_dir / "queue.json"
        out_md = output_dir / "queue.md"
        out_ledger = output_dir / "ledger.json"
    output_dir.mkdir(parents=True, exist_ok=True)
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--repo-root",
            str(local),
            "--local-ref",
            "HEAD",
            "--fork-url",
            fork.resolve().as_uri(),
            "--upstream-url",
            upstream.resolve().as_uri(),
            "--out-json",
            str(out_json),
            "--out-md",
            str(out_md),
            "--out-source-ledger",
            str(out_ledger),
            "--overrides",
            "overrides.json",
            *extra,
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    queue = json.loads(out_json.read_text(encoding="utf-8"))
    ledger = json.loads(out_ledger.read_text(encoding="utf-8"))
    return result, queue, ledger


def _error_codes(payload: dict[str, object]) -> set[str]:
    truth = payload["truth"]
    assert isinstance(truth, dict)
    counts = truth["error_counts"]
    assert isinstance(counts, dict)
    return set(counts)


def test_python_test_surface_detection() -> None:
    import importlib.util

    spec = importlib.util.spec_from_file_location("patch_queue", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    assert module.is_python_test_surface("tests/test_cli.py") is True
    assert module.is_python_test_surface("./test/helpers.py") is True
    assert module.is_python_test_surface("crates/hermes-cli/src/main.rs") is False


def test_commit_is_python_test_only() -> None:
    import importlib.util

    spec = importlib.util.spec_from_file_location("patch_queue", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    assert module.commit_is_python_test_only(["tests/a.py", "tests/b.py"]) is True
    assert module.commit_is_python_test_only(["tests/a.py", "skills/x.md"]) is False
    assert module.commit_is_python_test_only([]) is False


def test_clean_local_source_digest_is_stable_and_queue_generation_stays_verifiable(
    tmp_path: Path,
) -> None:
    local, _, fork, upstream = _common_history(tmp_path)
    first = _local_source(local)
    second = _local_source(local)

    assert first["state"] == "clean"
    assert first["authority"]["release_eligible"] is True
    assert first["tree_state_digest"] == second["tree_state_digest"]
    assert _load_queue_module().validate_local_source_record(first) == []

    result, queue, ledger = _run_queue(local, fork, upstream)
    after_generation = _local_source(local)

    assert result.returncode == 0, result.stdout + result.stderr
    assert queue["truth"]["pass"] is True
    assert ledger["source_valid"] is True
    assert _load_queue_module().json_digest(after_generation) == ledger["local_source_sha256"]
    assert after_generation["tree_state_digest"] == ledger["local_source"]["tree_state_digest"]


def test_staged_content_changes_local_source_digest_without_index_mutation(tmp_path: Path) -> None:
    local, _, _, _ = _common_history(tmp_path)
    clean = _local_source(local)
    (local / "base.txt").write_text("staged\n", encoding="utf-8")
    _git(local, "add", "base.txt")
    index_before = _git(local, "ls-files", "--stage", "--", "base.txt").stdout

    staged = _local_source(local)

    assert _git(local, "ls-files", "--stage", "--", "base.txt").stdout == index_before
    assert staged["index"]["clean"] is False
    assert staged["worktree"]["clean"] is True
    assert staged["authority"]["release_eligible"] is False
    assert staged["tree_state_digest"] != clean["tree_state_digest"]


def test_unstaged_content_changes_local_source_digest_and_release_authority(tmp_path: Path) -> None:
    local, _, _, _ = _common_history(tmp_path)
    clean = _local_source(local)
    (local / "base.txt").write_text("unstaged\n", encoding="utf-8")

    unstaged = _local_source(local)

    assert unstaged["index"]["clean"] is True
    assert unstaged["worktree"]["clean"] is False
    assert unstaged["authority"]["release_eligible"] is False
    assert unstaged["tree_state_digest"] != clean["tree_state_digest"]


def test_untracked_content_changes_local_source_digest_and_release_authority(tmp_path: Path) -> None:
    local, _, _, _ = _common_history(tmp_path)
    clean = _local_source(local)
    (local / "untracked.txt").write_text("untracked\n", encoding="utf-8")

    untracked = _local_source(local)

    assert untracked["untracked"]["clean"] is False
    assert untracked["untracked"]["count"] == 1
    assert untracked["authority"]["release_eligible"] is False
    assert untracked["tree_state_digest"] != clean["tree_state_digest"]


def test_untracked_digest_is_path_safe_and_creation_order_independent(tmp_path: Path) -> None:
    seed = tmp_path / "seed"
    seed.mkdir()
    _git(seed, "init", "-q")
    _configure_repo(seed)
    (seed / "base.txt").write_text("base\n", encoding="utf-8")
    _commit(seed, "base")
    first = tmp_path / "first"
    second = tmp_path / "second"
    _git(tmp_path, "clone", "-q", str(seed), str(first))
    _git(tmp_path, "clone", "-q", str(seed), str(second))
    paths = (Path("z space.txt"), Path("nested") / "line\nname.txt")
    for repo, ordered_paths in ((first, paths), (second, tuple(reversed(paths)))):
        for relative_path in ordered_paths:
            path = repo / relative_path
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"payload:{relative_path}\n", encoding="utf-8")

    first_record = _local_source(first)
    second_record = _local_source(second)

    assert "entries" not in first_record["untracked"]
    assert first_record["untracked"]["entries_sha256"] == second_record["untracked"]["entries_sha256"]
    assert first_record["tree_state_digest"] == second_record["tree_state_digest"]


def test_assume_unchanged_tracked_byte_mutation_fails_closed(tmp_path: Path) -> None:
    local, _, _, _ = _common_history(tmp_path)
    clean = _local_source(local)
    _git(local, "update-index", "--assume-unchanged", "base.txt")
    (local / "base.txt").write_text("hidden mutation\n", encoding="utf-8")

    hidden_mutation = _local_source(local)

    assert hidden_mutation["index"]["hidden_flags_detected"] is True
    assert hidden_mutation["index"]["hidden_flag_count"] > 0
    assert "hidden_flags" not in hidden_mutation["index"]
    assert hidden_mutation["worktree"]["clean"] is False
    assert hidden_mutation["authority"]["release_eligible"] is False
    assert hidden_mutation["tree_state_digest"] != clean["tree_state_digest"]


def test_public_source_ledger_omits_private_paths_and_content_hashes(tmp_path: Path) -> None:
    local, _, fork, upstream = _common_history(tmp_path)
    _write_reviewed_anchor(local, upstream)
    clean = _local_source(local)
    private_path = Path("private") / "untracked-api-token.txt"
    private_bytes = b"not-for-public-provenance\n"
    private_file = local / private_path
    private_file.parent.mkdir()
    private_file.write_bytes(private_bytes)

    result, _, ledger = _run_queue(
        local,
        fork,
        upstream,
        prepare_anchor=False,
        canonical_outputs=True,
    )

    assert result.returncode == 1
    local_source = ledger["local_source"]
    assert isinstance(local_source, dict)
    for component in ("index", "worktree", "untracked"):
        record = local_source[component]
        assert isinstance(record, dict)
        assert "entries" not in record
        assert "hidden_flags" not in record
    assert local_source["tree_state_digest"] != clean["tree_state_digest"]
    assert _load_queue_module().validate_local_source_record(local_source)

    public_text = (
        (local / "docs/parity/upstream-source-ledger.json").read_text(encoding="utf-8")
        + (local / "docs/parity/upstream-missing-queue.json").read_text(encoding="utf-8")
        + (local / "docs/parity/upstream-missing-queue.md").read_text(encoding="utf-8")
    )
    for private_value in (
        private_path.as_posix(),
        base64.b64encode(private_path.as_posix().encode("utf-8")).decode("ascii"),
        private_bytes.decode("utf-8").strip(),
        hashlib.sha256(private_bytes).hexdigest(),
        str(local.resolve()),
        str(tmp_path.resolve()),
        local.resolve().as_uri(),
        tmp_path.resolve().as_uri(),
    ):
        assert private_value not in public_text


def test_queue_authority_fails_for_staged_unstaged_and_untracked_local_source(
    tmp_path: Path,
) -> None:
    for kind in ("staged", "unstaged", "untracked"):
        case_root = tmp_path / kind
        case_root.mkdir()
        local, _, fork, upstream = _common_history(case_root)
        _write_reviewed_anchor(local, upstream)
        clean = _local_source(local)
        if kind == "staged":
            (local / "base.txt").write_text("staged\n", encoding="utf-8")
            _git(local, "add", "base.txt")
        elif kind == "unstaged":
            (local / "base.txt").write_text("unstaged\n", encoding="utf-8")
        else:
            (local / "untracked.txt").write_text("untracked\n", encoding="utf-8")

        result, queue, ledger = _run_queue(
            local,
            fork,
            upstream,
            prepare_anchor=False,
        )

        assert result.returncode == 1, kind
        assert queue["truth"]["pass"] is False
        assert ledger["source_valid"] is False
        assert "dirty_local_source" in _error_codes(queue)
        assert ledger["local_source"]["authority"]["release_eligible"] is False
        assert ledger["local_source"]["tree_state_digest"] != clean["tree_state_digest"]


def test_malformed_override_is_fail_closed(tmp_path: Path) -> None:
    local, upstream_work, fork, upstream = _common_history(tmp_path)
    (upstream_work / "delta.txt").write_text("upstream\n", encoding="utf-8")
    _commit(upstream_work, "upstream delta")
    _git(upstream_work, "push", "-q", "origin", "main")
    (local / "overrides.json").write_text("{not-json", encoding="utf-8")

    result, queue, ledger = _run_queue(local, fork, upstream)

    assert result.returncode == 1
    assert queue["truth"]["pass"] is False
    assert "malformed_overrides" in _error_codes(queue)
    assert ledger["source_valid"] is False


def test_prefix_sha_override_is_rejected_instead_of_prefix_matched(tmp_path: Path) -> None:
    local, upstream_work, fork, upstream = _common_history(tmp_path)
    (upstream_work / "delta.txt").write_text("upstream\n", encoding="utf-8")
    upstream_sha = _commit(upstream_work, "upstream delta")
    _git(upstream_work, "push", "-q", "origin", "main")
    (local / "overrides.json").write_text(
        json.dumps(
            {
                "sha": {
                    upstream_sha[:12]: {
                        "disposition": "ported",
                        "owner": "test",
                        "notes": "must not be accepted by prefix",
                    }
                }
            }
        ),
        encoding="utf-8",
    )

    result, queue, _ = _run_queue(local, fork, upstream)

    assert result.returncode == 1
    assert queue["commits"][0]["disposition"] == "pending"
    assert "malformed_override_sha" in _error_codes(queue)



def test_intentional_divergence_rejects_unapproved_changed_path(tmp_path: Path) -> None:
    local, upstream_work, fork, upstream = _common_history(tmp_path)
    (upstream_work / "delta.txt").write_text("upstream\n", encoding="utf-8")
    upstream_sha = _commit(upstream_work, "upstream delta")
    _git(upstream_work, "push", "-q", "origin", "main")
    fork_sha = _git(local, "rev-parse", "HEAD").stdout.strip()
    (local / "overrides.json").write_text(
        json.dumps(
            {
                "sha": {
                    upstream_sha: {
                        "disposition": "superseded",
                        "owner": "test",
                        "notes": "unapproved path must remain pending",
                        "provenance": {
                            "upstream_sha": upstream_sha,
                            "fork_sha": fork_sha,
                            "evidence": "fixture provenance",
                        },
                        "semantic_equivalence": {
                            "method": "intentional_divergence",
                            "evidence": "fixture divergence claim",
                        },
                    }
                }
            }
        ),
        encoding="utf-8",
    )

    result, queue, ledger = _run_queue(local, fork, upstream)

    assert result.returncode == 1
    assert queue["commits"][0]["disposition"] == "pending"
    assert "override_divergence_path_unapproved" in _error_codes(queue)
    assert ledger["source_valid"] is False


def test_prior_ported_disposition_is_not_reused(tmp_path: Path) -> None:
    local, upstream_work, fork, upstream = _common_history(tmp_path)
    (upstream_work / "delta.txt").write_text("upstream\n", encoding="utf-8")
    upstream_sha = _commit(upstream_work, "upstream delta")
    _git(upstream_work, "push", "-q", "origin", "main")
    (local / "queue.json").write_text(
        json.dumps({"commits": [{"sha": upstream_sha, "disposition": "ported"}]}),
        encoding="utf-8",
    )

    result, queue, _ = _run_queue(local, fork, upstream)

    assert result.returncode == 1
    assert queue["commits"][0]["sha"] == upstream_sha
    assert queue["commits"][0]["disposition"] == "pending"
    assert "unowned_divergence" in _error_codes(queue)


def test_exact_rebased_patch_id_equivalence_passes(tmp_path: Path) -> None:
    local, upstream_work, fork, upstream = _common_history(tmp_path)
    (upstream_work / "shared.txt").write_text("same semantic patch\n", encoding="utf-8")
    _commit(upstream_work, "upstream patch")
    _git(upstream_work, "push", "-q", "origin", "main")

    (local / "local-only.txt").write_text("unrelated\n", encoding="utf-8")
    _commit(local, "local unrelated")
    (local / "shared.txt").write_text("same semantic patch\n", encoding="utf-8")
    _commit(local, "rebased patch")
    _git(local, "push", "-q", "origin", "main")

    result, queue, ledger = _run_queue(local, fork, upstream)

    assert result.returncode == 0, result.stdout + result.stderr
    assert ledger["source_valid"] is True
    assert queue["truth"]["pass"] is True
    assert queue["commits"][0]["classification_rule"] == "patch_id_equivalence"
    assert queue["commits"][0]["disposition"] == "ported"


def test_ambiguous_patch_mapping_is_fail_closed(tmp_path: Path) -> None:
    local, upstream_work, fork, upstream = _common_history(tmp_path)
    (upstream_work / "base.txt").write_text("patched\n", encoding="utf-8")
    _commit(upstream_work, "upstream patch")
    _git(upstream_work, "push", "-q", "origin", "main")

    (local / "local-only.txt").write_text("unrelated\n", encoding="utf-8")
    _commit(local, "local unrelated")
    (local / "base.txt").write_text("patched\n", encoding="utf-8")
    _commit(local, "first equivalent patch")
    (local / "base.txt").write_text("base\n", encoding="utf-8")
    _commit(local, "revert patch")
    (local / "base.txt").write_text("patched\n", encoding="utf-8")
    _commit(local, "second equivalent patch")
    _git(local, "push", "-q", "origin", "main")

    result, queue, _ = _run_queue(local, fork, upstream)

    assert result.returncode == 1
    assert queue["commits"][0]["disposition"] == "pending"
    assert "ambiguous_patch_mapping" in _error_codes(queue)


def test_divergent_identical_trees_pass_with_reviewed_anchor(tmp_path: Path) -> None:
    fork_seed = tmp_path / "fork-seed"
    upstream_seed = tmp_path / "upstream-seed"
    for index, seed in enumerate((fork_seed, upstream_seed)):
        seed.mkdir()
        _git(seed, "init", "-q")
        _configure_repo(seed)
        (seed / "same.txt").write_text("same tree\n", encoding="utf-8")
        _commit(seed, f"independent root {index}")
        _git(seed, "branch", "-M", "main")
    upstream_anchor = _git(upstream_seed, "rev-parse", "HEAD").stdout.strip()
    _git(upstream_seed, "commit", "--allow-empty", "-qm", "semantically equivalent tree")
    fork = tmp_path / "fork.git"
    upstream = tmp_path / "upstream.git"
    _git(tmp_path, "clone", "-q", "--bare", str(fork_seed), str(fork))
    _git(tmp_path, "clone", "-q", "--bare", str(upstream_seed), str(upstream))
    local = tmp_path / "local"
    _git(tmp_path, "clone", "-q", str(fork), str(local))
    _configure_repo(local)

    result, queue, ledger = _run_queue(
        local,
        fork,
        upstream,
        anchor_sha=upstream_anchor,
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert queue["commits"][0]["classification_rule"] == "tree_content_equivalence"
    assert queue["commits"][0]["disposition"] == "ported"
    assert ledger["merge_base"]["status"] == "absent"
    assert ledger["reviewed_anchor"]["valid"] is True


def test_missing_merge_base_without_reviewed_anchor_is_fail_closed(tmp_path: Path) -> None:
    fork_seed = tmp_path / "fork-seed"
    upstream_seed = tmp_path / "upstream-seed"
    for seed in (fork_seed, upstream_seed):
        seed.mkdir()
        _git(seed, "init", "-q")
        _configure_repo(seed)
        (seed / "root.txt").write_text(f"{seed.name}\n", encoding="utf-8")
        _commit(seed, "independent root")
        _git(seed, "branch", "-M", "main")
    fork = tmp_path / "fork.git"
    upstream = tmp_path / "upstream.git"
    _git(tmp_path, "clone", "-q", "--bare", str(fork_seed), str(fork))
    _git(tmp_path, "clone", "-q", "--bare", str(upstream_seed), str(upstream))
    local = tmp_path / "local"
    _git(tmp_path, "clone", "-q", str(fork), str(local))
    _configure_repo(local)

    result, queue, ledger = _run_queue(
        local,
        fork,
        upstream,
        "--reviewed-anchor",
        "missing-review-anchor.json",
    )

    assert result.returncode == 1
    assert ledger["merge_base"]["status"] == "absent"
    assert "missing_reviewed_anchor" in _error_codes(queue)
    assert "missing_merge_base" in _error_codes(queue)


def test_shallow_or_unfetched_history_is_fail_closed(tmp_path: Path) -> None:
    local_full, upstream_work, fork, upstream = _common_history(tmp_path)
    (upstream_work / "delta.txt").write_text("upstream\n", encoding="utf-8")
    _commit(upstream_work, "upstream delta")
    _git(upstream_work, "push", "-q", "origin", "main")
    shallow = tmp_path / "shallow"
    _git(tmp_path, "clone", "-q", "--depth", "1", fork.resolve().as_uri(), str(shallow))
    _configure_repo(shallow)

    result, queue, _ = _run_queue(shallow, fork, upstream, "--no-fetch")

    assert result.returncode == 1
    codes = _error_codes(queue)
    assert "fetch_required" in codes
    assert "shallow_history" in codes
