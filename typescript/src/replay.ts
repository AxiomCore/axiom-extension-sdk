// X9d: replay a dev replay tape (`axiom-extension-replay-tape` version 1,
// contract x9d-replay-tape.md) against an extension definition, in a test
// (node). Test helper only: never part of an extension bundle.
//
// The guest gets the recorded input and round-1 snapshots. Each plan it yields
// must equal the recorded plan (content; the plan hash is a function of it),
// otherwise `replayTape` throws a `ReplayDifference` naming the first
// differing call. Outcomes and events come from the tape. The final result,
// failure, or the host's stop at the recorded point must equal the recording.
// ABI 1.1 tapes (async exports, sync exports in a 1.1 module) and ABI 1.0
// tapes (one yield, resume) are driven through the SDK's own round runner,
// the way the managed kernel drives it: start, then step each time the job
// queue has drained.
import type { ExtensionDefinition } from "./index.js";
import type { RoundRequest, RoundResponse } from "./async.js";
import { runRound, type AsyncRunnerOptions } from "./runner-async.js";

/** Same cap as the broker's `MAX_TAPE_FILE_BYTES`. */
export const MAX_TAPE_FILE_BYTES = 32 * 1024 * 1024;

// deno-lint-ignore no-explicit-any
type Json = any;

export interface ReplayTapeDocument {
  format: "axiom-extension-replay-tape";
  version: 1;
  header: { notice: string; replayable: boolean; sensitive: string; contains: string[] };
  identity: { application: string; extension: string; moduleSha256?: string };
  abi: "1.0" | "1.1";
  maxRounds: number;
  invocation: { export: string; input: Json; snapshots: Json[] };
  rounds: { round: number; planHash: string; plan: Json; status: "executed" | "refused" | "failed"; outcomes?: Json[]; events?: Json[] }[];
  terminal: { status: string; result?: Json; error?: { code: string; message: string; kind?: string }; committed: boolean };
  integrity: { algorithm: string; digest: string };
  [key: string]: Json;
}

export interface ReplayReport {
  /** Plans the guest yielded (each equal to the recorded plan). */
  yields: number;
  /** Recorded terminal status. */
  status: string;
  /** The guest's final result, when it completed. */
  result?: Json;
}

/** A difference between the recording and the replay. */
export class ReplayDifference extends Error {
  constructor(readonly at: string, readonly recorded: string, readonly replayed: string) {
    super(`AXIOM_EXTENSION_REPLAY_DIFFERS: ${at}: recorded ${recorded} but the guest ${replayed}`);
    this.name = "ReplayDifference";
  }
}

/**
 * An integer outside JavaScript's exact range (|n| > 2^53 - 1), kept as its
 * decimal text so the tape digest and the replay see the exact value. The
 * replay hands it to the guest as an exact decimal string, as the managed
 * kernel does for every integer outside that range.
 */
export class ExactInteger {
  constructor(readonly text: string) {}
  toJSON(): string {
    return this.text;
  }
}

/** Nesting deeper than this is refused (the Rust reader's limit). */
export const MAX_TAPE_DEPTH = 128;

