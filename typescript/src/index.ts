export { Cents, Percentage } from "./money.js";
export { HostOperationError } from "./errors.js";
export {
  EFFECT_GROUP_FORMAT, MAX_GROUP_BRANCHES, asyncExport, asyncHandlerOf, extension, segmentPlan,
  type AsyncContext, type AsyncExport, type AsyncHandler,
} from "./async.js";
import type { AsyncExport } from "./async.js";
import { HostOperationError as HostError } from "./errors.js";

export type AxiomValue =
  | "Null"
  | { Bool: boolean }
  // 64-bit values outside the JS safe-integer range travel as exact decimal
  // strings and decode to `bigint`; they are never rounded.
  | { Signed: number | string }
  | { Unsigned: number | string }
  | { String: string }
  | { Bytes: number[] }
  | { List: AxiomValue[] }
  | { Record: Array<{ name: string; value: AxiomValue }> }
  | { Variant: { case: string; value: AxiomValue | null } }
  | { Handle: { id: number | string; generation: number; kind: string } };

declare const secretBrand: unique symbol;
export type SecretHandle = {readonly id:number|string;readonly generation:number;readonly kind:"Secret";readonly [secretBrand]:true};
function checkedSecret(value:unknown):SecretHandle {
  if(value===null||typeof value!=="object") throw new Error("secret injection is unavailable");
  const handle=value as {id?:number|string;generation?:number;kind?:string};
  // Handle IDs are opaque 64-bit values; large ones arrive as exact decimal strings.
  const exactId=Number.isSafeInteger(handle.id)&&(handle.id as number)>0||typeof handle.id==="string"&&/^[1-9][0-9]{0,19}$/.test(handle.id);
  if(handle.kind!=="Secret"||!exactId||!Number.isSafeInteger(handle.generation)) throw new Error("secret injection is unavailable");
  return handle as SecretHandle;
}
export function authenticatedRequest(binding:Pick<Effect,"namespace"|"operation">,path:string,body:Uint8Array|readonly number[],secret:SecretHandle):Effect {
  return {...binding,input:{Record:[{name:"body",value:{Bytes:Array.from(body)}},{name:"path",value:{String:path}},{name:"secret",value:{Handle:checkedSecret(secret)}}]}};
}

export interface Snapshot {
  resource: string;
  revision: number;
  value: AxiomValue;
}

declare const selectorBrand: unique symbol;
export type Selector<T = unknown> = string & { readonly [selectorBrand]: T };

export interface Invocation {
  export: string;
  input: AxiomValue;
  snapshots: Snapshot[];
  deadline_unix_ms: number;
}

export type PatchOperation =
  | { Set: { path: string[]; value: AxiomValue } }
  | { Unset: { path: string[] } }
  | { Increment: { path: string[]; amount: number } }
  | { Append: { path: string[]; value: AxiomValue } }
  | { CompareAndSet: { path: string[]; expected: AxiomValue; replacement: AxiomValue } }
  | { Dispatch: { action: string; payload: AxiomValue } };

export interface Patch {
  resource: string;
  expected_revision: number;
  operations: PatchOperation[];
}

export interface InvocationResult {
  output: AxiomValue;
  patches: Patch[];
  transactions: Array<{ transaction_id: number; patches: Patch[] }>;
  emitted_events: Array<{ sequence: number; value: AxiomValue }>;
}

export interface Effect {
  namespace: string;
  operation: string;
  input: AxiomValue;
}

export type Completed = { kind: "completed"; result: InvocationResult };
export type Yielded = { kind: "yielded"; plan: { effects: Effect[] } };
export type Failed = {
  kind: "failed";
  error: { code: string; message: string; retryable: boolean };
};
export type ExtensionResponse = Completed | Yielded | Failed | { kind: "cancelled" };

export interface ExtensionContext {
  invocation: Invocation;
  input: unknown;
  secret(alias:string):SecretHandle;
  authenticated_request(namespace:string,operation:string,path:string,body:Uint8Array|readonly number[],secret:SecretHandle):Effect;
  snapshot(resource: string): StateSnapshot;
  patch(resource: string): PatchBuilder;
  /** Yield an effect plan: either the plan built with {@link Effects} (typed tickets) or raw effects. */
  effects(plan: Effects): Yielded;
  effects(...effects: Effect[]): Yielded;
  /** A new effect plan; `plan.push(effect)` returns a typed {@link Ticket}. */
  plan(): Effects;
  complete(output?: unknown, changes?: Partial<InvocationResult>): Completed;
}

