#!/usr/bin/env python3
"""Fail when the runtime SDK mirror drifts from this canonical SDK repository.

`axiom-extension-sdk` is the source of truth. `axiom-runtime/extensions`
carries mirrors used by the runtime's own fixtures and gates. Every mirrored
file must equal its canonical source after removing the reviewed, intentional
differences listed in ALLOWED_SNIPPETS; the comparison is exact. Each mirrored
directory must also contain exactly the expected file set.

Usage: python3 -I scripts/check_sdk_mirror.py [--workspace PATH] [--require-vendored]
"""
from __future__ import annotations

import argparse
import difflib
import hashlib
import pathlib
import re
import sys

# (canonical path in this repo, mirror path in axiom-runtime)
PAIRS = [
    ("rust/abi/src/lib.rs", "extensions/abi/src/lib.rs"),
    ("rust/abi/src/value_decode.rs", "extensions/abi/src/value_decode.rs"),
    ("rust/sdk/src/lib.rs", "extensions/sdk/src/lib.rs"),
    ("rust/sdk/src/typed.rs", "extensions/sdk/src/typed.rs"),
    ("rust/sdk/src/async_rt.rs", "extensions/sdk/src/async_rt.rs"),
    ("rust/sdk/src/test.rs", "extensions/sdk/src/test.rs"),
    ("rust/sdk/src/primitives.rs", "extensions/sdk/src/primitives.rs"),
    ("rust/sdk/src/secrets.rs", "extensions/sdk/src/secrets.rs"),
    ("rust/sdk-derive/src/lib.rs", "extensions/sdk-derive/src/lib.rs"),
    ("typescript/src/index.ts", "extensions/typescript/sdk/src/index.ts"),
    ("typescript/src/runner.ts", "extensions/typescript/sdk/src/runner.ts"),
    ("typescript/src/async.ts", "extensions/typescript/sdk/src/async.ts"),
    ("typescript/src/runner-async.ts", "extensions/typescript/sdk/src/runner-async.ts"),
    ("typescript/src/schema.ts", "extensions/typescript/sdk/src/schema.ts"),
    ("typescript/src/schema-runtime.ts", "extensions/typescript/sdk/src/schema-runtime.ts"),
    ("typescript/src/replay.ts", "extensions/typescript/sdk/src/replay.ts"),
    ("python/axiom_extension_sdk/__init__.pyi", "extensions/python/sdk/axiom_extension_sdk/__init__.pyi"),
]

# Reviewed differences. Each snippet is removed from the named side before
# comparison; it must actually be present, so a stale entry also fails.
ALLOWED_SNIPPETS = [
    {
        "mirror": "extensions/sdk/src/lib.rs",
        "side": "mirror",
        "reason": "runtime-only `schema` feature depends on the private axiom-lib compiler",
        "pattern": r"/// Selected descriptor validation is advisory to the guest\..*?\n}\n\n*",
    },
    {
        "mirror": "extensions/abi/src/lib.rs",
        "side": "canonical",
        "reason": "conformance fixture path differs by repository layout",
        "pattern": r'"\.\./fixtures/sdk-interface-conformance-v1\.json"',
    },
    {
        "mirror": "extensions/abi/src/lib.rs",
        "side": "mirror",
        "reason": "conformance fixture path differs by repository layout",
        "pattern": r'"\.\./\.\./\.\./\.\./axiom-lib/fixtures/sdk-interface-conformance-v1\.json"',
    },
]

# Directories whose complete file sets are compared (canonical, mirror).
DIRECTORIES = [
    ("rust/abi/src", "extensions/abi/src"),
    ("rust/sdk/src", "extensions/sdk/src"),
    ("rust/sdk-derive/src", "extensions/sdk-derive/src"),
    ("typescript/src", "extensions/typescript/sdk/src"),
    ("python/axiom_extension_sdk", "extensions/python/sdk/axiom_extension_sdk"),
]

# Canonical files with no mirror counterpart, by review.
CANONICAL_ONLY = {
    "typescript/src/selected.ts": "selected-profile runtime is consumed from the published package only",
    "python/axiom_extension_sdk/__init__.py": "runtime mirror ships typing stubs only",
    "python/axiom_extension_sdk/py.typed": "runtime mirror ships typing stubs only",
}

