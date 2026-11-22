// ABI 1.1 async exports (X8d): the runner's round protocol, driven the way
// the managed kernel drives Javy: the first call starts the export, then the
// step hook runs each time the job queue has drained (here: after a macrotask,
// which node runs only once every microtask has run).
import assert from "node:assert/strict";
import test from "node:test";
import { asyncExport, defineExtension, extension, operation, HostOperationError } from "../dist/index.js";
import { runRound } from "../dist/runner.js";
import { canonicalRoundPlan, LOWERED_ASYNC } from "../dist/async.js";
import { readFileSync } from "node:fs";

const load = operation("catalog", "load");
const price = operation("catalog", "price");
const drained = () => new Promise((resolve) => setImmediate(resolve));
const invocation = (name, input = "Null") => ({ export: name, input, snapshots: [{ resource: "ui:checkout", revision: 3, value: { Record: [{ name: "count", value: { Unsigned: 1 } }] } }], deadline_unix_ms: 0 });
const ok = (index, value) => ({ index, result: { Ok: value } });
const err = (index, code, message = "no") => ({ index, result: { Err: { code, message, retryable: false } } });

/** One round: returns the response and the number of steps it took. */
async function round(ext, name, tape = [], { input = "Null", abi = "1.1", maxRounds = 16 } = {}) {
  const written = [];
  delete globalThis.__axiom_managed_step__;
  runRound(ext, { kind: "invokeRound", request_id: 1, invocation: invocation(name, input), round: tape.length + 1, max_rounds: maxRounds, abi, tape }, (response) => written.push(response));
  let steps = 0;
  while (written.length === 0) {
    await drained();
    assert.ok(steps < tape.length + 2, "the runner answers within tape.length + 2 steps");
    globalThis.__axiom_managed_step__();
    steps += 1;
  }
  assert.equal(written.length, 1);
  return { response: written[0], steps };
}

/** Runs to completion, answering each plan with `host(effect)`; returns the final response and the plans. */
async function drive(ext, name, host, options = {}) {
  const tape = [];
  const plans = [];
  for (;;) {
    const { response } = await round(ext, name, tape, options);
    if (response.kind !== "yieldedRound") return { response, plans };
    if (response.replayed) assert.deepEqual(response.replayed, plans, "re-derived plans equal the earlier yields");
    plans.push(response.plan);
    const effects = response.plan.Plan ? response.plan.Plan.effects : response.plan.Group.branches.map((branch) => branch.effect);
    tape.push({ outcomes: effects.map((effect, index) => host(effect, index)), events: [] });
  }
}

const values = { load: 10, price: 20 };
const host = (effect, index) => (effect.operation === "fail" ? err(index, "Guest", "unavailable") : ok(index, { Unsigned: values[effect.operation] ?? 0 }));

const guest = defineExtension({
  exports: {
    scenario: extension.export(async (cx) => {
      const a = await cx.call(load.call({ prev: null }));
      const b = await cx.call(price.call({ prev: a }));
      return cx.complete({ total: a + b, round: cx.round() }, { patches: [cx.patch("ui:checkout").set("count", a + b).build()] });
    }),
    all: extension.export(async (cx, n) => (await Promise.all(Array.from({ length: n }, (_, i) => cx.call(load.call(i))))).length),
    plain: extension.export(async (cx) => (await cx.call(load.call(1))) + 1),
    early: extension.export(async (cx) => { void cx.call(load.call(1)); return 7; }),
    forever: extension.export(async () => { await new Promise(() => {}); }),
    failing: extension.export(async (cx) => cx.call(operation("catalog", "fail").call(1))),
    throwing: extension.export(async () => { throw new TypeError("bad"); }),
    caught: extension.export(async (cx) => {
      try { await cx.call(operation("catalog", "fail").call(1)); } catch (error) { return [error instanceof HostOperationError, error.code]; }
      return "none";
    }),
    divergent: extension.export(async (cx) => { await cx.call(load.call(cx.round())); return 0; }),
    explicit: asyncExport((cx) => cx.call(load.call(5))),
    sync: (ctx) => ctx.complete(ctx.input),
  },
});

test("a two-await scenario yields one plan per round and completes with patches", async () => {
  const { response, plans } = await drive(guest, "scenario", host);
  assert.deepEqual(plans, [
    { Plan: { effects: [{ namespace: "catalog", operation: "load", input: { Record: [{ name: "prev", value: "Null" }] } }] } },
    { Plan: { effects: [{ namespace: "catalog", operation: "price", input: { Record: [{ name: "prev", value: { Unsigned: 10 } }] } }] } },
  ]);
  assert.equal(response.kind, "completedRound");
  assert.equal(response.replayed.length, 2);
  assert.deepEqual(response.result.output, { Record: [{ name: "round", value: { Unsigned: 3 } }, { name: "total", value: { Unsigned: 30 } }] });
  assert.deepEqual(response.result.patches, [{ resource: "ui:checkout", expected_revision: 3, operations: [{ Set: { path: ["count"], value: { Unsigned: 30 } } }] }]);
});