export type ExtensionHandler = (context: ExtensionContext) => ExtensionResponse;
export interface ExtensionDefinition {
  /** 1.0 handlers, and async exports (`asyncExport`, ABI 1.1 rounds). */
  exports: Record<string, ExtensionHandler | AsyncExport>;
  resume?: (context: ResumeContext) => ExtensionResponse;
}

export interface ResumeContext {
  requestId?: number;
  outcomes: unknown[];
  events: unknown[];
  /**
   * Decode the outcome of the host call that issued `ticket`, typed as the
   * operation's output. A ticket from another plan or yield step throws.
   */
  get<O>(ticket: Ticket<O>): O;
  /**
   * A new effect plan for rebuilding the yielded plan's tickets in this
   * exchange (managed hosts run every exchange in a fresh engine). The rebuilt
   * plan must make exactly the yielded calls (namespace, operation and input)
   * in the same order: `get` checks it against the host's record of the
   * yielded plan, or against this heap's own record, and throws naming the
   * first call that differs.
   */
  plan(): Effects;
  /**
   * Positional outcome access.
   * @deprecated Use typed tickets: `const t = plan.push(op.call(input))`, then `resume.get(t)`.
   */
  outcome(index: number): unknown;
  complete(output?: unknown): Completed;
}

declare const ticketBrand: unique symbol;
/**
 * A typed claim on the outcome of one host call in one effect plan, returned
 * by {@link Effects.push}. Only the SDK can issue one; `index` is the ABI
 * outcome index (declaration order). `O` is invariant.
 */
export interface Ticket<O> {
  readonly index: number;
  readonly [ticketBrand]: (value: O) => O;
}

declare const outputBrand: unique symbol;
/** An effect whose outcome decodes as `O`. A plain {@link Effect} is a `TypedEffect<unknown>`. */
export type TypedEffect<O> = Effect & { readonly [outputBrand]?: (value: O) => O };

/** A typed host operation: `op.call(input)` is a {@link TypedEffect} for {@link Effects.push}. */
export interface Operation<I, O> {
  readonly namespace: string;
  readonly operation: string;
  call(input: I): TypedEffect<O>;
}

// Guest-local bookkeeping. Nothing here reaches the wire: plan bytes are the
// effect objects exactly as given, in call order.
type Decoder = (value: AxiomValue) => unknown;
const effectDecoders = new WeakMap<object, Decoder>();
const tickets = new WeakMap<object, { plan: Effects; index: number; decode?: Decoder }>();
const planEffects = new WeakMap<Effects, { effects: Effect[]; sealed: boolean; yieldedIn?: number }>();
/** The plan most recently yielded in this JavaScript heap, if any. */
let lastYielded: Effects | undefined;
/** Counts invocations started in this heap; a yielded plan records its invocation. */
let invocationGeneration = 0;

class IssuedTicket {
  constructor(readonly index: number) { Object.freeze(this); }
}

/** The output decoder attached to `effect` by {@link typedEffect} or {@link operation}, if any. */
export function effectDecoder(effect: object): ((value: AxiomValue) => unknown) | undefined {
  return effect !== null && typeof effect === "object" ? effectDecoders.get(effect) : undefined;
}

/** Attach an output decoder (and type) to an effect. The effect object itself is unchanged. */
export function typedEffect<O>(effect: Effect, decode?: (value: AxiomValue) => O): TypedEffect<O> {
  if (decode) effectDecoders.set(effect, decode);
  return effect as TypedEffect<O>;
}

/** Declare a typed host operation. Without a codec the input is `encodeValue`d and the output `decodeValue`d. */
export function operation<I = unknown, O = unknown>(namespace: string, operation: string,
  codec: { encode?: (input: I) => AxiomValue; decode?: (value: AxiomValue) => O } = {}): Operation<I, O> {
  return Object.freeze({ namespace, operation,
    call(input: I): TypedEffect<O> {
      return typedEffect<O>({ namespace, operation, input: codec.encode ? codec.encode(input) : encodeValue(input) }, codec.decode);
    } });
}

