"""Hermes home resolution for the Google Meet plugin.

This plugin can be imported in the Rust-first Ultra tree where the upstream
Python ``hermes_constants`` module is intentionally not vendored.
"""

from __future__ import annotations

import os
from pathlib import Path


def get_hermes_home() -> Path:
    """Return the active Hermes home directory."""
    override = os.environ.get("HERMES_HOME", "").strip()
    if override:
        return Path(override)
    return Path.home() / ".hermes"
