// Runs an AOT-compiled Python extension (axiom-build python/compile.py output)
// against this repository's TypeScript SDK, the way the managed host runs it:
// the SDK and the bundle share one JavaScript realm, and every exchange gets a
// fresh realm ("fresh"). The "persistent" run keeps one realm for invoke and
// resume, as an in-process test would.
// Usage: node aot_harness.mjs <generated.ts> <typescript/dist> <esbuild> <cases.json>
// cases.json: {"invoke": [{"export": "...", "input": <AxiomValue>}], "outcomes": [...]}
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";

const [generated, dist, esbuild, casesFile] = process.argv.slice(2);
const cases = JSON.parse(fs.readFileSync(casesFile, "utf8"));
const sdk = execFileSync(esbuild, [path.join(dist, "index.js"), "--bundle", "--format=iife", "--global-name=__SDK", "--log-level=warning"], { encoding: "utf8" });
const source = fs.readFileSync(generated, "utf8")
  .replace(/^import \{ defineExtension \} from "@axiomcore\/extension-sdk";$/m, "const { defineExtension } = __SDK;")
  .replace(/^import \{ selectors \} from "@axiomcore\/extension-bindings";$/m, "const selectors = {};")
  .replace(/^export default /m, "globalThis.__EXT = ");
if (!source.includes("const { defineExtension } = __SDK;")) throw new Error("unexpected compiler output: SDK import not found");

function realm() {
  const context = vm.createContext({ console });
  vm.runInContext(sdk, context);
  vm.runInContext(source, context, { filename: generated, timeout: 20000 });
  return context;
}
function exchange(context, code, value) {
  context.__ARG = JSON.stringify(value);
  return JSON.parse(vm.runInContext(`(() => { try { return JSON.stringify({ ok: ${code} }); } catch (e) { return JSON.stringify({ err: String(e && e.message || e) }); } })()`, context, { timeout: 20000 }));
}
const invoke = (context, item) => exchange(context, `__EXT.exports[${JSON.stringify(item.export)}](__SDK.createContext(JSON.parse(__ARG)))`, { export: item.export, input: item.input, snapshots: [], deadline_unix_ms: 0 });
const resume = (context, record) => exchange(context, `__EXT.resume(__SDK.createResumeContext(JSON.parse(__ARG).outcomes, [], 1, JSON.parse(__ARG).record))`, { outcomes: cases.outcomes, record });

// The managed kernel's record of a yielded plan (the bridge's `yielded_plan`):
// per call namespace, operation and the input's canonical text and length
// (same format as the kernel's `canonical_value`).
function canonical(v) {
  if (v === "Null") return '"Null"';
  if ("Bool" in v) return `{"Bool":${v.Bool === true}}`;
  if ("Signed" in v) return `{"Signed":${JSON.stringify(String(v.Signed))}}`;
  if ("Unsigned" in v) return `{"Unsigned":${JSON.stringify(String(v.Unsigned))}}`;
  if ("String" in v) return `{"String":${JSON.stringify(v.String)}}`;
  if ("Bytes" in v) return `{"Bytes":[${v.Bytes.join(",")}]}`;
  if ("List" in v) return `{"List":[${v.List.map(canonical).join(",")}]}`;
  if ("Record" in v) return `{"Record":[${v.Record.map((f) => `{"name":${JSON.stringify(f.name)},"value":${canonical(f.value)}}`).join(",")}]}`;
  throw new Error("harness: unsupported value");
}
const record = (plan) => plan.effects.map((e) => { const input = canonical(e.input); return { namespace: e.namespace, operation: e.operation, input_length: input.length, input }; });

const result = { fresh: {}, persistent: {} };
for (const item of cases.invoke) result.fresh[item.export] = invoke(realm(), item);
result.fresh.resume = resume(realm());
// Fresh realm with the kernel's yielded-plan record, as the current host sends it.
const yieldedPlan = result.fresh[cases.invoke[0].export].ok.plan;
result.echo = { resume: resume(realm(), record(yieldedPlan)) };
// A record of a different plan (calls swapped) is rejected, naming the call.
result.echo.swapped = resume(realm(), record({ effects: [...yieldedPlan.effects].reverse() }));
const persistent = realm();
result.persistent[cases.invoke[0].export] = invoke(persistent, cases.invoke[0]);
result.persistent.resume = resume(persistent);
console.log(JSON.stringify(result));
