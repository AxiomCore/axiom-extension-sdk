"""Ticket API tests. Run with CPython 3.12: python3.12 -m unittest discover -s tests

* The editor shim imports and exposes the typed surface (always runs).
* pyright strict type test of the stubs (runs when `pyright` is found, or
  AXIOM_PYRIGHT names it).
* AOT path: compiles tests/ticket_guest.py with axiom-build's
  python/compile.py (profile axiom-python-aot-javy@0.2.0) and runs it with
  node against this repository's built TypeScript SDK, one fresh realm per
  exchange as the managed host does (runs when AXIOM_BUILD_DIR or a sibling
  ../axiom-build checkout, node, esbuild and typescript/dist exist).
"""
from __future__ import annotations

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
PYTHON_ROOT = HERE.parent
REPO = PYTHON_ROOT.parent
sys.path.insert(0, str(PYTHON_ROOT))

import axiom_extension_sdk as sdk  # noqa: E402


def _tool(env: str, name: str, *fallbacks: pathlib.Path) -> str | None:
    if os.environ.get(env):
        return os.environ[env]
    for candidate in fallbacks:
        if candidate.is_file():
            return str(candidate)
    return shutil.which(name)


class ShimSurface(unittest.TestCase):
    def test_typed_names_exist_and_are_generic(self) -> None:
        self.assertIs(sdk.Ticket[str].__origin__, sdk.Ticket)
        effect: sdk.TypedEffect[str] = {"namespace": "a", "operation": "b", "input": "Null"}
        self.assertEqual(effect, {"namespace": "a", "operation": "b", "input": "Null"})
        for protocol, members in ((sdk.Context, ("plan", "effects")), (sdk.ResumeContext, ("get", "plan", "outcome")), (sdk.Effects, ("push", "length"))):
            for member in members:
                self.assertTrue(hasattr(protocol, member), f"{protocol.__name__}.{member}")

    def test_typed_effect_returns_the_plain_effect(self) -> None:
        effect = sdk.typed_effect("catalog", "name", {"Unsigned": 1}, str)
        self.assertEqual(effect, {"namespace": "catalog", "operation": "name", "input": {"Unsigned": 1}})
        self.assertIn("def typed_effect(namespace: str, operation: str, input: Any, output: type[O]) -> TypedEffect[O]",
                      (PYTHON_ROOT / "axiom_extension_sdk" / "__init__.pyi").read_text())

    def test_positional_outcome_is_kept_and_documented_deprecated(self) -> None:
        doc = sdk.ResumeContext.outcome.__doc__ or ""
        self.assertIn("Deprecated", doc)
        self.assertIn("ctx.get(ticket)", doc)

    def test_stub_and_shim_declare_the_same_ticket_names(self) -> None:
        stub = (PYTHON_ROOT / "axiom_extension_sdk" / "__init__.pyi").read_text()
        for name in ("class Ticket(Generic[O])", "class TypedEffect(TypedDict, Generic[O])", "class Effects(Protocol)", "def get(self, ticket: Ticket[O]) -> O", "def plan(self) -> Effects"):
            self.assertIn(name, stub)


class PyrightTypes(unittest.TestCase):
    def test_get_infers_ticket_output_type(self) -> None:
        pyright = _tool("AXIOM_PYRIGHT", "pyright")
        if not pyright:
            self.skipTest("pyright not found (set AXIOM_PYRIGHT)")
        with tempfile.TemporaryDirectory() as work:
            root = pathlib.Path(work)
            shutil.copy(HERE / "ticket_types.py", root / "ticket_types.py")
            shutil.copytree(PYTHON_ROOT / "axiom_extension_sdk", root / "axiom_extension_sdk")
            # Pyright reads the stub (.pyi) in preference to the shim.
            (root / "pyrightconfig.json").write_text(json.dumps({
                "include": ["ticket_types.py"], "pythonVersion": "3.12", "typeCheckingMode": "strict",
                "reportUnnecessaryTypeIgnoreComment": "error",
            }))
            result = subprocess.run([pyright, "--outputjson"], cwd=root, capture_output=True, text=True, timeout=300)
            report = json.loads(result.stdout)
            problems = [d for d in report["generalDiagnostics"] if d["severity"] == "error"]
            self.assertEqual(problems, [], json.dumps(problems, indent=1))
            self.assertEqual(result.returncode, 0, result.stdout[-2000:])


class AotPath(unittest.TestCase):
    def test_tickets_through_the_aot_compiler_and_typescript_runtime(self) -> None:
        build = pathlib.Path(os.environ.get("AXIOM_BUILD_DIR", REPO.parent / "axiom-build"))
        compiler = build / "python" / "compile.py"
        node = _tool("AXIOM_NODE", "node")
        esbuild = _tool("AXIOM_ESBUILD", "esbuild", REPO / "typescript" / "node_modules" / ".bin" / "esbuild")
        dist = REPO / "typescript" / "dist" / "index.js"
        missing = [str(p) for p, ok in ((compiler, compiler.is_file()), ("node", node), ("esbuild", esbuild), (dist, dist.is_file())) if not ok]
        if missing:
            self.skipTest(f"AOT path prerequisites missing: {missing}")
        with tempfile.TemporaryDirectory() as work:
            root = pathlib.Path(work)
            request = {"format": "axiom-python-aot-request/v1", "entryModule": "main", "exports": ["raw", "run"],
                       "modules": {"main.py": (HERE / "ticket_guest.py").read_text()}}
            (root / "request.json").write_text(json.dumps(request))
            compiled = subprocess.run([sys.executable, "-I", "-S", str(compiler), str(root / "request.json"), str(root / "generated.ts"), str(root / "report.json")],
                                      capture_output=True, text=True, timeout=300)
            self.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
            ok = lambda index, value: {"index": index, "result": {"Ok": value}}  # noqa: E731
            (root / "cases.json").write_text(json.dumps({
                "invoke": [{"export": "run", "input": {"Unsigned": 7}}, {"export": "raw", "input": {"Unsigned": 7}}],
                "outcomes": [ok(0, {"String": "Linen"}), ok(1, {"Bool": True})],
            }))
            run = subprocess.run([node, str(HERE / "aot_harness.mjs"), str(root / "generated.ts"), str(dist.parent), esbuild, str(root / "cases.json")],
                                 capture_output=True, text=True, timeout=300)
            self.assertEqual(run.returncode, 0, run.stderr[-3000:])
            result = json.loads(run.stdout)
        fresh, persistent = result["fresh"], result["persistent"]
        # The ticket-built plan is byte-identical to the raw ctx.effects(...) plan.
        self.assertIn("ok", fresh["run"], fresh["run"])
        self.assertEqual(json.dumps(fresh["run"]), json.dumps(fresh["raw"]))
        self.assertEqual(json.dumps(persistent["run"]), json.dumps(fresh["raw"]))
        expected = {"index": {"Unsigned": 1}, "legacy": {"String": "Linen"},
                    "mixed": {"String": "effect tickets from different effect plans were read in one resume"},
                    "name": {"String": "Linen"}, "stock": {"Bool": True}}
        echo = result["echo"]
        self.assertIn("err", echo["swapped"], echo["swapped"])
        self.assertIn("rebuilt effect plan differs from the yielded plan: call 0 is catalog.name, the yielded call 0 was catalog.stock", echo["swapped"]["err"])
        for mode in (fresh, persistent, echo):
            self.assertIn("ok", mode["resume"], mode["resume"])
            output = {field["name"]: field["value"] for field in mode["resume"]["ok"]["result"]["output"]["Record"]}
            self.assertEqual(output, expected)


if __name__ == "__main__":
    unittest.main()
