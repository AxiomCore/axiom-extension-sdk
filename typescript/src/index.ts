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
  effects(...effects: Effect[]): Yielded;
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
  outcome(index: number): unknown;
  complete(output?: unknown): Completed;
}
export function createResumeContext(outcomes: unknown[], events: unknown[], requestId?: number): ResumeContext {
  return {outcomes, events, requestId,
    outcome(index) {
      if (!Number.isSafeInteger(index) || index < 0 || index >= outcomes.length) throw new Error("host outcome is unavailable");
      const outcome = outcomes[index] as {index?: number; result?: {Ok?: AxiomValue; Err?: unknown}};
      if (!outcome || outcome.index !== index || !outcome.result || !("Ok" in outcome.result) || "Err" in outcome.result) throw new Error("host operation denied");
      return decodeValue(outcome.result.Ok!);
    },
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
    effects(...effects) {
      return { kind: "yielded", plan: { effects } };
    },
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
