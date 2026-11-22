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


MAX_TAPE_DEPTH = 128
"""Nesting deeper than this is refused (the Rust reader's limit)."""


def _depth(raw: bytes) -> int:
    """Deepest object/array nesting of ``raw`` (outside strings)."""
    depth = deepest = 0
    in_string = escaped = False
    for byte in raw:
        if in_string:
            if escaped:
                escaped = False
            elif byte == 0x5C:  # backslash
                escaped = True
            elif byte == 0x22:  # quote
                in_string = False
        elif byte == 0x22:
            in_string = True
        elif byte in (0x7B, 0x5B):  # { [
            depth += 1
            deepest = max(deepest, depth)
        elif byte in (0x7D, 0x5D):  # } ]
            depth -= 1
    return deepest


def _no_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for key, value in pairs:
        if key in out:
            raise TapeError(f"AXIOM_EXTENSION_REPLAY_TAPE: malformed tape: duplicate key `{key}`")
        out[key] = value
    return out


def load_tape(data: bytes | str, expect_extension: str | None = None) -> dict[str, Any]:
    """Read and verify a tape; raises ``TapeError``.

    Refused: over the size cap, not JSON, nesting deeper than
    ``MAX_TAPE_DEPTH``, a duplicate object key (the digest covers the parsed
    content, so bytes that could parse to two meanings are not accepted),
    another format or version, a failed integrity digest, a redacted tape
    (``replayable: false``; only earlier dev recorders wrote these, the
    current recorder refuses instead of redacting, x9d §3), and (with
    ``expect_extension``) a tape of another extension.
    """
    raw = data.encode("utf-8") if isinstance(data, str) else data
    if len(raw) > MAX_TAPE_FILE_BYTES:
        raise TapeError(f"AXIOM_EXTENSION_REPLAY_TAPE: tape is {len(raw)} bytes, over the {MAX_TAPE_FILE_BYTES}-byte limit")
    if _depth(raw) > MAX_TAPE_DEPTH:
        raise TapeError(f"AXIOM_EXTENSION_REPLAY_TAPE: nesting deeper than {MAX_TAPE_DEPTH}")
    try:
        tape = json.loads(raw, object_pairs_hook=_no_duplicate_keys)
    except RecursionError:
        raise TapeError(f"AXIOM_EXTENSION_REPLAY_TAPE: nesting deeper than {MAX_TAPE_DEPTH}") from None
    except TapeError:
        raise
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
    recorded = tape.get("identity", {}).get("extension")
    if expect_extension is not None and recorded != expect_extension:
        raise TapeError(
            f"AXIOM_EXTENSION_REPLAY_IDENTITY: the tape was recorded for extension `{recorded}`, not `{expect_extension}`"
        )
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
