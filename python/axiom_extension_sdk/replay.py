"""X9d: dev replay tapes for Python extensions (contract x9d-replay-tape.md).

A replay tape (``axiom-extension-replay-tape`` version 1) records one
invocation: input, round-1 snapshots, every yielded plan with the host's
outcomes, and the final result. ``load_tape`` reads and verifies one (size,
format, version, integrity digest, replayable). ``replay_compiled`` replays
tapes against an AOT-compiled Python extension the way Python extensions are
tested: the compiler's ``generated.ts`` runs on the TypeScript SDK in node,
driven by the SDK's own replay loader (``@axiomcore/extension-sdk/replay``),
because Python extension code only runs compiled.

Test helper only; the host never imports it. Standalone (no package-relative
imports) so it also runs as a script::

    python3.12 -I replay.py --generated generated.ts --sdk <typescript sdk dir> \\
        --esbuild <esbuild> [--node node] tape.axtape.json ...

exits 0 when every tape replays identically, 1 otherwise, and prints one JSON
object per tape.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
from typing import Any

MAX_TAPE_FILE_BYTES = 32 * 1024 * 1024
FORMAT = "axiom-extension-replay-tape"


class TapeError(ValueError):
    """A tape that cannot be read or verified."""


def canonical_json(value: Any) -> str:
    """Keys sorted, no whitespace: the input of the tape digest."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def load_tape(data: bytes | str) -> dict[str, Any]:
    """Read and verify a tape; raises ``TapeError``."""
    raw = data.encode("utf-8") if isinstance(data, str) else data
    if len(raw) > MAX_TAPE_FILE_BYTES:
        raise TapeError(f"AXIOM_EXTENSION_REPLAY_TAPE: tape is {len(raw)} bytes, over the {MAX_TAPE_FILE_BYTES}-byte limit")
    try:
        tape = json.loads(raw)
    except ValueError as error:
        raise TapeError(f"AXIOM_EXTENSION_REPLAY_TAPE: not JSON: {error}") from None
    if not isinstance(tape, dict) or tape.get("format") != FORMAT:
        raise TapeError(f"AXIOM_EXTENSION_REPLAY_TAPE: not an {FORMAT} document")
    if tape.get("version") != 1:
        raise TapeError("AXIOM_EXTENSION_REPLAY_TAPE: unsupported tape version (this reader knows version 1)")
    content = dict(tape)
    integrity = content.pop("integrity", None)
    if not isinstance(integrity, dict):
        raise TapeError("AXIOM_EXTENSION_REPLAY_TAPE: tape has no integrity digest")
    digest = hashlib.sha256(canonical_json(content).encode("utf-8")).hexdigest()
    if integrity.get("algorithm") != "sha256" or integrity.get("digest") != digest:
        raise TapeError("AXIOM_EXTENSION_REPLAY_TAPE: tape failed its integrity check (content changed after recording)")
    if tape.get("header", {}).get("replayable") is not True:
        raise TapeError("AXIOM_EXTENSION_REPLAY_TAPE: tape has redacted values and cannot be replayed byte-exactly")
    return tape


_HARNESS = """
import extension from %(guest)s;
import { replayTape } from %(replay)s;
import { readFileSync } from "node:fs";
const out = [];
for (const file of JSON.parse(process.env.AXIOM_REPLAY_TAPES)) {
  try { out.push({ file, ok: await replayTape(extension, readFileSync(file, "utf8")) }); }
  catch (error) { out.push({ file, error: String(error && error.message || error) }); }
}
console.log(JSON.stringify(out));
"""


def replay_compiled(
    tapes: list[pathlib.Path],
    generated_ts: pathlib.Path,
    sdk_typescript: pathlib.Path,
    esbuild: str,
    node: str = "node",
    timeout: int = 600,
) -> list[dict[str, Any]]:
    """Replay ``tapes`` against the compiled module ``generated_ts``.

    ``sdk_typescript`` is the TypeScript SDK package directory (with
    ``src/index.ts`` and ``src/replay.ts``). Every tape is verified here first;
    each result is ``{"file", "ok": report}`` or ``{"file", "error": text}``.
    """
    results: list[dict[str, Any]] = []
    good: list[str] = []
    for tape in tapes:
        try:
            load_tape(pathlib.Path(tape).read_bytes())
            good.append(str(pathlib.Path(tape).resolve()))
        except TapeError as error:
            results.append({"file": str(tape), "error": str(error)})
    if not good:
        return results
    sdk = pathlib.Path(sdk_typescript).resolve()
    generated = pathlib.Path(generated_ts).resolve()
    with tempfile.TemporaryDirectory() as work:
        harness = pathlib.Path(work) / "harness.mjs"
        harness.write_text(_HARNESS % {"guest": json.dumps(str(generated)), "replay": json.dumps(str(sdk / "src" / "replay.ts"))})
        bundle = pathlib.Path(work) / "bundle.mjs"
        subprocess.run(
            [esbuild, str(harness), "--bundle", "--platform=node", "--format=esm", "--log-level=warning",
             f"--alias:@axiomcore/extension-sdk={sdk / 'src' / 'index.ts'}", f"--outfile={bundle}"],
            cwd=str(generated.parent), check=True, timeout=timeout,
        )
        env = dict(os.environ, AXIOM_REPLAY_TAPES=json.dumps(good))
        run = subprocess.run([node, str(bundle)], capture_output=True, text=True, env=env, timeout=timeout, check=False)
        if run.returncode != 0:
            raise RuntimeError(f"node replay failed ({run.returncode}): {run.stderr[-2000:]}")
        results.extend(json.loads(run.stdout.strip().splitlines()[-1]))
    return results


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--generated", required=True, type=pathlib.Path)
    parser.add_argument("--sdk", required=True, type=pathlib.Path)
    parser.add_argument("--esbuild", required=True)
    parser.add_argument("--node", default="node")
    parser.add_argument("tapes", nargs="+", type=pathlib.Path)
    arguments = parser.parse_args(argv)
    results = replay_compiled(arguments.tapes, arguments.generated, arguments.sdk, arguments.esbuild, arguments.node)
    for result in results:
        print(json.dumps(result, sort_keys=True))
    return 0 if all("ok" in result for result in results) else 1


if __name__ == "__main__":
    sys.exit(main())
