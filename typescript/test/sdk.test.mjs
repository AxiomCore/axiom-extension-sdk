import assert from "node:assert/strict";
import test from "node:test";
import { defineExtension, encodeValue, decodeValue } from "../dist/index.js";

test("package exposes the authored extension API", () => {
  const extension = defineExtension({ exports: { echo: (context) => context.complete(context.input) } });
  assert.equal(typeof extension.exports.echo, "function");
  assert.deepEqual(decodeValue(encodeValue({ amount: 12 })), { amount: 12 });
});

test("runner subpath is loadable without executing a guest", async () => {
  const runner = await import("../dist/runner.js");
  assert.equal(typeof runner.runExtension, "function");
});