/** Canonical JSON: keys sorted, no whitespace (the tape digest's input). */
export function canonicalJson(value: Json): string {
  if (value instanceof ExactInteger) return value.text;
  if (typeof value === "bigint") return value.toString();
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  const keys = Object.keys(value).sort();
  return `{${keys.map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
}

/**
 * Parse JSON exactly: integers outside the exact range become
 * `ExactInteger`, a duplicate object key, trailing content and nesting deeper
 * than `MAX_TAPE_DEPTH` are refused (the digest covers the parsed content, so
 * a document that could parse to two meanings is not accepted).
 */
export function parseExactJson(text: string): Json {
  let at = 0;
  const error = (message: string): never => fail(`not JSON: ${message} at offset ${at}`);
  const space = () => {
    while (at < text.length && " \t\n\r".includes(text[at])) at++;
  };
  const value = (depth: number): Json => {
    if (depth > MAX_TAPE_DEPTH) fail(`nesting deeper than ${MAX_TAPE_DEPTH}`);
    space();
    const c = text[at];
    if (c === "{") {
      at++;
      const out: Record<string, Json> = {};
      const seen = new Set<string>();
      space();
      if (text[at] === "}") {
        at++;
        return out;
      }
      for (;;) {
        space();
        if (text[at] !== "\"") error("expected a key");
        const key = string();
        if (seen.has(key)) fail(`malformed tape: duplicate key \`${key}\``);
        seen.add(key);
        space();
        if (text[at] !== ":") error("expected `:`");
        at++;
        // defineProperty: a `__proto__` key is data, never the prototype.
        Object.defineProperty(out, key, { value: value(depth + 1), enumerable: true, writable: true, configurable: true });
        space();
        if (text[at] === ",") {
          at++;
          continue;
        }
        if (text[at] === "}") {
          at++;
          return out;
        }
        error("expected `,` or `}`");
      }
    }
    if (c === "[") {
      at++;
      const out: Json[] = [];
      space();
      if (text[at] === "]") {
        at++;
        return out;
      }
      for (;;) {
        out.push(value(depth + 1));
        space();
        if (text[at] === ",") {
          at++;
          continue;
        }
        if (text[at] === "]") {
          at++;
          return out;
        }
        error("expected `,` or `]`");
      }
    }
    if (c === "\"") return string();
    for (const [word, result] of [["true", true], ["false", false], ["null", null]] as const) {
      if (text.startsWith(word, at)) {
        at += word.length;
        return result;
      }
    }
    const match = /^-?(?:0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?/.exec(text.slice(at, at + 400));
    if (!match) error("unexpected character");
    at += match![0].length;
    const number = Number(match![0]);
    if (match![1] === undefined && match![2] === undefined && !Number.isSafeInteger(number)) return new ExactInteger(match![0]);
    return number;
  };
  const string = (): string => {
    const start = at;
    at++;
    while (at < text.length && text[at] !== "\"") at += text[at] === "\\" ? 2 : 1;
    if (at >= text.length) error("unterminated string");
    at++;
    return JSON.parse(text.slice(start, at));
  };
  const result = value(0);
  space();
  if (at !== text.length) error("trailing content");
  return result;
}

/** The tape with every `ExactInteger` as the exact decimal string the guest sees. */
function bridged(value: Json): Json {
  if (value instanceof ExactInteger) return value.text;
  if (Array.isArray(value)) return value.map(bridged);
  if (value !== null && typeof value === "object") {
    const out: Record<string, Json> = {};
    for (const key of Object.keys(value)) out[key] = bridged(value[key]);
    return out;
  }
  return value;
}

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
  0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
  0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
  0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

/** SHA-256 of `bytes`, lower-case hex (no platform crypto needed). */
export function sha256Hex(bytes: Uint8Array): string {
  const length = bytes.length;
  const padded = new Uint8Array(((length + 9 + 63) >> 6) << 6);
  padded.set(bytes);
  padded[length] = 0x80;
  const bits = length * 8;
  const view = new DataView(padded.buffer);
  view.setUint32(padded.length - 8, Math.floor(bits / 0x100000000));
  view.setUint32(padded.length - 4, bits >>> 0);
  const h = new Uint32Array([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]);
  const w = new Uint32Array(64);
  for (let offset = 0; offset < padded.length; offset += 64) {
    for (let i = 0; i < 16; i++) w[i] = view.getUint32(offset + i * 4);
    for (let i = 16; i < 64; i++) {
      const a = w[i - 15], b = w[i - 2];
      const s0 = ((a >>> 7) | (a << 25)) ^ ((a >>> 18) | (a << 14)) ^ (a >>> 3);
      const s1 = ((b >>> 17) | (b << 15)) ^ ((b >>> 19) | (b << 13)) ^ (b >>> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) >>> 0;
    }
    let [a, b, c, d, e, f, g, hh] = h;
    for (let i = 0; i < 64; i++) {
      const s1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
      const t1 = (hh + s1 + ((e & f) ^ (~e & g)) + K[i] + w[i]) >>> 0;
      const s0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
      const t2 = (s0 + ((a & b) ^ (a & c) ^ (b & c))) >>> 0;
      hh = g; g = f; f = e; e = (d + t1) >>> 0; d = c; c = b; b = a; a = (t1 + t2) >>> 0;
    }
    h[0] += a; h[1] += b; h[2] += c; h[3] += d; h[4] += e; h[5] += f; h[6] += g; h[7] += hh;
  }
  return Array.from(h, (word) => word.toString(16).padStart(8, "0")).join("");
}

function fail(message: string): never {
  throw new Error(`AXIOM_EXTENSION_REPLAY_TAPE: ${message}`);
}

/** Parse and verify a tape: size, format, version, integrity, replayable. */
export function parseTape(text: string): ReplayTapeDocument {
  const bytes = new TextEncoder().encode(text);
  if (bytes.length > MAX_TAPE_FILE_BYTES) fail(`tape is ${bytes.length} bytes, over the ${MAX_TAPE_FILE_BYTES}-byte limit`);
  return verifyTape(parseExactJson(text));
}