/**
 * An effect plan built call by call. `plan.push(effect)` returns a typed
 * {@link Ticket}; yield the plan with `context.effects(plan)` or
 * {@link yieldFor}. The yielded bytes equal `context.effects(...effects)`.
 */
export class Effects {
  constructor() { planEffects.set(this, { effects: [], sealed: false }); }

  /**
   * Same as {@link Effects.push}.
   * @deprecated Use `plan.push(effect)`, the spelling shared by TypeScript,
   * Python and Rust (Rust's `Effects::call` is the by-value chain and returns
   * no ticket; Python sources cannot use a member named `call`).
   */
  call<O>(effect: TypedEffect<O>): Ticket<O> { return this.push(effect); }

  /** Add a host call to the plan; the returned ticket reads its typed outcome in the resume. */
  push<O>(effect: TypedEffect<O>): Ticket<O> {
    const state = planEffects.get(this)!;
    if (state.sealed) throw new Error("effect plan was already yielded or read; build a new plan");
    if (effect === null || typeof effect !== "object") throw new Error("effect plan entries must be effects");
    const ticket = new IssuedTicket(state.effects.length);
    tickets.set(ticket, { plan: this, index: state.effects.length, decode: effectDecoders.get(effect) });
    state.effects.push(effect);
    return ticket as unknown as Ticket<O>;
  }

  /** Number of host calls in the plan. */
  get length(): number { return planEffects.get(this)!.effects.length; }
}

/** Suspend the invocation while the host runs `plan`; continue in `resume`. */
export function yieldFor(plan: Effects): Yielded {
  const state = planEffects.get(plan);
  if (!state) throw new Error("yieldFor expects an Effects plan");
  state.sealed = true;
  state.yieldedIn = invocationGeneration;
  lastYielded = plan;
  return { kind: "yielded", plan: { effects: [...state.effects] } };
}

/**
 * One yielded host call as the managed host echoes it on resume (host-internal
 * bridge JSON, not the extension ABI). `input` is the input's canonical text,
 * present while it fits the host's inline budget; `input_length` is that
 * text's length, always present.
 */
export interface YieldedCall {
  namespace: string;
  operation: string;
  input_length: number;
  input?: string;
}

/**
 * The canonical text of an ABI value. It must stay identical to
 * `canonical_value` in the TypeScript kernel (`axiom-extension-typescript-kernel`):
 * fixed key order, every integer as an exact decimal string, JSON string escaping.
 */
export function canonicalValue(value: AxiomValue): string {
  if (value === "Null") return "\"Null\"";
  if (value === null || typeof value !== "object") return JSON.stringify(value) ?? "undefined";
  const v = value as Record<string, any>;
  if ("Bool" in v) return `{"Bool":${v.Bool === true}}`;
  if ("Signed" in v) return `{"Signed":${JSON.stringify(String(v.Signed))}}`;
  if ("Unsigned" in v) return `{"Unsigned":${JSON.stringify(String(v.Unsigned))}}`;
  if ("String" in v) return `{"String":${JSON.stringify(String(v.String))}}`;
  if ("Bytes" in v) return `{"Bytes":[${Array.from(v.Bytes as ArrayLike<number>, (byte) => String(byte)).join(",")}]}`;
  if ("List" in v) return `{"List":[${(v.List as AxiomValue[]).map(canonicalValue).join(",")}]}`;
  if ("Record" in v) return `{"Record":[${(v.Record as Array<{ name: string; value: AxiomValue }>).map((field) => `{"name":${JSON.stringify(String(field.name))},"value":${canonicalValue(field.value)}}`).join(",")}]}`;
  if ("Variant" in v) {
    const variant = v.Variant as { case: string; value: AxiomValue | null };
    return `{"Variant":{"case":${JSON.stringify(String(variant.case))},"value":${variant.value == null ? "null" : canonicalValue(variant.value)}}}`;
  }
  if ("Handle" in v) {
    const handle = v.Handle as { id: number | string; generation: number; kind: string };
    return `{"Handle":{"id":${JSON.stringify(String(handle.id))},"generation":${String(handle.generation)},"kind":${JSON.stringify(String(handle.kind))}}}`;
  }
  return JSON.stringify(value) ?? "undefined";
}

