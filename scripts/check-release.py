#!/usr/bin/env python3
"""Validate the version, release notes, agent link, and supported release targets."""
import json
import os
from pathlib import Path
import re
import subprocess

root = Path(__file__).resolve().parent.parent
metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=root
))
version = next(p["version"] for p in metadata["packages"] if p["name"] == "enso")
changelog = (root / "CHANGELOG.md").read_text()
assert "## [Unreleased]" in changelog, "Missing Unreleased changelog section"
assert re.search(rf"^## \[{re.escape(version)}\] - \d{{4}}-\d{{2}}-\d{{2}}$", changelog, re.M), "Missing dated notes for the Cargo version"
if os.environ.get("GITHUB_REF_TYPE") == "tag":
    assert os.environ["GITHUB_REF_NAME"] == f"v{version}", "Tag and Cargo version differ"
assert (root / "CLAUDE.md").is_symlink(), "CLAUDE.md must be a symlink"
assert os.readlink(root / "CLAUDE.md") == "AGENTS.md", "CLAUDE.md must link to AGENTS.md"
assert (root / "LICENSE").is_file(), "Missing license"
targets = json.loads(re.search(r"^targets = (\[.*\])$", (root / "dist-workspace.toml").read_text(), re.M)[1])
assert set(targets) == {
    "aarch64-apple-darwin", "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl",
}, "Unexpected release targets"
print(f"Release metadata is consistent for v{version}")
