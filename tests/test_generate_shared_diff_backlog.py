import importlib.util
from pathlib import Path


def load_module():
    repo_root = Path(__file__).resolve().parents[1]
    script = repo_root / "scripts" / "generate-shared-diff-backlog.py"
    spec = importlib.util.spec_from_file_location("generate_shared_diff_backlog", script)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def test_longest_prefix_match_prefers_specific_prefix():
    mod = load_module()
    items = [
        {"path": "tests", "classification": "functional"},
        {"path": "tests/gateway", "classification": "policy_only"},
    ]

    match = mod.longest_prefix_match("tests/gateway/test_slack.py", items)

    assert match["path"] == "tests/gateway"


def test_fetch_remote_branch_force_updates_tracking_ref(monkeypatch, tmp_path):
    mod = load_module()
    calls = []

    def fake_run_git(repo_root, args, check=True):
        calls.append((repo_root, args, check))
        return ""

    monkeypatch.setattr(mod, "run_git", fake_run_git)

    mod.fetch_remote_branch(tmp_path, "upstream", "main")

    assert calls == [
        (
            tmp_path,
            [
                "fetch",
                "--no-tags",
                "upstream",
                "+refs/heads/main:refs/remotes/upstream/main",
            ],
            True,
        )
    ]


def test_build_ledger_classifies_shared_different_entries(monkeypatch, tmp_path):
    mod = load_module()
    local_tree = {
        "README.md": "local-readme",
        "tests/gateway/test_a.py": "local-test",
        "tests/unchanged.py": "same",
        "only-local.py": "local-only",
    }
    upstream_tree = {
        "README.md": "upstream-readme",
        "tests/gateway/test_a.py": "upstream-test",
        "tests/unchanged.py": "same",
        "only-upstream.py": "upstream-only",
    }

    def fake_ls_tree_blobs(_repo_root, ref):
        return local_tree if ref == "local" else upstream_tree

    def fake_run_git(_repo_root, args, check=True):
        assert args[0] == "rev-parse"
        return f"{args[1]}-sha"

    monkeypatch.setattr(mod, "ls_tree_blobs", fake_ls_tree_blobs)
    monkeypatch.setattr(mod, "run_git", fake_run_git)

    payload = mod.build_ledger(
        repo_root=tmp_path,
        local_ref="local",
        upstream_ref="upstream",
        classification_items=[
            {
                "path": "README.md",
                "classification": "policy_only",
                "owner": "sheawinkler",
                "ticket": 25,
            },
            {
                "path": "tests/gateway",
                "classification": "functional",
                "owner": "sheawinkler",
                "ticket": 25,
            },
        ],
    )

    assert payload["summary"]["total_shared_different"] == 2
    assert payload["summary"]["cleared_non_runtime"] == 1
    assert payload["summary"]["pending_review"] == 1
    by_path = {entry["path"]: entry for entry in payload["entries"]}
    assert by_path["README.md"]["status"] == "cleared_non_runtime"
    assert by_path["tests/gateway/test_a.py"]["rust_requirement"] == (
        "rust_equivalent_or_contract_test_required"
    )
