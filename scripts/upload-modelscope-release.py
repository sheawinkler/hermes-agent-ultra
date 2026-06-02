#!/usr/bin/env python3
"""Upload release artifacts to ModelScope dataset."""
import argparse
import json
import os
import sys
from pathlib import Path


ALLOWED_PATTERNS = ["hermes-*.tar.gz", "hermes-*.zip", "checksums.sha256"]


def collect_artifacts(dist_dir: Path) -> list[Path]:
    """Collect release artifacts from dist directory, excluding .sig/.pem/security."""
    artifacts: list[Path] = []
    for pattern in ALLOWED_PATTERNS:
        artifacts.extend(sorted(dist_dir.glob(pattern)))
    # Deduplicate while preserving order
    seen: set[Path] = set()
    unique: list[Path] = []
    for a in artifacts:
        if a not in seen:
            seen.add(a)
            unique.append(a)
    return unique


def build_latest_json(version: str, artifacts: list[Path]) -> dict:
    """Build the latest.json payload."""
    clean_version = version.lstrip("v")
    return {
        "version": clean_version,
        "tag": version,
        "artifacts": [a.name for a in artifacts],
    }


def main():
    parser = argparse.ArgumentParser(description="Upload release to ModelScope")
    parser.add_argument(
        "--repo",
        required=True,
        help="ModelScope dataset repo (e.g. flowy2025/agent)",
    )
    parser.add_argument(
        "--version",
        required=True,
        help="Release version tag (e.g. v0.1.0)",
    )
    parser.add_argument(
        "--dist-dir",
        required=True,
        help="Directory containing release artifacts",
    )
    args = parser.parse_args()

    token = os.environ.get("MODELSCOPE_TOKEN")
    if not token:
        raise SystemExit("ERROR: MODELSCOPE_TOKEN environment variable not set")

    dist_dir = Path(args.dist_dir)
    if not dist_dir.is_dir():
        raise SystemExit(f"ERROR: dist directory not found: {dist_dir}")

    version: str = args.version
    repo: str = args.repo

    # Collect artifacts
    artifacts = collect_artifacts(dist_dir)
    if not artifacts:
        raise SystemExit(f"ERROR: no release artifacts found in {dist_dir}")

    print(f"Found {len(artifacts)} artifact(s) to upload:")
    for a in artifacts:
        print(f"  - {a.name} ({a.stat().st_size:,} bytes)")

    # Build latest.json
    latest = build_latest_json(version, artifacts)
    latest_path = dist_dir / "latest.json"
    latest_path.write_text(json.dumps(latest, indent=2) + "\n", encoding="utf-8")
    print(f"\nGenerated latest.json: {json.dumps(latest)}")

    # Import ModelScope SDK
    try:
        from modelscope.hub.api import HubApi
    except ImportError:
        raise SystemExit(
            "ERROR: modelscope package not installed. Run: pip install modelscope"
        )

    # Authenticate
    api = HubApi()
    api.login(token)
    print(f"\nAuthenticated to ModelScope, uploading to dataset: {repo}")

    # Upload each artifact
    upload_prefix = f"hermes-agent-ultra/{version}"
    success_count = 0
    fail_count = 0

    # Upload artifacts
    for artifact in artifacts:
        remote_path = f"{upload_prefix}/{artifact.name}"
        try:
            api.upload_file(
                repo_id=repo,
                file_path=str(artifact),
                path_in_repo=remote_path,
                repo_type="dataset",
            )
            print(f"  [OK] {artifact.name} -> {remote_path}")
            success_count += 1
        except Exception as e:
            print(f"  [FAIL] {artifact.name}: {e}", file=sys.stderr)
            fail_count += 1

    # Upload latest.json
    remote_latest = "hermes-agent-ultra/latest.json"
    try:
        api.upload_file(
            repo_id=repo,
            file_path=str(latest_path),
            path_in_repo=remote_latest,
            repo_type="dataset",
        )
        print(f"  [OK] latest.json -> {remote_latest}")
        success_count += 1
    except Exception as e:
        print(f"  [FAIL] latest.json: {e}", file=sys.stderr)
        fail_count += 1

    # Summary
    print(f"\nUpload complete: {success_count} succeeded, {fail_count} failed")
    if fail_count > 0:
        raise SystemExit(f"ERROR: {fail_count} file(s) failed to upload")


if __name__ == "__main__":
    main()