const callName = (call: { namespace: string; operation: string }): string => `${call.namespace}.${call.operation}`;

/** A description of the first call where `effects` differs from the host's record, or undefined when they match. */
function differsFromYielded(effects: Effect[], yielded: YieldedCall[]): string | undefined {
  if (effects.length !== yielded.length) return `the rebuilt plan has ${effects.length} call(s), the yielded plan had ${yielded.length}`;
  for (let i = 0; i < effects.length; i++) {
    const effect = effects[i];
    const expected = yielded[i];
    if (effect.namespace !== expected.namespace || effect.operation !== expected.operation)
      return `call ${i} is ${callName(effect)}, the yielded call ${i} was ${callName(expected)}`;
    const text = canonicalValue(effect.input);
    if (text.length !== expected.input_length || (expected.input !== undefined && text !== expected.input))
      return `call ${i} (${callName(effect)}) has a different input than the yielded call ${i}`;
  }
  return undefined;
}

/** The first call where two plans in this heap differ (namespace, operation or input), or undefined. */
function differsFromPlan(effects: Effect[], yielded: Effect[]): string | undefined {
  if (effects.length !== yielded.length) return `the rebuilt plan has ${effects.length} call(s), the yielded plan had ${yielded.length}`;
  for (let i = 0; i < effects.length; i++) {
    if (effects[i].namespace !== yielded[i].namespace || effects[i].operation !== yielded[i].operation)
      return `call ${i} is ${callName(effects[i])}, the yielded call ${i} was ${callName(yielded[i])}`;
    if (canonicalValue(effects[i].input) !== canonicalValue(yielded[i].input))
      return `call ${i} (${callName(effects[i])}) has a different input than the yielded call ${i}`;
  }
  return undefined;
}

const validYieldedPlan = (plan: unknown): plan is YieldedCall[] => Array.isArray(plan) && plan.every((call) =>
  call !== null && typeof call === "object" && typeof call.namespace === "string" && typeof call.operation === "string"
  && Number.isSafeInteger(call.input_length) && call.input_length >= 0
  && (call.input === undefined || (typeof call.input === "string" && call.input.length === call.input_length)));

/**
 * The resume context for one exchange. `yieldedPlan` is the managed host's
 * record of the yielded plan (host-internal bridge field `yielded_plan`); when
 * present, the plan whose tickets are read must make exactly those calls.
 * Without it (an older host) only the call count is checked.
 */