/** Integers JSON.parse already rounded cannot be verified or replayed exactly. */
function inexact(value: Json): boolean {
  if (typeof value === "number") return Number.isInteger(value) && !Number.isSafeInteger(value);
  if (Array.isArray(value)) return value.some(inexact);
  if (value !== null && typeof value === "object" && !(value instanceof ExactInteger)) return Object.values(value).some(inexact);
  return false;
}

/**
 * Verify an already-parsed tape the way `parseTape` verifies text: format,
 * version, integrity, replayable. An object holding an integer outside the
 * exact range as a plain number (JSON.parse rounded it) is refused: pass the
 * tape text, or parse it with `parseExactJson`.
 */
export function verifyTape(tape: Json): ReplayTapeDocument {
  if (tape === null || typeof tape !== "object" || Array.isArray(tape)) fail("not an axiom-extension-replay-tape document");
  if (tape.format !== "axiom-extension-replay-tape") fail("not an axiom-extension-replay-tape document");
  if (tape.version !== 1) fail("unsupported tape version (this reader knows version 1)");
  if (inexact(tape)) fail("the tape holds an integer outside the exact range as a rounded number; pass the tape text (parseTape) instead of a JSON.parse result");
  const { integrity, ...content } = tape;
  if (!integrity) fail("tape has no integrity digest");
  if (integrity.algorithm !== "sha256" || integrity.digest !== sha256Hex(new TextEncoder().encode(canonicalJson(content)))) {
    fail("tape failed its integrity check (content changed after recording)");
  }
  if (tape.header?.replayable !== true) fail("tape has redacted values and cannot be replayed byte-exactly");
  return tape as ReplayTapeDocument;
}

/** Identity a replay can check (contract x9d, "Replay identity"). */
export interface ReplayExpectation {
  /** The extension the caller replays (the tape's `identity.extension`). */
  extension?: string;
  /** The application (the tape's `identity.application`). */
  application?: string;
  /** The module under test, when known: a different module is a different build. */
  moduleSha256?: string;
  /** Replay against a different build of the same extension (module differs). */
  allowIdentityMismatch?: boolean;
}

function checkIdentity(doc: ReplayTapeDocument, expect: ReplayExpectation | undefined): void {
  if (!expect) return;
  const refuse = (message: string): never => {
    throw new Error(`AXIOM_EXTENSION_REPLAY_IDENTITY: ${message}`);
  };
  if (expect.extension !== undefined && doc.identity.extension !== expect.extension) {
    refuse(`the tape was recorded for extension \`${doc.identity.extension}\`, not \`${expect.extension}\``);
  }
  if (expect.application !== undefined && doc.identity.application !== expect.application) {
    refuse(`the tape was recorded by application \`${doc.identity.application}\`, not \`${expect.application}\``);
  }
  const recorded = doc.identity.moduleSha256;
  if (expect.moduleSha256 !== undefined && recorded !== undefined && recorded !== expect.moduleSha256 && !expect.allowIdentityMismatch) {
    refuse(`the tape was recorded by module ${recorded}, not ${expect.moduleSha256}; set allowIdentityMismatch to replay against a different build`);
  }
}

function call(effect: Json, branch?: string): string {
  const text = `${effect.namespace}.${effect.operation}(${JSON.stringify(effect.input)})`;
  return branch === undefined ? text : `${text} as branch \`${branch}\``;
}

function calls(plan: Json): string[] {
  if (plan?.Plan) return plan.Plan.effects.map((effect: Json) => call(effect));
  if (plan?.Group) return plan.Group.branches.map((branch: Json) => call(branch.effect, branch.name));
  return [];
}

function comparePlan(round: number, recorded: Json, replayed: Json): void {
  if (canonicalJson(recorded) === canonicalJson(replayed)) return;
  const kind = (plan: Json) => (plan?.Plan ? "plan" : "group");
  const [a, b] = [calls(recorded), calls(replayed)];
  if (kind(recorded) !== kind(replayed)) {
    throw new ReplayDifference(`round ${round}`, `a ${kind(recorded)} of ${a.length} call(s)`, `yielded a ${kind(replayed)} of ${b.length} call(s)`);
  }
  for (let index = 0; index < Math.max(a.length, b.length); index++) {
    if (a[index] !== b[index]) {
      throw new ReplayDifference(`round ${round}, call ${index}`, a[index] ?? "no call", `yielded ${b[index] ?? "no call"}`);
    }
  }
  throw new ReplayDifference(`round ${round}`, `group settings ${canonicalJson(recorded)}`, `yielded ${canonicalJson(replayed)}`);
}

