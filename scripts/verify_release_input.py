#!/usr/bin/env python3
"""Fail closed if a manual release input differs from committed metadata."""

import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
MANIFESTS = {
    "rust-abi": ("rust/abi/Cargo.toml", "axiom-extension-abi"),
    "rust-derive": ("rust/sdk-derive/Cargo.toml", "axiom-extension-sdk-derive"),
    "rust-sdk": ("rust/sdk/Cargo.toml", "axiom-extension-sdk"),
    "typescript-sdk": ("typescript/package.json", "@axiomcore/extension-sdk"),
    "python-sdk": ("python/pyproject.toml", "axiom-extension-sdk"),
}


def main(component: str, version: str) -> None:
    if component not in MANIFESTS or not re.fullmatch(r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)", version):
        raise SystemExit("invalid package component or stable version")
    path, expected_name = MANIFESTS[component]
    manifest = ROOT / path
    if path.endswith(".json"):
        package = json.loads(manifest.read_text())
    else:
        document = tomllib.loads(manifest.read_text())
        package = document["project" if component == "python-sdk" else "package"]
    if package["name"] != expected_name or package["version"] != version:
        raise SystemExit(f"{manifest}: expected {expected_name}@{version}, found {package['name']}@{package['version']}")
    status = subprocess.run(["git", "status", "--porcelain"], cwd=ROOT, capture_output=True, text=True, check=True)
    if status.stdout.strip():
        raise SystemExit("release source checkout must be clean and committed")
    print(f"verified {expected_name}@{version} from committed source")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("usage: verify_release_input.py COMPONENT VERSION")
    main(sys.argv[1], sys.argv[2])
