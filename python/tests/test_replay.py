"""X9d: the Python tape loader (axiom_extension_sdk/replay.py). Run with
CPython 3.12: python3.12 -m unittest discover -s tests

The fixtures were recorded by the broker from the wasm builds of the
runtime's async Python and TypeScript conformance guests; verifying them here
checks that the Python digest agrees with the Rust recorder. Replaying a
compiled module (``replay_compiled``) needs node, esbuild and a compiled
``generated.ts``; the runtime's vac-conformance test
``sdk_loaders_replay_recorded_tapes`` runs it.
"""
from __future__ import annotations

import json
import pathlib
import sys
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

from axiom_extension_sdk import replay  # noqa: E402


def tape(name: str) -> bytes:
    return (HERE / "fixtures" / "tapes" / f"{name}.axtape.json").read_bytes()


class TapeLoader(unittest.TestCase):
    def test_recorded_tapes_verify(self) -> None:
        for name in ["py_rounds-run", "ts_rounds-run"]:
            loaded = replay.load_tape(tape(name))
            self.assertEqual(loaded["invocation"]["export"], "run")
            self.assertEqual(len(loaded["rounds"]), 3)
            self.assertEqual(loaded["terminal"]["status"], "completed")

    def test_tampered_foreign_and_oversized_tapes_are_refused(self) -> None:
        text = tape("py_rounds-run").decode()
        with self.assertRaisesRegex(replay.TapeError, "integrity check"):
            replay.load_tape(text.replace('"Unsigned": 20', '"Unsigned": 21', 1))
        with self.assertRaisesRegex(replay.TapeError, "not an axiom-extension-replay-tape"):
            replay.load_tape("{}")
        document = json.loads(text)
        del document["integrity"]
        with self.assertRaisesRegex(replay.TapeError, "no integrity digest"):
            replay.load_tape(json.dumps(document))
        with self.assertRaisesRegex(replay.TapeError, "over the"):
            replay.load_tape(b" " * (replay.MAX_TAPE_FILE_BYTES + 1))

    def test_canonical_json_matches_the_recorder(self) -> None:
        self.assertEqual(replay.canonical_json({"b": [1, {"d": "x", "c": None}], "a": "é\n"}), '{"a":"é\\n","b":[1,{"c":null,"d":"x"}]}')


if __name__ == "__main__":
    unittest.main()
