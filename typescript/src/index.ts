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
  /** A new effect plan; `plan.call(effect)` returns a typed {@link Ticket}. */
  plan(): Effects;
  complete(output?: unknown, changes?: Partial<InvocationResult>): Completed;
}

export type ExtensionHandler = (context: ExtensionContext) => ExtensionResponse;
export interface ExtensionDefinition {
  exports: Record<string, ExtensionHandler>;
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
   * exchange (managed hosts run every exchange in a fresh engine). When this
   * heap still knows the yielded plan, the rebuilt plan must call the same
   * operations in the same order.
   */
  plan(): Effects;
  /**
   * Positional outcome access.
   * @deprecated Use typed tickets: `const t = plan.call(op.call(input))`, then `resume.get(t)`.
   */
  outcome(index: number): unknown;
  complete(output?: unknown): Completed;
}

declare const ticketBrand: unique symbol;
/**
 * A typed claim on the outcome of one host call in one effect plan, returned
 * by {@link Effects.call}. Only the SDK can issue one; `index` is the ABI
 * outcome index (declaration order). `O` is invariant.
 */
export interface Ticket<O> {
  readonly index: number;
  readonly [ticketBrand]: (value: O) => O;
}

declare const outputBrand: unique symbol;
/** An effect whose outcome decodes as `O`. A plain {@link Effect} is a `TypedEffect<unknown>`. */
export type TypedEffect<O> = Effect & { readonly [outputBrand]?: (value: O) => O };

/** A typed host operation: `op.call(input)` is a {@link TypedEffect} for {@link Effects.call}. */
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
const planEffects = new WeakMap<Effects, { effects: Effect[]; sealed: boolean }>();
/** The plan most recently yielded in this JavaScript heap, if any. */
let lastYielded: Effects | undefined;

class IssuedTicket {
  constructor(readonly index: number) { Object.freeze(this); }
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
 * An effect plan built call by call. `plan.call(effect)` returns a typed
 * {@link Ticket}; yield the plan with `context.effects(plan)` or
 * {@link yieldFor}. The yielded bytes equal `context.effects(...effects)`.
 */
export class Effects {
  constructor() { planEffects.set(this, { effects: [], sealed: false }); }

  call<O>(effect: TypedEffect<O>): Ticket<O> { return this.push(effect); }

  /**
   * Same as {@link Effects.call}. Python sources use `plan.push(effect)`: the
   * AOT host bridge never exposes a member named `call`.
   */
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
  lastYielded = plan;
  return { kind: "yielded", plan: { effects: [...state.effects] } };
}

const shape = (plan: Effects): string => JSON.stringify(planEffects.get(plan)!.effects.map((effect) => [effect.namespace, effect.operation]));

export function createResumeContext(outcomes: unknown[], events: unknown[], requestId?: number): ResumeContext {
  // When this heap yielded the plan being resumed, tickets must come from it
  // or from a plan rebuilt with `resume.plan()` that calls the same operations.
  // Hosts that run every exchange in a fresh engine keep no such record; then
  // the first ticket read binds the resume to its plan.
  const expected = lastYielded;
  lastYielded = undefined;
  const rebuilt = new WeakSet<Effects>();
  let bound: Effects | undefined;
  const read = (index: number): AxiomValue => {
    if (!Number.isSafeInteger(index) || index < 0 || index >= outcomes.length) throw new Error("host outcome is unavailable");
    const outcome = outcomes[index] as {index?: number; result?: {Ok?: AxiomValue; Err?: unknown}};
    if (!outcome || outcome.index !== index || !outcome.result || !("Ok" in outcome.result) || "Err" in outcome.result) throw new Error("host operation denied");
    return outcome.result.Ok!;
  };
  return {outcomes, events, requestId,
    get<O>(ticket: Ticket<O>): O {
      const issued = ticket !== null && typeof ticket === "object" ? tickets.get(ticket) : undefined;
      if (!issued) throw new Error("expected an effect ticket issued by Effects.call");
      if (bound !== undefined && issued.plan !== bound) throw new Error("effect tickets from different effect plans were read in one resume");
      if (expected && issued.plan !== expected) {
        if (!rebuilt.has(issued.plan)) throw new Error("effect ticket belongs to another effect plan or yield step");
        if (shape(issued.plan) !== shape(expected)) throw new Error("rebuilt effect plan calls different operations than the yielded plan");
      }
      bound = issued.plan;
      const state = planEffects.get(issued.plan)!;
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
  private readonly operations: PatchOperation[] = [];

  constructor(
    private readonly resource: string,
    private readonly revision: number,
  ) {}

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
      operations: [...this.operations],
    };
  }
}

export function createContext(invocation: Invocation): ExtensionContext {
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
  if (typeof value === "boolean") return { Bool: value };
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value)) throw new Error("Axiom numbers must be safe integers");
    return value >= 0 ? { Unsigned: value } : { Signed: value };
  }
  if (typeof value === "bigint") {
    if (value < -(2n ** 63n) || value > 2n ** 64n - 1n) throw new Error("Axiom integers must fit in 64 bits");
    const safe = value >= BigInt(Number.MIN_SAFE_INTEGER) && value <= BigInt(Number.MAX_SAFE_INTEGER);
    const exact = safe ? Number(value) : value.toString();
    return value >= 0n ? { Unsigned: exact } : { Signed: exact };
  }
  if (typeof value === "string") return { String: value };
  if (value instanceof Uint8Array) return { Bytes: Array.from(value) };
  if (Array.isArray(value)) return { List: value.map(encodeValue) };
  if (typeof value === "object") {
    return {
      Record: Object.entries(value as Record<string, unknown>)
        .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
        .map(([name, item]) => ({ name, value: encodeValue(item) })),
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

function splitPath(path: string): string[] {
  const result = path.split(".");
  if (result.some((segment) => !/^[A-Za-z_][A-Za-z0-9_]*$/.test(segment))) {
    throw new Error(`invalid static Axiom path: ${path}`);
  }
  return result;
}
