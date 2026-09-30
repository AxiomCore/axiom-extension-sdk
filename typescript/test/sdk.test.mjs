import assert from "node:assert/strict";
import test from "node:test";
import { defineExtension, createContext, encodeValue, decodeValue } from "../dist/index.js";

test("package exposes the authored extension API", () => {
  const extension = defineExtension({ exports: { echo: (context) => context.complete(context.input) } });
  assert.equal(typeof extension.exports.echo, "function");
  assert.deepEqual(decodeValue(encodeValue({ amount: 12 })), { amount: 12 });
});

test("runner subpath is loadable without executing a guest", async () => {
  const runner = await import("../dist/runner.js");
  assert.equal(typeof runner.runExtension, "function");
});


test("patches preserve the authorized snapshot resource and revision", () => {
  const context = createContext({
    deadline_unix_ms: 0, export: "calculate", input: "Null",
    snapshots: [{ resource: "ui:cart", revision: 7, value: encodeValue({ total: 100 }) }],
  });
  assert.deepEqual(context.patch("ui:cart").set("total", 90).build(), {
    resource: "ui:cart", expected_revision: 7,
    operations: [{ Set: { path: ["total"], value: { Unsigned: 90 } } }],
  });
  assert.throws(() => context.patch("cart"), /absent snapshot/);
  assert.throws(() => context.patch("ui:other"), /absent snapshot/);
});