export function createResumeContext(outcomes: unknown[], events: unknown[], requestId?: number, yieldedPlan?: unknown): ResumeContext {
  // When this heap yielded the plan being resumed, tickets must come from it
  // or from a plan rebuilt with `resume.plan()` that makes the same calls.
  // Hosts that run every exchange in a fresh engine keep no such record in
  // the heap; they echo the yielded plan instead (`yieldedPlan`). Either way
  // the first ticket read binds the resume to its plan.
  if (yieldedPlan !== undefined && yieldedPlan !== null && !validYieldedPlan(yieldedPlan)) throw new Error("malformed yielded plan record from the managed host");
  const hostRecord = yieldedPlan == null ? undefined : yieldedPlan;
  const expected = lastYielded;
  lastYielded = undefined;
  const rebuilt = new WeakSet<Effects>();
  let bound: Effects | undefined;
  const read = (index: number): AxiomValue => {
    if (!Number.isSafeInteger(index) || index < 0 || index >= outcomes.length) throw new Error("host outcome is unavailable");
    const outcome = outcomes[index] as {index?: number; result?: {Ok?: AxiomValue; Err?: unknown}};
    if (outcome && outcome.index === index && outcome.result && "Err" in outcome.result && !("Ok" in outcome.result)) {
      // H5 (w4-h): the host's error keeps its kind (`Exhausted`, `Denied`,
      // ...) through a 1.0 resume, as the Rust SDK's `?` and the 1.1 async
      // runtime do; the runners report it with that code.
      const error = outcome.result.Err as {code?: unknown; message?: unknown; retryable?: unknown} | null;
      if (error && typeof error === "object" && typeof error.code === "string" && error.code !== "")
        throw new HostError(error.code, `host operation denied (${error.code}): ${typeof error.message === "string" ? error.message : ""}`, error.retryable === true);
    }
    if (!outcome || outcome.index !== index || !outcome.result || !("Ok" in outcome.result) || "Err" in outcome.result) throw new Error("host operation denied");
    return outcome.result.Ok!;
  };
  return {outcomes, events, requestId,
    get<O>(ticket: Ticket<O>): O {
      const issued = ticket !== null && typeof ticket === "object" ? tickets.get(ticket) : undefined;
      if (!issued) throw new Error("expected an effect ticket issued by Effects.call");
      if (bound !== undefined && issued.plan !== bound) throw new Error("effect tickets from different effect plans were read in one resume");
      const state = planEffects.get(issued.plan)!;
      // A plan yielded by an earlier invocation in this heap never answers this one.
      if (state.yieldedIn !== undefined && state.yieldedIn !== invocationGeneration) throw new Error("effect ticket belongs to another effect plan or yield step");
      if (bound === undefined) {
        if (expected && issued.plan !== expected) {
          if (!rebuilt.has(issued.plan)) throw new Error("effect ticket belongs to another effect plan or yield step");
          const difference = differsFromPlan(state.effects, planEffects.get(expected)!.effects);
          if (difference) throw new Error(`rebuilt effect plan differs from the yielded plan: ${difference}`);
        }
        if (hostRecord) {
          const difference = differsFromYielded(state.effects, hostRecord);
          if (difference) throw new Error(`rebuilt effect plan differs from the yielded plan: ${difference}`);
        }
      }
      bound = issued.plan;
      state.sealed = true;
      if (state.effects.length !== outcomes.length) throw new Error(`effect ticket belongs to a plan of ${state.effects.length} call(s), but this resume carries ${outcomes.length} outcome(s)`);
      const value = read(issued.index);
      return (issued.decode ? issued.decode(value) : decodeValue(value)) as O;
    },
    plan() { const plan = new Effects(); rebuilt.add(plan); return plan; },
    outcome(index) { return decodeValue(read(index)); },
    complete(output = null) {return {kind:"completed",result:{output:encodeValue(output),patches:[],transactions:[],emitted_events:[]}};},
  };
}

export function defineExtension(definition: ExtensionDefinition): ExtensionDefinition {
  return definition;
}

export class StateSnapshot {
  constructor(readonly snapshot: Snapshot) {}

  get<T>(path: Selector<T>): T | undefined {
    let current: unknown = decodeValue(this.snapshot.value);
    for (const segment of path.split(".")) {
      if (current === null || typeof current !== "object") return undefined;
      current = (current as Record<string, unknown>)[segment];
    }
    return current as T;
  }
}

export class PatchBuilder {
  // Assigned in the constructor in the order the class fields had (resource,
  // revision, operations): the same own properties, without first defining
  // each one as undefined.
  private declare readonly resource: string;
  private declare readonly revision: number;
  private declare readonly operations: PatchOperation[];

  constructor(resource: string, revision: number) {
    this.resource = resource;
    this.revision = revision;
    this.operations = [];
  }

  set<T>(path: Selector<T>, value: T): this {
    this.operations.push({ Set: { path: splitPath(path), value: encodeValue(value) } });
    return this;
  }

  unset(path: Selector): this {
    this.operations.push({ Unset: { path: splitPath(path) } });
    return this;
  }

  increment(path: Selector<number>, amount: number): this {
    this.operations.push({ Increment: { path: splitPath(path), amount } });
    return this;
  }

  build(): Patch {
    return {
      resource: this.resource,
      expected_revision: this.revision,
      operations: this.operations.slice(),
    };
  }
}

