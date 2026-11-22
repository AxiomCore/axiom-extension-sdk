// X9d: the replay loader (`@axiomcore/extension-sdk/replay`) against tapes
// the broker recorded from the wasm build of the runtime's TypeScript
// conformance guest (axiom-runtime extensions/typescript/tests/managed-async/
// ts_rounds.ts, exports `run` and `sync_yield`; recorded by
// vac-conformance/tests/replay_tape.rs). The guest below is the same source.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { defineExtension, extension, operation } from "../dist/index.js";
import { parseTape, replayTape, ReplayDifference, canonicalJson, sha256Hex, parseExactJson, verifyTape, ExactInteger } from "../dist/replay.js";

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

// w3-bc2 (w3-vbc finding 10): exact integers, already-parsed objects verified
// like text, duplicate keys, trailing content, depth, identity.
const reseal = (doc) => {
  const { integrity, ...content } = doc;
  return { ...content, integrity: { algorithm: "sha256", digest: sha256Hex(new TextEncoder().encode(canonicalJson(content))) } };
};
// `this[key]` is the value before its toJSON ran.
const exactText = (doc) => JSON.stringify(doc, function (key, value) { const raw = this[key]; return raw instanceof ExactInteger ? `@@${raw.text}@@` : value; }, 2).replace(/"@@(-?[0-9]+)@@"/g, "$1");

test("integers above 2^53 are read exactly and handed to the guest as exact strings", () => {
  const doc = parseExactJson(tape("ts_rounds-sync_yield"));
  doc.invocation.input = { Unsigned: new ExactInteger("9007199254740993") };
  const text = exactText(reseal(doc));
  assert.match(text, /"Unsigned": 9007199254740993\b/);
  const parsed = parseTape(text);
  assert.ok(parsed.invocation.input.Unsigned instanceof ExactInteger);
  assert.equal(parsed.invocation.input.Unsigned.text, "9007199254740993");
  // A JSON.parse result has rounded it: refused rather than mis-verified.
  assert.throws(() => verifyTape(JSON.parse(text)), /outside the exact range/);
  assert.equal(parseExactJson("18446744073709551615").text, "18446744073709551615");
  assert.equal(parseExactJson("-9223372036854775808").text, "-9223372036854775808");
  assert.equal(parseExactJson("9007199254740991"), 9007199254740991);
});

test("an already-parsed tape is verified exactly like text", async () => {
  const text = tape("ts_rounds-sync_yield");
  assert.equal((await replayTape(same, JSON.parse(text))).yields, 1);
  const tampered = JSON.parse(text);
  tampered.invocation.input = { Unsigned: 999 };
  await assert.rejects(replayTape(same, tampered), /integrity check/);
  const future = JSON.parse(text);
  future.version = 2;
  await assert.rejects(replayTape(same, future), /unsupported tape version/);
  const redacted = reseal(JSON.parse(text));
  redacted.header.replayable = false;
  await assert.rejects(replayTape(same, reseal(redacted)), /redacted/);
});

test("duplicate keys, trailing content and deep nesting are refused", () => {
  const text = tape("ts_rounds-sync_yield");
  assert.throws(() => parseTape(text.replace('"abi": ', '"abi": "1.1",\n  "abi": ')), /duplicate key `abi`/);
  assert.throws(() => parseTape(`${text} {}`), /trailing content/);
  assert.throws(() => parseExactJson(`${"[".repeat(200)}${"]".repeat(200)}`), /nesting deeper than 128/);
  assert.equal(parseTape(`${text}\n\n`).format, "axiom-extension-replay-tape");
  // `__proto__` is data, never the prototype.
  const odd = parseExactJson('{"__proto__": {"polluted": true}}');
  assert.equal(Object.getPrototypeOf(odd), Object.prototype);
  assert.equal({}.polluted, undefined);
});

test("a replay can require the recording's identity", async () => {
  const text = tape("ts_rounds-sync_yield");
  const doc = parseTape(text);
  await assert.rejects(replayTape(same, text, { expect: { extension: "someone.else" } }), /AXIOM_EXTENSION_REPLAY_IDENTITY/);
  assert.equal((await replayTape(same, text, { expect: { extension: doc.identity.extension, application: doc.identity.application } })).yields, 1);
  if (doc.identity.moduleSha256) {
    await assert.rejects(replayTape(same, text, { expect: { moduleSha256: "f".repeat(64) } }), /different build/);
    assert.equal((await replayTape(same, text, { expect: { moduleSha256: "f".repeat(64), allowIdentityMismatch: true } })).yields, 1);
  }
});