# Byte copies of canonical SDK files that other repositories embed. The
# axiom-build Python AOT compiler bundles `money.py` as a program module and
# pins its SHA-256 in compile.py `SDK_PROGRAM_MODULES`; both must follow the
# canonical file. Checked when the repository is present in the workspace
# (`--require-vendored` makes its absence an error).
VENDORED = [
    {
        "canonical": "python/axiom_extension_sdk/money.py",
        "repo": "axiom-build",
        "copy": "python/runtime/sdk/axiom_extension_sdk/money.py",
        "pin": "python/compile.py",
    },
]


def files(root: pathlib.Path) -> set[str]:
    return {
        str(path.relative_to(root))
        for path in root.rglob("*")
        if path.is_file() and "__pycache__" not in path.parts and path.name != ".DS_Store"
    }


def strip_allowed(text: str, mirror: str, side: str, problems: list[str]) -> str:
    for entry in ALLOWED_SNIPPETS:
        if entry["mirror"] != mirror or entry["side"] != side:
            continue
        updated, count = re.subn(entry["pattern"], "", text, count=1, flags=re.S)
        if count != 1:
            problems.append(f"{mirror}: allowlisted {side} snippet not found ({entry['reason']}); update the allowlist")
        text = updated
    return text


def main() -> int:
    parser = argparse.ArgumentParser()
    root = pathlib.Path(__file__).resolve().parents[1]
    parser.add_argument("--workspace", type=pathlib.Path, default=root.parent)
    parser.add_argument("--require-vendored", action="store_true", help="fail when a repository holding a vendored copy is absent")
    arguments = parser.parse_args()
    runtime = arguments.workspace / "axiom-runtime"
    problems: list[str] = []
    for canonical, mirror in PAIRS:
        source, copy = root / canonical, runtime / mirror
        if not source.exists() or not copy.exists():
            problems.append(f"missing pair member: {source if not source.exists() else copy}")
            continue
        left = strip_allowed(source.read_text(encoding="utf-8"), mirror, "canonical", problems)
        right = strip_allowed(copy.read_text(encoding="utf-8"), mirror, "mirror", problems)
        if left != right:
            diff = difflib.unified_diff(
                left.splitlines(), right.splitlines(), f"canonical/{canonical}", f"mirror/{mirror}", lineterm="", n=1
            )
            problems.append(f"{mirror} drifted from {canonical}:\n" + "\n".join(list(diff)[:40]))
    paired = {canonical for canonical, _ in PAIRS}
    for canonical_dir, mirror_dir in DIRECTORIES:
        canonical_files = files(root / canonical_dir)
        mirror_files = files(runtime / mirror_dir)
        for name in sorted(mirror_files - canonical_files):
            problems.append(f"mirror-only file is not reviewed: {mirror_dir}/{name}")
        for name in sorted(canonical_files - mirror_files):
            path = f"{canonical_dir}/{name}"
            if path not in CANONICAL_ONLY:
                problems.append(f"canonical file has no mirror and no review entry: {path}")
        for name in sorted(canonical_files & mirror_files):
            path = f"{canonical_dir}/{name}"
            if path not in paired and (root / path).read_bytes() != (runtime / mirror_dir / name).read_bytes():
                problems.append(f"unpaired mirrored file differs: {path}")
    for canonical, reason in CANONICAL_ONLY.items():
        if not (root / canonical).exists():
            problems.append(f"canonical-only entry is stale: {canonical} ({reason})")
    vendored = 0
    for entry in VENDORED:
        repo = arguments.workspace / entry["repo"]
        if not repo.is_dir():
            if arguments.require_vendored:
                problems.append(f"vendored copy not checked: {repo} is absent")
            continue
        vendored += 1
        source, copy = root / entry["canonical"], repo / entry["copy"]
        if not copy.is_file() or copy.read_bytes() != source.read_bytes():
            problems.append(f"{entry['repo']}/{entry['copy']} is not a byte copy of {entry['canonical']}")
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        pin = repo / entry["pin"]
        if not pin.is_file() or digest not in pin.read_text(encoding="utf-8"):
            problems.append(f"{entry['repo']}/{entry['pin']} does not pin sha256 {digest} of {entry['canonical']}")
    for problem in problems:
        print(problem, file=sys.stderr)
    print(f"checked {len(PAIRS)} mirrored files and {vendored} vendored copies; {len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
