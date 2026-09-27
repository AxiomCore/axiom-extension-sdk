#!/usr/bin/env bash
set -euo pipefail

sdk_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo test --manifest-path "$sdk_root/Cargo.toml" --workspace --locked
cargo package --manifest-path "$sdk_root/Cargo.toml" -p axiom-extension-abi --no-verify --allow-dirty
cargo package --manifest-path "$sdk_root/Cargo.toml" -p axiom-extension-sdk-derive --no-verify --allow-dirty
# Listing stays usable even when the next SDK version's registry dependencies
# have not yet propagated; the protected release workflow packages it fully.
cargo package --manifest-path "$sdk_root/Cargo.toml" -p axiom-extension-sdk --list --allow-dirty >/dev/null
npm ci --prefix "$sdk_root/typescript" --ignore-scripts
npm test --prefix "$sdk_root/typescript"
npm pack "$sdk_root/typescript" --dry-run --json --ignore-scripts >/dev/null
uv build "$sdk_root/python"
uv run --no-project --python 3.12 python -B -m unittest discover -s "$sdk_root/scripts" -p 'test_*.py'
