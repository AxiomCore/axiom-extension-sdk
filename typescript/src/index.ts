export type AxiomValue =
  | "Null"
  | { Bool: boolean }
  | { Signed: number }
  | { Unsigned: number }
  | { String: string }
  | { Bytes: number[] }
  | { List: AxiomValue[] }
  | { Record: Array<{ name: string; value: AxiomValue }> }
  | { Variant: { case: string; value: AxiomValue | null } }
  | { Handle: { id: number; generation: number; kind: string } };

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
  snapshot(resource: string): StateSnapshot;
  patch(resource: string): PatchBuilder;
  effects(...effects: Effect[]): Yielded;
  complete(output?: unknown, changes?: Partial<InvocationResult>): Completed;
}

export type ExtensionHandler = (context: ExtensionContext) => ExtensionResponse;
export interface ExtensionDefinition {
  exports: Record<string, ExtensionHandler>;
  resume?: (context: {
    outcomes: unknown[];
    events: unknown[];
  }) => ExtensionResponse;
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
    snapshot(resource) {
      const snapshot = snapshots.get(resource);
      if (!snapshot) throw new Error(`authorized snapshot ${resource} was not supplied`);
      return new StateSnapshot(snapshot);
    },
    patch(resource) {
      const snapshot = snapshots.get(resource);
      if (!snapshot) throw new Error(`cannot patch absent snapshot ${resource}`);
      return new PatchBuilder(resource.replace(/^ui:/, ""), snapshot.revision);
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

export function decodeValue(value: AxiomValue): unknown {
  if (value === "Null") return null;
  if ("Bool" in value) return value.Bool;
  if ("Signed" in value) return value.Signed;
  if ("Unsigned" in value) return value.Unsigned;
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
