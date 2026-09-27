#!/usr/bin/env python3
"""Install one extension SDK and materialize its editor-visible bindings.

Use --sdk-root for an unpublished local checkout; omit it after the packages
are released. AxiomDeps.toml remains the authority and interface manifest.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib

SDK_VERSION = "0.1.0"
PACKAGE = "@axiomcore/extension-sdk"


def run(*args: str, cwd: Path) -> str:
    result = subprocess.run(args, cwd=cwd, text=True, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(f"{' '.join(args)} failed:\n{result.stderr.strip()}")
    return result.stdout


def write_if_missing(path: Path, content: str) -> None:
    if not path.exists():
        path.write_text(content)


def acore_boundary(project: Path, extension: dict) -> dict[str, dict[str, str]]:
    """Validate declared UI fields against the local .acore state declarations.

    This is a conservative editor preflight, not a substitute for the Acore
    parser or the signed runtime authority intersection. It only accepts the
    simple field syntax it can identify without guessing.
    """
    found: dict[str, dict[str, str]] = {}
    files = [path for path in project.rglob("*.acore") if ".axiom" not in path.parts]
    for path in files:
        scope = None
        for line in path.read_text().splitlines():
            opened = re.match(r"^\s*state\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{\s*$", line)
            if opened:
                scope = opened.group(1)
                found.setdefault(scope, {})
                continue
            if scope and re.match(r"^\s*}\s*$", line):
                scope = None
                continue
            if scope:
                field = re.match(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*:\s*([A-Za-z_][A-Za-z0-9_]*)\b", line)
                if field:
                    previous = found[scope].get(field.group(1))
                    if previous and previous != field.group(2):
                        raise RuntimeError(f"conflicting .acore types for {scope}.{field.group(1)}")
                    found[scope][field.group(1)] = field.group(2)
    permissions = extension.get("permissions", {}).get("ui-state", {})
    if files:
        for scope, grants in permissions.items():
            # An extension may target state supplied by a host or another
            # module. Only validate fields when this project declares scope.
            if scope not in found:
                continue
            for field in grants.get("read", []) + grants.get("write", []):
                if field not in found[scope]:
                    raise RuntimeError(f"AxiomDeps.toml declares {scope}.{field}, but no such .acore state field exists")
    return {scope: found[scope] for scope in permissions if scope in found}


def check_bindings(project: Path, alias: str, language: str, cli: str) -> None:
    configurations = {
        "rust": ("rust-inspect", "bindings", project / ".axiom/ide" / alias / "axiom_bindings.rs"),
        "typescript": ("typescript-inspect", "declarations", project / "node_modules/@axiomcore/extension-bindings/index.d.ts"),
        "python": ("python-inspect", "stubs", project / "axiom_bindings.pyi"),
    }
    action, view, target = configurations[language]
    expected = run(cli, "extensions", action, alias, "--deps", "AxiomDeps.toml", "--view", view, cwd=project)
    if not target.is_file() or target.read_text() != expected:
        raise RuntimeError(f"generated bindings are missing or stale: {target}; rerun setup_project.py")


def check_installed_sdk(project: Path, language: str) -> None:
    if language == "rust":
        manifest = project / "Cargo.toml"
        if not manifest.is_file():
            raise RuntimeError("Rust extension project needs Cargo.toml")
        packages = json.loads(run("cargo", "metadata", "--locked", "--offline", "--format-version", "1",
                                  "--manifest-path", str(manifest), cwd=project))["packages"]
        if not any(package["name"] == "axiom-extension-sdk" and package["version"] == SDK_VERSION
                   for package in packages):
            raise RuntimeError(f"Rust extension project needs installed axiom-extension-sdk=={SDK_VERSION}")
    elif language == "typescript":
        root = project / "node_modules/@axiomcore/extension-sdk"
        manifest = root / "package.json"
        if not manifest.is_file() or json.loads(manifest.read_text()).get("version") != SDK_VERSION:
            raise RuntimeError(f"TypeScript extension project needs installed {PACKAGE}@{SDK_VERSION}")
        if not (root / "dist/index.d.ts").is_file() or not (root / "dist/runner.js").is_file():
            raise RuntimeError("TypeScript extension SDK is missing built declarations or runner")
    elif language == "python":
        site_packages = list((project / ".venv/lib").glob("*/site-packages"))
        for site in site_packages:
            metadata = site / f"axiom_extension_sdk-{SDK_VERSION}.dist-info/METADATA"
            if (site / "axiom_extension_sdk/__init__.pyi").is_file() and metadata.is_file():
                if f"Version: {SDK_VERSION}" in metadata.read_text().splitlines():
                    return
        raise RuntimeError(f"Python extension project needs axiom-extension-sdk=={SDK_VERSION} in .venv")
    else:
        raise RuntimeError(f"unsupported extension language: {language}")


def rust(project: Path, alias: str, source: Path, sdk: Path | None, cli: str) -> None:
    manifest = project / "Cargo.toml"
    if not manifest.exists():
        relative_sdk = os.path.relpath(sdk / "rust/sdk", project).replace(os.sep, "/") if sdk else None
        dependency = (f'{{ path = "{relative_sdk}", version = "={SDK_VERSION}" }}'
                      if sdk else f'"={SDK_VERSION}"')
        manifest.write_text(
            f'[package]\nname = "axiom-extension-project"\nversion = "0.0.0"\nedition = "2021"\n'
            f'publish = false\n\n[dependencies]\naxiom-extension-sdk = {dependency}\n'
        )
    document = tomllib.loads(manifest.read_text())
    if "axiom-extension-sdk" not in document.get("dependencies", {}):
        raise RuntimeError("Cargo.toml exists but does not declare axiom-extension-sdk; add its exact dependency first")
    target = f"axiom-{alias}"
    if all(item.get("name") != target for item in document.get("example", [])):
        with manifest.open("a") as output:
            output.write(f'\n[[example]]\nname = "{target}"\npath = ".axiom/ide/{alias}/main.rs"\n')
    generated = project / ".axiom/ide" / alias
    generated.mkdir(parents=True, exist_ok=True)
    (generated / "axiom_bindings.rs").write_text(
        run(cli, "extensions", "rust-inspect", alias, "--deps", "AxiomDeps.toml",
            "--view", "bindings", cwd=project)
    )
    relative = os.path.relpath(source, generated).replace(os.sep, "/")
    (generated / "main.rs").write_text(
        f'// Generated by setup_project.py; do not edit.\n'
        f'#![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals, unused_imports)]\n'
        f'pub mod axiom_bindings;\n#[path = "{relative}"]\nmod authored;\nfn main() {{}}\n'
    )
    if not (project / "Cargo.lock").is_file():
        run("cargo", "generate-lockfile", cwd=project)
    run("cargo", "fetch", "--locked", cwd=project)
    run("cargo", "metadata", "--locked", "--offline", "--format-version", "1", cwd=project)


def typescript(project: Path, alias: str, sdk: Path | None, cli: str) -> None:
    write_if_missing(project / "package.json", '{"name":"axiom-extension-project","private":true,"type":"module"}\n')
    package = str(sdk / "typescript") if sdk else f"{PACKAGE}@{SDK_VERSION}"
    expected_dependency = ("file:" + os.path.relpath(sdk / "typescript", project).replace(os.sep, "/")
                           if sdk else SDK_VERSION)
    document = json.loads((project / "package.json").read_text())
    locked = (project / "package-lock.json").is_file()
    dependencies = document.get("devDependencies", {})
    if (not locked or dependencies.get(PACKAGE) != expected_dependency
            or dependencies.get("typescript") != "7.0.2"):
        run("npm", "install", "--save-dev", "--save-exact", "--ignore-scripts", package,
            "typescript@7.0.2", cwd=project)
    if sdk is not None and not (sdk / "typescript/dist/index.d.ts").is_file():
        raise RuntimeError("build the local TypeScript SDK with `npm run build` before linking it")
    run("npm", "ci", "--ignore-scripts", cwd=project)
    installed = json.loads((project / "node_modules/@axiomcore/extension-sdk/package.json").read_text())
    if installed.get("version") != SDK_VERSION:
        raise RuntimeError("installed TypeScript SDK version does not match the compiler")
    bindings = project / "node_modules/@axiomcore/extension-bindings"
    bindings.mkdir(parents=True, exist_ok=True)
    (bindings / "index.d.ts").write_text(
        run(cli, "extensions", "typescript-inspect", alias, "--deps", "AxiomDeps.toml",
            "--view", "declarations", cwd=project)
    )
    (bindings / "package.json").write_text(
        '{"name":"@axiomcore/extension-bindings","version":"0.0.0-generated",'
        '"types":"./index.d.ts","exports":{".":{"types":"./index.d.ts"}}}\n'
    )
    write_if_missing(project / "tsconfig.json", json.dumps({
        "compilerOptions": {"strict": True, "noEmit": True, "module": "NodeNext",
                            "moduleResolution": "NodeNext", "target": "ES2023"},
        "include": ["**/*.ts"],
    }, indent=2) + "\n")


def python(project: Path, alias: str, sdk: Path | None, cli: str) -> None:
    manifest = project / "pyproject.toml"
    if not manifest.exists():
        relative_sdk = os.path.relpath(sdk / "python", project).replace(os.sep, "/") if sdk else None
        source = (f'\n[tool.uv.sources]\naxiom-extension-sdk = {{ path = "{relative_sdk}" }}\n'
                  if sdk else "")
        manifest.write_text(
            f'[project]\nname = "axiom-extension-project"\nversion = "0.0.0"\n'
            f'requires-python = ">=3.12"\ndependencies = ["axiom-extension-sdk=={SDK_VERSION}"]\n'
            + source
        )
    document = tomllib.loads(manifest.read_text())
    if not any(item.startswith("axiom-extension-sdk") for item in document.get("project", {}).get("dependencies", [])):
        raise RuntimeError("pyproject.toml exists but does not declare axiom-extension-sdk==0.1.0")
    command = ["uv", "sync", "--project", str(project)]
    if (project / "uv.lock").is_file():
        command.append("--locked")
    run(*command, cwd=project)
    (project / "axiom_bindings.pyi").write_text(
        run(cli, "extensions", "python-inspect", alias, "--deps", "AxiomDeps.toml",
            "--view", "stubs", cwd=project)
    )
    write_if_missing(project / "pyrightconfig.json", json.dumps({
        "venvPath": ".", "venv": ".venv", "extraPaths": ["."],
    }, indent=2) + "\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("project", type=Path)
    parser.add_argument("alias")
    parser.add_argument("--sdk-root", type=Path, help="local unpublished axiom-extension-sdk checkout")
    parser.add_argument("--cli", default="axiom")
    parser.add_argument("--check", action="store_true", help="fail if generated bindings or .acore boundary are stale")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z][A-Za-z0-9_-]*", args.alias):
        parser.error("alias must be a simple Axiom extension name")
    project = args.project.resolve(strict=True)
    sdk = args.sdk_root.resolve(strict=True) if args.sdk_root else None
    document = tomllib.loads((project / "AxiomDeps.toml").read_text())
    extension = document.get("extensions", {}).get(args.alias)
    if not isinstance(extension, dict):
        parser.error(f"AxiomDeps.toml does not declare extension {args.alias!r}")
    language = extension.get("language")
    source = project / extension["source"]
    if not source.is_file() or not source.resolve().is_relative_to(project):
        parser.error("declared extension source is missing or escapes the project")
    boundary = acore_boundary(project, extension)
    boundary_digest = hashlib.sha256(json.dumps(boundary, sort_keys=True).encode()).hexdigest()
    evidence = project / ".axiom/ide" / args.alias / "acore-boundary.sha256"
    if args.check:
        check_installed_sdk(project, language)
        check_bindings(project, args.alias, language, args.cli)
        if not evidence.is_file() or evidence.read_text().strip() != boundary_digest:
            raise RuntimeError(".acore state boundary changed; rerun setup_project.py")
        print(f"{args.alias}: {language} bindings and .acore boundary are current")
        return
    if language == "rust":
        rust(project, args.alias, source, sdk, args.cli)
    elif language == "typescript":
        typescript(project, args.alias, sdk, args.cli)
    elif language == "python":
        python(project, args.alias, sdk, args.cli)
    else:
        parser.error(f"unsupported extension language: {language}")
    evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence.write_text(boundary_digest + "\n")
    print(f"{args.alias}: {language} SDK installed and editor bindings synchronized")


if __name__ == "__main__":
    main()
