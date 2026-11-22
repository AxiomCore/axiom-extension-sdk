"""Async export surface (X8d, ABI 1.1 rounds). Run with CPython 3.12:
python3.12 -m unittest discover -s tests

* The CPython shim: `gather` is asyncio.gather, `HostOperationError` carries
  the ABI code (always runs).
* pyright strict type test of the stubs (`async_types.py`; runs when pyright
  is found, or AXIOM_PYRIGHT names it).
The compiled path is covered by axiom-build's async differential suite
(python/tests/differential/async) and the managed conformance tests.
"""
from __future__ import annotations

import asyncio
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
PYTHON_ROOT = HERE.parent
sys.path.insert(0, str(PYTHON_ROOT))

import axiom_extension_sdk as sdk  # noqa: E402
from test_tickets import _tool  # noqa: E402


class Shim(unittest.TestCase):
    def test_gather_and_host_operation_error_under_asyncio(self) -> None:
        async def value(n: int) -> int:
            await asyncio.sleep(0)
            return n

        async def failing() -> int:
            raise sdk.HostOperationError("Denied", "no grant")

        async def main() -> list[object]:
            values = await sdk.gather(value(1), value(2))
            collected = await sdk.gather(value(3), failing(), return_exceptions=True)
            return [values, collected[0], type(collected[1]).__name__, getattr(collected[1], "code"), str(collected[1])]

        self.assertEqual(asyncio.run(main()), [[1, 2], 3, "HostOperationError", "Denied", "no grant"])
        error = sdk.HostOperationError("Exhausted", "budget", True)
        self.assertEqual((error.code, error.retryable, str(error)), ("Exhausted", True, "budget"))

    def test_async_names_are_exported(self) -> None:
        for name in ["AsyncContext", "HostOperationError", "gather"]:
            self.assertTrue(hasattr(sdk, name), name)


class PyrightAsyncTypes(unittest.TestCase):
    def test_async_surface_type_checks_strictly(self) -> None:
        pyright = _tool("AXIOM_PYRIGHT", "pyright")
        if not pyright:
            self.skipTest("pyright not found (set AXIOM_PYRIGHT)")
        with tempfile.TemporaryDirectory() as work:
            root = pathlib.Path(work)
            shutil.copy(HERE / "async_types.py", root / "async_types.py")
            shutil.copytree(PYTHON_ROOT / "axiom_extension_sdk", root / "axiom_extension_sdk")
            (root / "pyrightconfig.json").write_text(json.dumps({
                "include": ["async_types.py"], "pythonVersion": "3.12", "typeCheckingMode": "strict",
                "reportUnnecessaryTypeIgnoreComment": "error",
            }))
            result = subprocess.run([pyright, "--outputjson"], cwd=root, capture_output=True, text=True, timeout=300)
            report = json.loads(result.stdout)
            problems = [d for d in report["generalDiagnostics"] if d["severity"] == "error"]
            self.assertEqual(problems, [], json.dumps(problems, indent=1))
            self.assertEqual(result.returncode, 0, result.stdout[-2000:])


if __name__ == "__main__":
    unittest.main()
