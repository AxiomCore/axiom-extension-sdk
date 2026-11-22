// w4-h H5: an ABI 1.0 resume that reads a failed host operation keeps the
// host's error kind (Exhausted, Denied, ...) end to end, as the Rust SDK's
// `?` and the 1.1 async runtime do. Before w4-h the 1.0 runners reported
// every such failure as `Guest: host operation denied`.
import assert from "node:assert/strict";
import test from "node:test";
import { createResumeContext, defineExtension, HostOperationError, operation } from "../dist/index.js";
import { runExtension } from "../dist/runner.js";

const getName = operation("catalog", "name");

function bridge(extension, request) {
  const input = new TextEncoder().encode(JSON.stringify(request));
  let offset = 0;
  const written = [];
  globalThis.Javy = { IO: {
    readSync(_fd, buffer) { const n = Math.min(buffer.length, input.length - offset); buffer.set(input.subarray(offset, offset + n)); offset += n; return n; },
    writeSync(_fd, bytes) { written.push(Buffer.from(bytes)); return bytes.length; },
  } };
  try { runExtension(extension); } finally { delete globalThis.Javy; }
  return JSON.parse(Buffer.concat(written).toString());
}

const failedOutcome = (code, message = "max_host_calls exhausted") => ({ index: 0, result: { Err: { code, message, retryable: false } } });

test("a failed host outcome raises HostOperationError with the host's code", () => {
  for (const code of ["Exhausted", "Denied", "Cancelled", "Host"]) {
    const resume = createResumeContext([failedOutcome(code)], [], 1);
    const ticket = resume.plan().call(getName.call(0));
    assert.throws(() => resume.get(ticket), (error) => error instanceof HostOperationError && error.code === code && /host operation denied/.test(error.message));
  }
  // A malformed outcome stays a plain refusal.
  const malformed = createResumeContext([{ index: 0, result: { Err: "nope" } }], [], 1);
  const ticket = malformed.plan().call(getName.call(0));
  assert.throws(() => malformed.get(ticket), (error) => !(error instanceof HostOperationError) && /host operation denied/.test(error.message));
});

test("the 1.0 bridge runner reports the host's code, not Guest", () => {
  const extension = defineExtension({
    exports: { run: (ctx) => { const fx = ctx.plan(); fx.call(getName.call(0)); return ctx.effects(fx); } },
    resume: (resume) => { const ticket = resume.plan().call(getName.call(0)); return resume.complete(resume.get(ticket)); },
  });
  const response = bridge(extension, { kind: "resume", request_id: 1, outcomes: [failedOutcome("Exhausted")], events: [] });
  assert.equal(response.kind, "failed");
  assert.equal(response.error.code, "Exhausted");
  assert.match(response.error.message, /max_host_calls exhausted/);
  // Any other guest exception is still `Guest`.
  const thrower = defineExtension({ exports: { run: () => { throw new Error("boom"); } } });
  const other = bridge(thrower, { kind: "invoke", request_id: 1, invocation: { export: "run", input: "Null", snapshots: [], deadline_unix_ms: 0 } });
  assert.deepEqual(other.error, { code: "Guest", message: "boom", retryable: false });
});