test("replaying two rounds takes tape.length + 2 engine steps", async () => {
  const tape = [{ outcomes: [ok(0, { Unsigned: 10 })], events: [] }, { outcomes: [ok(0, { Unsigned: 20 })], events: [] }];
  const { response, steps } = await round(guest, "scenario", tape);
  assert.equal(response.kind, "completedRound");
  assert.equal(steps, 3);
});

test("Promise.all batches into one Collect group: b{i}, limit min(n, 8); more than 32 is Exhausted", async () => {
  for (const n of [2, 5, 9, 32]) {
    const { response } = await round(guest, "all", [], { input: { Unsigned: n } });
    const group = response.plan.Group;
    assert.equal(group.format, "axiom-extension-effect-group/v1");
    assert.equal(group.mode, "Collect");
    assert.equal(group.limit, Math.min(n, 8));
    assert.deepEqual(group.branches.map((branch) => branch.name), Array.from({ length: n }, (_, i) => `b${i}`));
  }
  const { response } = await round(guest, "all", [], { input: { Unsigned: 33 } });
  assert.deepEqual(response.error, { code: "Exhausted", message: "33 host operations are pending together; one round group allows at most 32", retryable: false });
});

test("an early return never sends the pending operation; waiting on nothing is InvalidInput", async () => {
  const { response } = await round(guest, "early");
  assert.equal(response.kind, "completedRound");
  assert.deepEqual(response.result.output, { Unsigned: 7 });
  const forever = (await round(guest, "forever")).response;
  assert.equal(forever.error.code, "InvalidInput");
});

test("host errors reject with HostOperationError and keep their code when they escape", async () => {
  const tape = [{ outcomes: [err(0, "Denied", "no grant")], events: [] }];
  assert.deepEqual((await round(guest, "failing", tape)).response.error, { code: "Denied", message: "no grant", retryable: false });
  assert.deepEqual((await round(guest, "caught", tape)).response.result.output, { List: [{ Bool: true }, { String: "Denied" }] });
  assert.deepEqual((await round(guest, "throwing")).response.error, { code: "Guest", message: "bad", retryable: false });
  // A tape without the outcome: the operation rejects with Protocol.
  assert.equal((await round(guest, "failing", [{ outcomes: [], events: [] }])).response.error.code, "Protocol");
});

test("re-derived plans are reported for the kernel to hash; completing early is Protocol", async () => {
  // Round 2 re-derives load(2) for a tape recorded from load(1): the kernel
  // compares the hashes; the runner reports what it re-derived.
  const { response } = await round(guest, "divergent", [{ outcomes: [ok(0, { Unsigned: 1 })], events: [] }]);
  assert.deepEqual(response.replayed, [{ Plan: { effects: [{ namespace: "catalog", operation: "load", input: { Unsigned: 2 } }] } }]);
  const early = (await round(guest, "early", [{ outcomes: [], events: [] }])).response;
  assert.deepEqual(early.error, { code: "Protocol", message: "non-deterministic guest: the export completed after replaying 0 of 1 effect rounds", retryable: false });
});

test("on a 1.0 host an async export may yield once", async () => {
  const tape = [{ outcomes: [ok(0, { Unsigned: 10 })], events: [] }];
  assert.equal((await round(guest, "plain", [], { abi: "1.0" })).response.kind, "yieldedRound");
  assert.deepEqual((await round(guest, "plain", tape, { abi: "1.0" })).response.result.output, { Unsigned: 11 });
  const second = (await round(guest, "scenario", tape, { abi: "1.0" })).response;
  assert.deepEqual(second.error, { code: "Exhausted", message: "async export needs more than one effect round, but the host negotiated ABI 1.0, which allows one", retryable: false });
});

test("asyncExport, 1.0 handlers in a rounds module, and the 1.0 bridge refusing async exports", async () => {
  assert.equal((await round(guest, "explicit")).response.plan.Plan.effects[0].input.Unsigned, 5);
  const sync = (await round(guest, "sync", [], { input: { Unsigned: 4 } })).response;
  assert.deepEqual(sync, { kind: "completedRound", result: { output: { Unsigned: 4 }, patches: [], transactions: [], emitted_events: [] } });
  assert.throws(() => guest.exports.plain({}), /needs an ABI 1.1 managed build/);
  assert.equal(extension.export(guest.exports.sync), guest.exports.sync, "a non-async function stays a 1.0 handler");
});