const drained = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

/** One round through the SDK runner, stepping after each drained job queue. */
async function runOne(extension: ExtensionDefinition, request: RoundRequest, options: AsyncRunnerOptions): Promise<RoundResponse> {
  const written: RoundResponse[] = [];
  const g = globalThis as Json;
  delete g.__axiom_managed_step__;
  runRound(extension, request, (response) => written.push(response), options);
  for (let steps = 0; written.length === 0; steps++) {
    await drained();
    if (steps > request.tape.length + 64) fail("the guest did not answer the round (it waits for something that is not a host operation)");
    if (typeof g.__axiom_managed_step__ !== "function") fail("the guest neither answered nor installed a step hook");
    g.__axiom_managed_step__();
  }
  return written[0];
}

/** Replay `tape` against `extension`; throws `ReplayDifference` on a difference. */
export async function replayTape(
  extension: ExtensionDefinition,
  tape: ReplayTapeDocument | string,
  options: AsyncRunnerOptions & { expect?: ReplayExpectation } = {},
): Promise<ReplayReport> {
  // An object is verified exactly like text (w3-vbc finding 10).
  const verified = typeof tape === "string" ? parseTape(tape) : verifyTape(tape);
  if (verified.abi !== "1.0" && verified.abi !== "1.1") fail(`unknown abi \`${String(verified.abi)}\``);
  checkIdentity(verified, options.expect);
  const doc = bridged(verified) as ReplayTapeDocument;
  const invocation = { export: doc.invocation.export, input: doc.invocation.input, snapshots: doc.invocation.snapshots, deadline_unix_ms: 0 };
  const records: { outcomes: Json[]; events: Json[] }[] = [];
  const stoppedAfterLast = doc.terminal.result === undefined && ["refused", "cancelled", "sandboxFailed"].includes(doc.terminal.status);
  for (let round = 1; round <= doc.maxRounds + 1; round++) {
    const request = { kind: "invokeRound", request_id: 1, invocation, round, max_rounds: doc.maxRounds, abi: doc.abi, tape: records.slice() } as unknown as RoundRequest;
    const response = (await runOne(extension, request, options)) as Json;
    const index = records.length;
    const recorded = doc.rounds[index];
    if (response.kind === "yieldedRound") {
      if (Array.isArray(response.replayed)) {
        const expected = doc.rounds.slice(0, index).map((r) => canonicalJson(r.plan));
        if (canonicalJson(response.replayed.map((p: Json) => JSON.parse(canonicalJson(p)))) !== `[${expected.join(",")}]`) {
          throw new ReplayDifference(`round ${round}`, "the earlier plans on replay", "re-derived different earlier plans");
        }
      }
      if (!recorded) throw new ReplayDifference(`round ${round}`, `no further yield (the invocation ended \`${doc.terminal.status}\`)`, `yielded ${calls(response.plan).join(", ")}`);
      comparePlan(recorded.round, recorded.plan, response.plan);
      const last = index + 1 === doc.rounds.length;
      if (recorded.status !== "executed" || (last && stoppedAfterLast)) return { yields: index + 1, status: doc.terminal.status };
      records.push({ outcomes: recorded.outcomes ?? [], events: recorded.events ?? [] });
      continue;
    }
    if (recorded) throw new ReplayDifference(`round ${recorded.round}`, `a yield of ${calls(recorded.plan).join(", ")}`, `replied ${response.kind}`);
    if (response.kind === "completedRound") {
      if (doc.terminal.result === undefined) throw new ReplayDifference("result", `\`${doc.terminal.status}\``, "completed");
      if (canonicalJson(doc.terminal.result) !== canonicalJson(response.result)) {
        throw new ReplayDifference("result", canonicalJson(doc.terminal.result), `returned ${canonicalJson(response.result)}`);
      }
      return { yields: index, status: doc.terminal.status, result: response.result };
    }
    if (response.kind === "failed") {
      const error = doc.terminal.error;
      if (doc.terminal.status === "guestFailed" && error && error.code === response.error.code && error.message === response.error.message) {
        return { yields: index, status: doc.terminal.status };
      }
      throw new ReplayDifference("result", `\`${doc.terminal.status}\` ${JSON.stringify(error)}`, `failed ${JSON.stringify(response.error)}`);
    }
    throw new ReplayDifference("result", `\`${doc.terminal.status}\``, `replied ${JSON.stringify(response)}`);
  }
  throw new ReplayDifference("rounds", `at most ${doc.maxRounds} yield(s)`, "yielded more");
}
