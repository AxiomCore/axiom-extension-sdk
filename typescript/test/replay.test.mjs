// X9d: the replay loader (`@axiomcore/extension-sdk/replay`) against tapes
// the broker recorded from the wasm build of the runtime's TypeScript
// conformance guest (axiom-runtime extensions/typescript/tests/managed-async/
// ts_rounds.ts, exports `run` and `sync_yield`; recorded by
// vac-conformance/tests/replay_tape.rs). The guest below is the same source.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { defineExtension, extension, operation } from "../dist/index.js";
import { parseTape, replayTape, ReplayDifference, canonicalJson, sha256Hex } from "../dist/replay.js";

const catalog = (name) => operation("catalog", name);
const LOAD = catalog("load");
const PRICE = catalog("price");
const RESERVE = catalog("reserve");
const prev = (value) => ({ prev: value });
const commit = (cx, output, total) => cx.complete(output, { patches: [cx.patch("ui:checkout").set("count", total).build()] });
const tape = (name) => readFileSync(new URL(`./fixtures/tapes/${name}.axtape.json`, import.meta.url), "utf8");

const guest = (priceInput) => defineExtension({
  exports: {
    run: extension.export(async (cx) => {
      const load = await cx.call(LOAD.call(prev(null)));
      const price = await cx.call(PRICE.call(priceInput(load)));
      const reserve = await cx.call(RESERVE.call(prev(price)));
      return commit(cx, { outcomes: [load, price, reserve], round: cx.round() }, load + price + reserve);
    }),
    sync_yield: (ctx) => { const plan = ctx.plan(); plan.push(LOAD.call(7)); return ctx.effects(plan); },
  },
  resume: (ctx) => {
    const plan = ctx.plan();
    const ticket = plan.push(LOAD.call(7));
    return ctx.complete(ctx.get(ticket) + 1);
  },
});
const same = guest((load) => prev(load));

test("sha256 and canonical JSON agree with the Rust recorder", () => {
  assert.equal(sha256Hex(new TextEncoder().encode("abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  assert.equal(canonicalJson({ b: [1, { d: "x", c: null }], a: "é\n" }), '{"a":"é\\n","b":[1,{"c":null,"d":"x"}]}');
  for (const name of ["ts_rounds-run", "ts_rounds-sync_yield", "ts_rounds_sync_build-sync_yield", "ts_rounds-run-cancelled"]) parseTape(tape(name));
});

test("recorded tapes replay identically (1.1 async, 1.1 sync, 1.0 sync, cancelled)", async () => {
  const run = await replayTape(same, tape("ts_rounds-run"));
  assert.equal(run.yields, 3);
  assert.equal(run.result.output.Record[1].value.Unsigned, 4);
  assert.deepEqual(await replayTape(same, tape("ts_rounds-sync_yield")), { yields: 1, status: "completed", result: { output: { Unsigned: 11 }, patches: [], transactions: [], emitted_events: [] } });
  assert.equal((await replayTape(same, tape("ts_rounds_sync_build-sync_yield"))).yields, 1);
  assert.deepEqual(await replayTape(same, tape("ts_rounds-run-cancelled")), { yields: 2, status: "cancelled" });
});

test("a changed guest is named at its first differing call", async () => {
  const changed = guest((load) => load);
  await assert.rejects(replayTape(changed, tape("ts_rounds-run")), (error) => {
    assert.ok(error instanceof ReplayDifference);
    assert.equal(error.at, "round 2, call 0");
    assert.match(error.message, /recorded catalog\.price\(\{"Record":\[\{"name":"prev","value":\{"Unsigned":10\}\}\]\}\) but the guest yielded catalog\.price\(\{"Unsigned":10\}\)/);
    return true;
  });
});

test("tampered, foreign and redacted tapes are refused", () => {
  const text = tape("ts_rounds-run");
  assert.throws(() => parseTape(text.replace('"Unsigned": 20', '"Unsigned": 21')), /integrity check/);
  assert.throws(() => parseTape("{}"), /not an axiom-extension-replay-tape/);
  const doc = JSON.parse(text);
  doc.header.replayable = false;
  assert.throws(() => parseTape(JSON.stringify(doc)), /integrity check|redacted/);
});