// w2-c2 L-2: the same two texts as the kernel test canonical_round_plan_text_is_golden.
test("the canonical round-plan text matches the kernel golden text", () => {
  const golden = '{"Record":[{"name":"big","value":{"Unsigned":"18446744073709551615"}},{"name":"neg","value":{"Signed":"-9223372036854775808"}},{"name":"txt","value":{"String":"é\\"\\\\\\n\\u0001😀/"}},{"name":"bytes","value":{"Bytes":[0,255]}},{"name":"list","value":{"List":["Null",{"Bool":true}]}},{"name":"var","value":{"Variant":{"case":"some","value":{"Bool":false}}}},{"name":"none","value":{"Variant":{"case":"none","value":null}}},{"name":"h","value":{"Handle":{"id":"9007199254740993","generation":2,"kind":"Secret"}}}]}';
  const input = { Record: [
    { name: "big", value: { Unsigned: "18446744073709551615" } },
    { name: "neg", value: { Signed: "-9223372036854775808" } },
    { name: "txt", value: { String: 'é"\\\n\u0001😀/' } },
    { name: "bytes", value: { Bytes: [0, 255] } },
    { name: "list", value: { List: ["Null", { Bool: true }] } },
    { name: "var", value: { Variant: { case: "some", value: { Bool: false } } } },
    { name: "none", value: { Variant: { case: "none", value: null } } },
    { name: "h", value: { Handle: { id: "9007199254740993", generation: 2, kind: "Secret" } } },
  ] };
  const put = { namespace: "catalog", operation: "put", input };
  const quoted = { namespace: 'ns"', operation: "op", input: "Null" };
  assert.equal(canonicalRoundPlan({ Plan: { effects: [put, quoted] } }), `{"Plan":[["catalog","put",${golden}],["ns\\"","op","Null"]]}`);
  assert.equal(
    canonicalRoundPlan({ Group: { format: "axiom-extension-effect-group/v1", limit: 2, mode: "Collect", branches: [{ name: "b0", effect: put }, { name: "b1", effect: quoted }] } }),
    `{"Group":{"format":"axiom-extension-effect-group/v1","limit":2,"mode":"Collect","branches":[["b0","catalog","put",${golden}],["b1","ns\\"","op","Null"]]}}`,
  );
});

// w2-c2 L-2: a re-derived plan that differs from the kernel's tape text fails
// Protocol before any outcome of that round is used.
test("a re-derived plan that differs from the tape plan text fails Protocol at once", async () => {
  const LOAD = operation("catalog", "load");
  const PRICE = operation("catalog", "price");
  const ext = defineExtension({ exports: { run: asyncExport(async (cx) => { const a = await cx.call(LOAD.call(1)); return cx.call(PRICE.call(a)); }) } });
  const plan = { Plan: { effects: [{ namespace: "catalog", operation: "load", input: { Unsigned: 1 } }] } };
  const outcome = { outcomes: [{ index: 0, result: { Ok: { Unsigned: 10 } } }], events: [] };
  const run = async (tapePlans) => {
    const out = [];
    delete globalThis.__axiom_managed_step__;
    runRound(ext, { kind: "invokeRound", request_id: 1, invocation: invocation("run"), round: 2, max_rounds: 4, abi: "1.1", tape: [outcome], tape_plans: tapePlans }, (r) => out.push(r));
    let steps = 0;
    while (out.length === 0) {
      await drained();
      globalThis.__axiom_managed_step__();
      steps += 1;
    }
    return { response: out[0], steps };
  };
  // Matching text: replayed, then the next plan (two steps).
  const same = await run([canonicalRoundPlan(plan)]);
  assert.equal(same.response.kind, "yieldedRound");
  assert.equal(same.steps, 2);
  // Differing text: Protocol in the first step, before the outcome is used.
  const other = await run(['{"Plan":[["catalog","reserve",{"Unsigned":"1"}]]}']);
  assert.equal(other.steps, 1);
  assert.equal(other.response.kind, "failed");
  assert.equal(other.response.error.code, "Protocol");
  assert.match(other.response.error.message, /non-deterministic guest: the plan re-derived for effect round 1 differs from the tape/);
  // null (above the kernel's bound): no check here; the kernel's hash check decides.
  const unbounded = await run([null]);
  assert.equal(unbounded.response.kind, "yieldedRound");
});

// w2-c2 L-5: the ABI 1.0 runner does not import the async runtime; the async
// runner does. (Sync-only bundles are byte-identical to the pre-async build.)
test("the 1.0 runner carries no async runtime; the async runner does", () => {
  const runner = readFileSync(new URL("../dist/runner.js", import.meta.url), "utf8");
  const asyncRunner = readFileSync(new URL("../dist/runner-async.js", import.meta.url), "utf8");
  assert.doesNotMatch(runner, /from "\.\/async\.js"/);
  assert.match(runner, /export \{ runRound \} from "\.\/runner-async\.js"/);
  assert.match(asyncRunner, /from "\.\/async\.js"/);
  // Node runs the SDK unlowered (native async functions).
  assert.equal(LOWERED_ASYNC, false);
});