export function createContext(invocation: Invocation): ExtensionContext {
  // A new invocation: a plan yielded by an earlier, abandoned request must not
  // be taken for the plan this request's resume answers.
  lastYielded = undefined;
  invocationGeneration += 1;
  const snapshots = new Map(invocation.snapshots.map((item) => [item.resource, item]));
  return {
    invocation,
    input: decodeValue(invocation.input),
    secret(alias) {const snapshot=snapshots.get(`secret:${alias}`);return checkedSecret(snapshot&&decodeValue(snapshot.value));},
    authenticated_request(namespace,operation,path,body,secret) {return authenticatedRequest({namespace,operation},path,body,secret);},
    snapshot(resource) {
      const snapshot = snapshots.get(resource);
      if (!snapshot) throw new Error(`authorized snapshot ${resource} was not supplied`);
      return new StateSnapshot(snapshot);
    },
    patch(resource) {
      const snapshot = snapshots.get(resource);
      if (!snapshot) throw new Error(`cannot patch absent snapshot ${resource}`);
      return new PatchBuilder(resource, snapshot.revision);
    },
    effects(...effects: Array<Effect | Effects>): Yielded {
      if (effects.length === 1 && effects[0] instanceof Effects) return yieldFor(effects[0]);
      if (effects.some((effect) => effect instanceof Effects)) throw new Error("yield one Effects plan, or raw effects");
      lastYielded = undefined;
      return { kind: "yielded", plan: { effects: effects as Effect[] } };
    },
    plan() { return new Effects(); },
    complete(output = null, changes = {}) {
      return {
        kind: "completed",
        result: {
          output: encodeValue(output),
          patches: changes.patches ?? [],
          transactions: changes.transactions ?? [],
          emitted_events: changes.emitted_events ?? [],
        },
      };
    },
  };
}

export function encodeValue(value: unknown): AxiomValue {
  if (value === null || value === undefined) return "Null";
  // `typeof` once, most frequent first (the primitive cases are disjoint).
  const type = typeof value;
  if (type === "string") return { String: value as string };
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value)) throw new Error("Axiom numbers must be safe integers");
    return value >= 0 ? { Unsigned: value } : { Signed: value };
  }
  if (type === "boolean") return { Bool: value as boolean };
  if (typeof value === "bigint") {
    if (value < -(2n ** 63n) || value > 2n ** 64n - 1n) throw new Error("Axiom integers must fit in 64 bits");
    const safe = value >= BigInt(Number.MIN_SAFE_INTEGER) && value <= BigInt(Number.MAX_SAFE_INTEGER);
    const exact = safe ? Number(value) : value.toString();
    return value >= 0n ? { Unsigned: exact } : { Signed: exact };
  }
  if (value instanceof Uint8Array) return { Bytes: Array.from(value) };
  if (Array.isArray(value)) return { List: value.map(encodeValue) };
  if (typeof value === "object") {
    return {
      // Entries are [key, value] arrays: indexing them is their destructuring
      // without the iterator protocol (cheaper in QuickJS).
      Record: Object.entries(value as Record<string, unknown>)
        .sort((left, right) => (left[0] < right[0] ? -1 : left[0] > right[0] ? 1 : 0))
        .map((entry) => ({ name: entry[0], value: encodeValue(entry[1]) })),
    };
  }
  throw new Error(`unsupported Axiom value: ${typeof value}`);
}

function exactInteger(value: number | string): number | bigint {
  return typeof value === "string" ? BigInt(value) : value;
}

export function decodeValue(value: AxiomValue): unknown {
  if (value === "Null") return null;
  if ("Bool" in value) return value.Bool;
  if ("Signed" in value) return exactInteger(value.Signed);
  if ("Unsigned" in value) return exactInteger(value.Unsigned);
  if ("String" in value) return value.String;
  if ("Bytes" in value) return new Uint8Array(value.Bytes);
  if ("List" in value) return value.List.map(decodeValue);
  if ("Record" in value) {
    return Object.fromEntries(value.Record.map((field) => [field.name, decodeValue(field.value)]));
  }
  if ("Variant" in value) {
    return { case: value.Variant.case, value: value.Variant.value && decodeValue(value.Variant.value) };
  }
  return value.Handle;
}

// A whole static path: segments of [A-Za-z_][A-Za-z0-9_]* joined by ".".
const STATIC_PATH = /^[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*$/;

function splitPath(path: string): string[] {
  const result = path.split(".");
  // One test of the whole string instead of one per segment; the same paths
  // pass (a string path; anything else keeps the per-segment test).
  if (typeof path === "string" ? !STATIC_PATH.test(path) : result.some((segment) => !/^[A-Za-z_][A-Za-z0-9_]*$/.test(segment))) {
    throw new Error(`invalid static Axiom path: ${path}`);
  }
  return result;
}
