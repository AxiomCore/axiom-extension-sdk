import assert from "node:assert/strict";
import test from "node:test";
import { defineExtension } from "../dist/index.js";
import { runExtension } from "../dist/runner.js";

// Drive the bridge runner with a mocked Javy stdin/stdout, across chunk and
// 64 KiB boundaries, and check the request arrives byte-exact.
function run(requestText) {
  const input = new TextEncoder().encode(requestText);
  let offset = 0;
  const written = [];
  let reads = 0;
  globalThis.Javy = {
    IO: {
      readSync(fd, buffer) {
        assert.equal(fd, 0);
        reads += 1;
        const count = Math.min(buffer.length, input.length - offset);
        buffer.set(input.subarray(offset, offset + count));
        offset += count;
        return count;
      },
      writeSync(fd, bytes) {
        assert.equal(fd, 1);
        written.push(Buffer.from(bytes));
        return bytes.length;
      },
    },
  };
  runExtension(defineExtension({ exports: { echo: (context) => context.complete(context.input) } }));
  delete globalThis.Javy;
  return { response: JSON.parse(Buffer.concat(written).toString()), reads };
}

function requestOfLength(total) {
  const shell = (filler) => JSON.stringify({ kind: "invoke", request_id: 1, invocation: { export: "echo", input: { String: filler }, snapshots: [], deadline_unix_ms: 0 } });
  const filler = "x".repeat(Math.max(0, total - shell("").length));
  const text = shell(filler);
  assert.equal(new TextEncoder().encode(text).length, total);
  return { text, filler };
}

for (const total of [200, 65535, 65536, 65537, 131072, 131073, 300000, 1048576]) {
  test(`request of ${total} bytes reaches the handler byte-exact`, () => {
    const { text, filler } = requestOfLength(total);
    const { response, reads } = run(text);
    assert.equal(response.kind, "completed");
    assert.deepEqual(response.result.output, { String: filler });
    assert.ok(reads >= 2, "input is read until the zero-length end-of-input read");
  });
}