// ---- p-live: live rounds (the host keeps the engine and resumes the run) ----

/** Runs to completion on the live path: one start, then one resume per yield, all in this realm. */
async function driveLive(ext, name, hostFn, { input = "Null", maxRounds = 16 } = {}) {
  const written = [];
  const plans = [];
  const responses = [];
  delete globalThis.__axiom_managed_step__;
  runRound(ext, { kind: "invokeRound", request_id: 1, invocation: invocation(name, input), round: 1, max_rounds: maxRounds, abi: "1.1", tape: [], live: true }, (response) => written.push(response));
  let round = 1;
  let resume;
  for (;;) {
    let steps = 0;
    while (written.length === 0) {
      await drained();
      assert.ok(steps < 2 || (round === 1 && steps < 3), "a live round answers within two engine steps");
      globalThis.__axiom_managed_step__(resume);
      resume = undefined;
      steps += 1;
    }
    assert.equal(written.length, 1);
    const response = written.pop();
    responses.push(response);
    if (response.kind !== "yieldedRound") return { response, plans, responses };
    assert.equal(response.live, true, "a runner offered a live continuation keeps the run");
    assert.deepEqual(response.replayed, plans, "a live run reports the plans it holds");
    plans.push(response.plan);
    const effects = response.plan.Plan ? response.plan.Plan.effects : response.plan.Group.branches.map((branch) => branch.effect);
    round += 1;
    resume = { kind: "resumeRound", request_id: 1, round, record: { outcomes: effects.map((effect, index) => hostFn(effect, index)), events: [] } };
  }
}

/** The replay path's responses round by round (as `drive`, keeping each response). */
async function driveReplay(ext, name, hostFn, options = {}) {
  const tape = [];
  const plans = [];
  const responses = [];
  for (;;) {
    const { response } = await round(ext, name, tape, options);
    responses.push(response);
    if (response.kind !== "yieldedRound") return { response, plans, responses };
    plans.push(response.plan);
    const effects = response.plan.Plan ? response.plan.Plan.effects : response.plan.Group.branches.map((branch) => branch.effect);
    tape.push({ outcomes: effects.map((effect, index) => hostFn(effect, index)), events: [] });
  }
}

test("p-live: every deterministic export answers identically live and on replay, round by round", async () => {
  const cases = [["scenario"], ["all", { Unsigned: 3 }], ["all", { Unsigned: 40 }], ["plain"], ["early"], ["forever"], ["failing"], ["throwing"], ["caught"], ["explicit"]];
  for (const [name, input] of cases) {
    const options = input === undefined ? {} : { input };
    const replay = await driveReplay(guest, name, host, options);
    const live = await driveLive(guest, name, host, options);
    // The only difference on the wire is the `live` flag of each yield.
    const strip = (responses) => responses.map(({ live: _live, ...rest }) => rest);
    assert.deepEqual(strip(live.responses), strip(replay.responses), `${name}: live and replay answers differ`);
    assert.deepEqual(live.plans, replay.plans, `${name}: plans differ`);
  }
});

test("p-live: a resume that does not continue the suspended run fails Protocol; without `live` nothing is kept", async () => {
  // Wrong round.
  const written = [];
  delete globalThis.__axiom_managed_step__;
  runRound(guest, { kind: "invokeRound", request_id: 1, invocation: invocation("scenario"), round: 1, max_rounds: 4, abi: "1.1", tape: [], live: true }, (r) => written.push(r));
  while (written.length === 0) { await drained(); globalThis.__axiom_managed_step__(); }
  assert.equal(written.pop().live, true);
  globalThis.__axiom_managed_step__({ kind: "resumeRound", request_id: 1, round: 3, record: { outcomes: [ok(0, { Unsigned: 10 })], events: [] } });
  assert.equal(written.pop().error.code, "Protocol");
  // Without `live` in the request the yield carries no `live` flag (the replay host's bytes).
  const { response } = await round(guest, "scenario");
  assert.equal(response.kind, "yieldedRound");
  assert.equal("live" in response, false);
});

test("p-live: a guest whose plan depends on cx.round() is caught on replay but runs once live (documented difference)", async () => {
  // Replay re-derives load(2) for round 1's load(1): the kernel's hash check
  // refuses it (Protocol). Live, round 1's code ran once, so the plan it
  // reports is the one it yielded and the kernel's check passes.
  const replay = await driveReplay(guest, "divergent", host);
  assert.notDeepEqual(replay.response.replayed, replay.plans);
  const live = await driveLive(guest, "divergent", host);
  assert.equal(live.response.kind, "completedRound");
  assert.deepEqual(live.response.replayed, live.plans);
});
