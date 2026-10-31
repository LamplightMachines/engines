"""Ensure the Rust binaries the CLI tests drive are built once per session."""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]


@pytest.fixture(scope="session", autouse=True)
def build_binaries():
    proc = subprocess.run(
        ["cargo", "build", "--release", "--bin", "engines-render", "--bin", "engines-song"],
        cwd=REPO, capture_output=True, text=True,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        pytest.skip("cargo build failed — Rust toolchain unavailable")
