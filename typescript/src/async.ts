// ABI 1.1 async exports for managed (QuickJS/Javy) guests: rounds and replay.
//
// Observable behaviour matches the Rust async SDK (contract x8c-rust-async.md):
// every round runs the export from the top in a fresh engine; host operations
// register when `cx.call` is called; when the job queue drains with the export
// still pending, the operations registered since the previous stall form one
// round plan (one -> `Plan`, 2..32 -> a `Collect` group with branches `b0..`
// and `limit = min(n, 8)`, more -> `Exhausted`). Stalls already on the tape
// are answered from it in order; the re-derived plans are reported back so
// the managed kernel hashes them and compares them with the tape.
//
// How the job queue's drain is detected (see contract x8d-managed-async.md):
// the engine's `_start` export runs the bundle and then drains every pending
// job. The managed kernel then calls the async build's step export
// (`axiom_managed_step`) on the same instance, which evaluates only
// `globalThis.__axiom_managed_step__()`: the step hook installed below. Each
// step therefore begins exactly when the queue is empty. No timers are
// involved. (Older builds re-entered `_start` behind an esbuild banner.)
//
// Plan order (w2-c2 M-1): the operations of one round are ordered by their
// position in the tree of promise creations, not by when they registered.
// The async build lowers `async`/`await` to generators (esbuild
// `--supported:async-await=false`), so every await continues through
// `Promise.prototype.then`; during a round `Promise` is a subclass whose
// constructor opens a child node of the current node (every async function
// call creates one), and `then` runs its callbacks in the node that was
// current when `then` was called. An operation's key is the path of the node
// that called `cx.call` plus its sequence in that node. Sorting a round's
// operations by key gives a depth-first walk of the async call tree in
// creation order: the order in which the Rust executor's depth-first poll of
// `join!` registers them, whatever the number of microtask hops (awaits of
// pure helpers, resolved promises) in between.
import type { AxiomValue, Completed, Effect, ExtensionHandler, Invocation, InvocationResult, SecretHandle, TypedEffect } from "./index.js";
import { PatchBuilder, StateSnapshot, canonicalValue, createContext, decodeValue, effectDecoder, encodeValue } from "./index.js";
import { HostOperationError } from "./errors.js";
export { HostOperationError } from "./errors.js";

/** Group format of an ABI 1.1 `Collect` round group (`abi::EFFECT_GROUP_FORMAT`). */
export const EFFECT_GROUP_FORMAT = "axiom-extension-effect-group/v1";
/** At most this many host operations may be pending together in one round. */
export const MAX_GROUP_BRANCHES = 32;
/** The `limit` of a round group is `min(n, MAX_GROUP_LIMIT)`. */
export const MAX_GROUP_LIMIT = 8;
/** Name of the step hook the async build's step export calls. */
export const STEP_HOOK = "__axiom_managed_step__";

/**
 * True when this bundle was built with `async`/`await` lowered to generators
 * (the async build's esbuild step). Only then is the plan order independent
 * of microtask hops; the async runner refuses async exports otherwise.
 */
export const LOWERED_ASYNC: boolean = Object.getPrototypeOf(async function () {}) === Function.prototype;

// ------------------------------------------------------------ plan order
// (w2-c3) Registration follows the Rust executor's first-poll rule.
//  - A node of the async call tree is the promise of one async function call
//    (esbuild's `__async` helper) or any other promise made during a round.
//    Its code runs in that node, but the node gets its place in the tree only
//    when the promise is first awaited, combined or returned (it is "touched"):
//    that is the moment the Rust future would be polled first. Until then its
//    operations wait ("parked") and join the round in which the node is
//    reached from the root.
//  - `cx.call` creates a lazy operation: it registers when first touched
//    (awaited, `then`, combined by a touched `Promise.all`/`gather`, or
//    returned from an async function), at the next sequence number of the
//    touching node.
//  - `Promise.all` / `allSettled` / `race` / `any` during a round are lazy
//    combinators: touching one touches its children in argument order (the
//    Rust `join!` poll order), so nesting order never depends on argument
//    evaluation order.
//  - A fail-fast `Promise.all` that rejects abandons its children at the next
//    stall (Rust `try_join!` drops them): operations they registered stay
//    registered (never withdrawn); operations they would start later are never
//    registered.
interface OrderNode {
  /** Set when the node is attached (touched); the root has none. */
  parent: OrderNode | undefined;
  /** Position among the parent's children and operations. */
  seq: number;
  attached: boolean;
  abandoned: boolean;
  readonly root: boolean;
  /** Next sequence number for children and operations touched in this node. */
  next: number;
  /** `touchClock` at the latest touch of this node's promise. */
  lastTouch: number;
}
const NativePromise = Promise;
const nativeThen = NativePromise.prototype.then;
/** The node whose code is running; undefined outside an async round. */
let currentNode: OrderNode | undefined;
/** The run of the current round (one per engine in production). */
let activeRun: RoundRun | undefined;

const newNode = (root = false): OrderNode => ({ parent: undefined, seq: 0, attached: root, abandoned: false, root, next: 0, lastTouch: 0 });
/** Counts touches of nodes, to tell a re-await after a rejection. */
let touchClock = 0;
/** The node of each promise made during a round. */
const nodeOf = new WeakMap<object, OrderNode>();
/** Lazy values (operations, combinators) and what touching them does. */
interface Lazy { touched: boolean; touch(at: OrderNode | undefined): void; children?: unknown[] }
const lazyOf = new WeakMap<object, Lazy>();

type Reach = "reachable" | "parked" | "abandoned";
function reach(node: OrderNode): Reach {
  for (let n: OrderNode | undefined = node; n !== undefined; n = n.parent) {
    if (n.abandoned) return "abandoned";
    if (n.root) return "reachable";
  }
  return "parked";
}

function pathOf(node: OrderNode): number[] {
  const path: number[] = [];
  for (let n: OrderNode | undefined = node; n !== undefined && !n.root; n = n.parent) path.push(n.seq);
  return path.reverse();
}

/** First touch of `value` at node `at` (the first poll of a Rust future). */
function touch(value: unknown, at: OrderNode | undefined): void {
  if (value === null || (typeof value !== "object" && typeof value !== "function")) return;
  const lazy = lazyOf.get(value);
  if (lazy !== undefined) {
    if (!lazy.touched) {
      lazy.touched = true;
      lazy.touch(at);
    }
    return;
  }
  const node = nodeOf.get(value);
  if (node === undefined || at === undefined) return;
  node.lastTouch = ++touchClock;
  if (node.attached) {
    // Awaiting a branch a fail-fast combinator dropped: Rust cannot (the
    // branch was moved into `try_join!`); refused at the stall.
    if (node.abandoned && reach(at) !== "abandoned" && activeRun) activeRun.reusedAbandoned = true;
    return;
  }
  // Never attach a node below itself (a promise cycle; it deadlocks anyway).
  for (let n: OrderNode | undefined = at; n !== undefined; n = n.parent) if (n === node) return;
  node.attached = true;
  node.parent = at;
  node.seq = at.next++;
}

function inNode<A extends unknown[], R>(node: OrderNode | undefined, fn: ((...args: A) => R) | null | undefined): ((...args: A) => R) | null | undefined {
  if (typeof fn !== "function" || node === undefined) return fn;
  return (...args: A): R => {
    const saved = currentNode;
    currentNode = node;
    try {
      return fn(...args);
    } finally {
      currentNode = saved;
    }
  };
}

/** Subscribe to `item` without touching it (combinator wiring). */
function observe(item: unknown, onFulfilled: (value: unknown) => void, onRejected: (reason: unknown) => void): void {
  if (item instanceof NativePromise) {
    nativeThen.call(item, onFulfilled, onRejected);
  } else {
    nativeThen.call(NativePromise.resolve(item), onFulfilled, onRejected);
  }
}

type CombinatorKind = "all" | "allSettled" | "race" | "any";
declare const AggregateError: new (errors: unknown[], message?: string) => Error;

/** A lazy `Promise.all` / `allSettled` / `race` / `any` made during a round. */
function lazyCombinator(kind: CombinatorKind, iterable: Iterable<unknown>): Promise<unknown> {
  const items = Array.from(iterable);
  const run = activeRun;
  let resolve!: (value: unknown) => void;
  let reject!: (reason: unknown) => void;
  const out = new NativePromise<unknown>((res, rej) => { resolve = res; reject = rej; });
  lazyOf.set(out, { touched: false, children: items, touch: (at) => { for (const item of items) touch(item, at); } });
  const values: unknown[] = new Array(items.length);
  let left = items.length;
  let done = false;
  const finish = (ok: boolean, value: unknown) => {
    if (done) return;
    done = true;
    if (ok) resolve(value);
    else reject(value);
  };
  if (left === 0) {
    if (kind === "any") finish(false, new AggregateError([], "All promises were rejected"));
    else if (kind !== "race") finish(true, []);
    return out;
  }
  items.forEach((item, index) => {
    observe(
      item,
      (value) => {
        if (kind === "race" || kind === "any") return finish(true, value);
        values[index] = kind === "allSettled" ? { status: "fulfilled", value } : value;
        if (--left === 0) finish(true, values);
      },
      (reason) => {
        if (kind === "race") return finish(false, reason);
        if (kind === "all") {
          if (!done && run) run.abandonAtStall(items, touchClock);
          return finish(false, reason);
        }
        values[index] = kind === "allSettled" ? { status: "rejected", reason } : reason;
        if (--left === 0) finish(kind === "allSettled", kind === "any" ? new AggregateError(values, "All promises were rejected") : values);
      },
    );
  });
  return out;
}

/** True when one of `items` was awaited again after the combinator rejected at `clock`. */
function retouched(items: unknown[], clock: number): boolean {
  return items.some((item) => {
    if (item === null || (typeof item !== "object" && typeof item !== "function")) return false;
    const node = nodeOf.get(item);
    if (node !== undefined && node.lastTouch > clock) return true;
    const children = lazyOf.get(item)?.children;
    return children !== undefined && retouched(children, clock);
  });
}

/** Mark the subtrees of `items` abandoned (a fail-fast combinator dropped them). */
function abandon(items: unknown[]): void {
  for (const item of items) {
    if (item === null || (typeof item !== "object" && typeof item !== "function")) continue;
    const node = nodeOf.get(item);
    if (node !== undefined) node.abandoned = true;
    const children = lazyOf.get(item)?.children;
    if (children !== undefined) abandon(children);
  }
}

let orderingInstalled = false;
/**
 * Install the plan-order hooks (once per engine; every round is a fresh
 * engine in production). Outside a round (`currentNode` undefined) every hook
 * behaves exactly like the native one.
 *
 * Realm freeze (X9a w2-d4 proposal, applied by w2-i): the async build's realm
 * prelude calls this (as `installOrderingHooks`) *before* it freezes `Promise`,
 * `Promise.prototype` and the other intrinsics and before any guest code
 * runs; `RoundRun.start` then finds the hooks installed and redefines nothing
 * (a redefinition would fail on the frozen intrinsics).
 */
export function installOrderingHooks(): void {
  installOrdering();
}

function installOrdering(): void {
  if (orderingInstalled) return;
  orderingInstalled = true;
  class OrderedPromise<T> extends NativePromise<T> {
    constructor(executor: (resolve: (value: T | PromiseLike<T>) => void, reject: (reason?: unknown) => void) => void) {
      if (currentNode === undefined || typeof executor !== "function") {
        super(executor);
        return;
      }
      const node = newNode();
      super((resolve, reject) => {
        const saved = currentNode;
        currentNode = node;
        try {
          // Resolving with a lazy value or another promise touches it here
          // (an async function returning `cx.call(...)` polls it).
          executor((value) => { touch(value, node); resolve(value); }, reject);
        } finally {
          currentNode = saved;
        }
      });
      nodeOf.set(this, node);
    }
    static resolve(value?: unknown): Promise<any> {
      // Every lowered `await` goes through `Promise.resolve(value).then(...)`.
      if (currentNode !== undefined) touch(value, currentNode);
      return super.resolve(value);
    }
    static all(iterable: Iterable<unknown>): Promise<any> {
      return currentNode === undefined ? super.all(iterable) : lazyCombinator("all", iterable);
    }
    static allSettled(iterable: Iterable<unknown>): Promise<any> {
      return currentNode === undefined ? super.allSettled(iterable) : lazyCombinator("allSettled", iterable);
    }
    static race(iterable: Iterable<unknown>): Promise<any> {
      return currentNode === undefined ? super.race(iterable) : lazyCombinator("race", iterable);
    }
    static any(iterable: Iterable<unknown>): Promise<any> {
      return currentNode === undefined ? (NativePromise as any).any.call(this, iterable) : lazyCombinator("any", iterable);
    }
  }
  Object.defineProperty(NativePromise.prototype, "then", {
    value: function then(this: Promise<unknown>, onFulfilled?: unknown, onRejected?: unknown) {
      const node = currentNode;
      touch(this, node);
      return nativeThen.call(this, inNode(node, onFulfilled as any) as any, inNode(node, onRejected as any) as any);
    },
    writable: true,
    enumerable: false,
    configurable: true,
  });
  Object.defineProperty(globalThis, "Promise", { value: OrderedPromise, writable: true, enumerable: false, configurable: true });
}

/** Lexicographic order of two operation keys (a key is never a prefix of another). */
function compareKeys(a: readonly number[], b: readonly number[]): number {
  const length = Math.min(a.length, b.length);
  for (let i = 0; i < length; i++) if (a[i] !== b[i]) return a[i] - b[i];
  return a.length - b.length;
}

/**
 * The canonical text of a round plan. It must stay identical to
 * `canonical_round_plan` in the TypeScript kernel (`rounds.rs`): the kernel
 * passes the text of each tape round's plan (`tape_plans`, bounded) so a
 * re-derived plan that differs fails `Protocol` before any outcome is used.
 */
export function canonicalRoundPlan(plan: RoundPlanJson): string {
  const call = (effect: Effect, name?: string) => `[${name === undefined ? "" : JSON.stringify(name) + ","}${JSON.stringify(effect.namespace)},${JSON.stringify(effect.operation)},${canonicalValue(effect.input)}]`;
  if ("Plan" in plan) return `{"Plan":[${plan.Plan.effects.map((effect) => call(effect)).join(",")}]}`;
  const group = plan.Group;
  return `{"Group":{"format":${JSON.stringify(group.format)},"limit":${group.limit},"mode":${JSON.stringify(group.mode)},"branches":[${group.branches.map((branch) => call(branch.effect, branch.name)).join(",")}]}}`;
}

/** The context of one async export run. */
export interface AsyncContext {
  readonly invocation: Invocation;
  readonly input: unknown;
  /** Current invoke number (1-based). It must not influence which operations are called. */
  round(): number;
  /** The yield budget (`maxRounds`) of this invocation. */
  maxRounds(): number;
  /**
   * Start a host operation. It registers in the current round at once; the
   * promise resolves with the operation's typed output when the host has run
   * it, or rejects with a {@link HostOperationError}.
   */
  call<O>(effect: TypedEffect<O>): Promise<O>;
  snapshot(resource: string): StateSnapshot;
  patch(resource: string): PatchBuilder;
  secret(alias: string): SecretHandle;
  /** The export's result: output plus the patches, transactions and events to commit. */
  complete(output?: unknown, changes?: Partial<InvocationResult>): Completed;
}

export type AsyncHandler<I = unknown> = (cx: AsyncContext, input: I) => unknown;

declare const asyncExportBrand: unique symbol;
/**
 * An async export, made with {@link asyncExport} (or `extension.export` on an
 * `async` function). It is callable only through an ABI 1.1 managed build;
 * the 1.0 bridge refuses it with a clear error.
 */
export type AsyncExport = ExtensionHandler & { readonly [asyncExportBrand]: true };

const asyncHandlers = new WeakMap<object, AsyncHandler<any>>();

/** Mark `handler` as an async export (ABI 1.1 rounds). */
export function asyncExport<I = unknown>(handler: AsyncHandler<I>): AsyncExport {
  if (typeof handler !== "function") throw new TypeError("asyncExport expects a function");
  const refuse = (): never => {
    throw new Error("async export needs an ABI 1.1 managed build (the async build adds the step export and the async-exports section); this module was invoked through the ABI 1.0 bridge");
  };
  asyncHandlers.set(refuse, handler);
  return refuse as unknown as AsyncExport;
}

/** The async handler behind an export, if it is one. */
export function asyncHandlerOf(value: unknown): AsyncHandler | undefined {
  return typeof value === "function" ? asyncHandlers.get(value) : undefined;
}

const asyncFunctionPrototype = Object.getPrototypeOf(async function () {});

/**
 * `extension.export(handler)`: an `async` function becomes an async export
 * (rounds), any other function stays a 1.0 handler. Use {@link asyncExport}
 * for a non-`async` function that returns a promise.
 */
export const extension = Object.freeze({
  export<H extends (...args: any[]) => unknown>(handler: H): H extends (cx: AsyncContext, input: any) => Promise<unknown> ? AsyncExport : H {
    return (Object.getPrototypeOf(handler) === asyncFunctionPrototype ? asyncExport(handler as AsyncHandler) : handler) as any;
  },
});

/** One record of the host-owned tape as the managed kernel passes it to the runner. */
export interface TapeRecord {
  outcomes: Array<{ index: number; result: { Ok?: AxiomValue; Err?: { code: string; message: string; retryable: boolean } } }>;
  events: unknown[];
}

/** The `invokeRound` bridge request (host-internal JSON, never the extension ABI). */
export interface RoundRequest {
  kind: "invokeRound";
  request_id: number;
  invocation: Invocation;
  round: number;
  max_rounds: number;
  /** "1.1": rounds; "1.0": the host negotiated ABI 1.0, which allows one yield. */
  abi: "1.1" | "1.0";
  tape: TapeRecord[];
  yielded_plan?: unknown;
  /** The canonical text of each tape round's plan, or null above the kernel's bound. */
  tape_plans?: Array<string | null>;
}

type RoundPlanJson =
  | { Plan: { effects: Effect[] } }
  | { Group: { format: string; limit: number; mode: "Collect"; branches: Array<{ name: string; effect: Effect }> } };

export type RoundResponse =
  | { kind: "completedRound"; result: InvocationResult; replayed?: RoundPlanJson[] }
  | { kind: "yieldedRound"; plan: RoundPlanJson; replayed?: RoundPlanJson[] }
  | { kind: "failed"; error: { code: string; message: string; retryable: boolean }; replayed?: RoundPlanJson[] }
  | { kind: "cancelled" };

/** The plan for the operations of one segment, in registration order. */
export function segmentPlan(effects: Effect[]): RoundPlanJson {
  if (effects.length === 1) return { Plan: { effects } };
  return {
    Group: {
      format: EFFECT_GROUP_FORMAT,
      limit: Math.min(effects.length, MAX_GROUP_LIMIT),
      mode: "Collect",
      branches: effects.map((effect, index) => ({ name: `b${index}`, effect })),
    },
  };
}

interface Registered {
  effect: Effect;
  /** The node that touched the operation, and its sequence number there. */
  owner: OrderNode;
  seq: number;
  /** Position in the async call tree, computed at the stall that plans it. */
  key: readonly number[];
  resolve(value: unknown): void;
  reject(error: unknown): void;
  decode?: (value: AxiomValue) => unknown;
}

const failure = (code: string, message: string, retryable = false) => ({ code, message, retryable });

function guestError(error: unknown): { code: string; message: string; retryable: boolean } {
  if (error instanceof HostOperationError) return failure(error.code, error.message, error.retryable);
  return failure("Guest", error instanceof Error ? error.message : "managed-language extension failed");
}

function plainEffect(effect: unknown): Effect {
  if (effect === null || typeof effect !== "object") throw new TypeError("cx.call expects an effect (namespace, operation, input)");
  const { namespace, operation, input } = effect as Effect;
  if (typeof namespace !== "string" || typeof operation !== "string" || input === undefined) throw new TypeError("cx.call expects an effect (namespace, operation, input)");
  return { namespace, operation, input };
}

/** One run of an async export in this engine (one round). */
class RoundRun {
  /** Touched operations not planned yet (parked ones wait for their node). */
  private pending: Registered[] = [];
  /** Children of fail-fast combinators that rejected since the last stall, with the touch clock at the rejection. */
  private toAbandon: Array<{ items: unknown[]; clock: number }> = [];
  /** Set when a branch a fail-fast combinator dropped is awaited again. */
  reusedAbandoned = false;
  private cursor = 0;
  private readonly replayed: RoundPlanJson[] = [];
  private settled: { ok: true; value: unknown } | { ok: false; error: unknown } | undefined;
  private answered = false;
  private readonly completions = new WeakSet<object>();
  /** The root of this run's async call tree (plan order). */
  private readonly root: OrderNode = newNode(true);
  /** Set when an operation started outside the async call tree (see `register`). */
  private outsideTree = false;

  constructor(private readonly request: RoundRequest, private readonly write: (response: RoundResponse) => void) {}

  context(): AsyncContext {
    const base = createContext(this.request.invocation);
    const run = this;
    return Object.freeze({
      invocation: base.invocation,
      input: base.input,
      round: () => run.request.round,
      maxRounds: () => run.request.max_rounds,
      call<O>(effect: TypedEffect<O>): Promise<O> { return run.register(effect) as Promise<O>; },
      snapshot: (resource: string) => base.snapshot(resource),
      patch: (resource: string) => base.patch(resource),
      secret: (alias: string) => base.secret(alias),
      complete(output: unknown = null, changes: Partial<InvocationResult> = {}): Completed {
        const completed = base.complete(output, changes);
        run.completions.add(completed);
        return completed;
      },
    });
  }

  start(handler: AsyncHandler, cx: AsyncContext): void {
    installOrdering();
    activeRun = this;
    const saved = currentNode;
    currentNode = this.root;
    try {
      let promise: Promise<unknown>;
      try {
        promise = Promise.resolve(handler(cx, cx.input));
      } catch (error) {
        promise = Promise.reject(error);
      }
      promise.then(
        (value) => { if (!this.settled) this.settled = { ok: true, value }; },
        (error) => { if (!this.settled) this.settled = { ok: false, error }; },
      );
    } finally {
      currentNode = saved;
    }
  }

  /** Mark the children of a rejected fail-fast combinator for the next stall. */
  abandonAtStall(items: unknown[], clock: number): void {
    this.toAbandon.push({ items, clock });
  }

  private register(effect: unknown): Promise<unknown> {
    const plain = plainEffect(effect);
    const decode = effectDecoder(effect as object);
    let resolve!: (value: unknown) => void;
    let reject!: (reason: unknown) => void;
    // The operation's own promise is native: it opens no node.
    const promise = new NativePromise<unknown>((res, rej) => { resolve = res; reject = rej; });
    const touchAt = (at: OrderNode | undefined) => {
      // Operations started after the export returned are never planned or sent.
      if (this.settled || this.answered) return;
      // In a lowered (async) build all guest code runs inside the call tree;
      // only a custom thenable's `then`, called by the engine itself, runs
      // outside it. Such an operation has no defined place in the plan, so
      // the round fails at its stall (a deterministic error, not an arbitrary
      // order).
      if (at === undefined) {
        this.outsideTree = true;
        at = this.root;
      }
      // A branch dropped by a fail-fast combinator registers nothing more.
      if (reach(at) === "abandoned") return;
      this.pending.push({ effect: plain, owner: at, seq: at.next++, key: [], resolve, reject, decode });
    };
    if (!LOWERED_ASYNC) {
      // Unlowered (node tests): native awaits bypass the hooks, so every
      // operation registers when it is created, under the root.
      touchAt(currentNode ?? this.root);
    } else {
      lazyOf.set(promise, { touched: false, touch: touchAt });
    }
    return promise;
  }

  private answer(response: RoundResponse): void {
    this.answered = true;
    this.write(response);
  }

  /** Called when the job queue has drained. */
  step(): void {
    if (this.answered) return;
    try {
      this.stepChecked();
    } catch (error) {
      this.answer({ kind: "failed", error: guestError(error), replayed: this.replayed });
    }
  }

  private stepChecked(): void {
    const tape = this.request.tape;
    const replayed = this.replayed;
    if (this.settled) {
      // Operations still pending when the export returned are never sent.
      this.pending = [];
      if (this.cursor !== tape.length) {
        this.answer({ kind: "failed", error: failure("Protocol", `non-deterministic guest: the export completed after replaying ${this.cursor} of ${tape.length} effect rounds`), replayed });
        return;
      }
      if (!this.settled.ok) {
        this.answer({ kind: "failed", error: guestError(this.settled.error), replayed });
        return;
      }
      const value = this.settled.value;
      const result = value !== null && typeof value === "object" && this.completions.has(value)
        ? (value as Completed).result
        : { output: encodeValue(value), patches: [], transactions: [], emitted_events: [] };
      this.answer({ kind: "completedRound", result, replayed });
      return;
    }
    if (this.outsideTree) {
      this.answer({ kind: "failed", error: failure("InvalidInput", "a host operation was started outside the async call tree (for example from a custom thenable's `then`, which the engine calls itself); its place in the round's plan is not defined. Start host operations from async functions or promise callbacks"), replayed });
      return;
    }
    if (this.toAbandon.some(({ items, clock }) => retouched(items, clock))) this.reusedAbandoned = true;
    if (this.reusedAbandoned) {
      this.answer({ kind: "failed", error: failure("InvalidInput", "a promise that a fail-fast Promise.all / gather dropped after a sibling failed was awaited again; its later host operations are never registered (as with Rust try_join!, which drops its children). Use Promise.allSettled / gather(..., return_exceptions=True) (Rust join!) to keep every branch"), replayed });
      return;
    }
    // The operations whose node is reached from the root, in the depth-first
    // order of the async call tree (independent of microtask hops); parked
    // ones wait for their node, abandoned ones are dropped.
    const segment: Registered[] = [];
    const parked: Registered[] = [];
    for (const registered of this.pending) {
      const state = reach(registered.owner);
      if (state === "reachable") {
        registered.key = [...pathOf(registered.owner), registered.seq];
        segment.push(registered);
      } else if (state === "parked") {
        parked.push(registered);
      }
    }
    this.pending = parked;
    segment.sort((a, b) => compareKeys(a.key, b.key));
    // Children of fail-fast combinators that rejected are dropped now: what
    // they registered before this stall stays registered.
    for (const { items } of this.toAbandon) abandon(items);
    this.toAbandon = [];
    if (segment.length === 0) {
      this.answer({ kind: "failed", error: failure("InvalidInput", "async export is waiting on something that is not a host operation; only `cx.call` promises can be awaited (no timers, channels or other executors)"), replayed });
      return;
    }
    if (segment.length > MAX_GROUP_BRANCHES) {
      this.answer({ kind: "failed", error: failure("Exhausted", `${segment.length} host operations are pending together; one round group allows at most ${MAX_GROUP_BRANCHES}`), replayed });
      return;
    }
    const plan = segmentPlan(segment.map((registered) => registered.effect));
    if (this.cursor >= tape.length) {
      if (this.request.abi === "1.0" && this.cursor >= 1) {
        this.answer({ kind: "failed", error: failure("Exhausted", "async export needs more than one effect round, but the host negotiated ABI 1.0, which allows one"), replayed });
        return;
      }
      this.answer({ kind: "yieldedRound", plan, replayed });
      return;
    }
    // Replay: the kernel hashes every re-derived plan and compares it with
    // the tape; outcomes resolve the segment's operations in index order.
    replayed.push(plan);
    // L-2: the kernel's text of the tape plan, when it sent one, catches a
    // divergence here, before this round's outcomes reach the wrong calls.
    const expected = this.request.tape_plans?.[this.cursor];
    if (typeof expected === "string" && canonicalRoundPlan(plan) !== expected) {
      this.answer({ kind: "failed", error: failure("Protocol", `non-deterministic guest: the plan re-derived for effect round ${this.cursor + 1} differs from the tape`), replayed });
      return;
    }
    const outcomes = tape[this.cursor].outcomes ?? [];
    this.cursor += 1;
    segment.forEach((registered, index) => {
      const outcome = outcomes.find((candidate) => candidate && candidate.index === index);
      const result = outcome?.result;
      if (!result) {
        registered.reject(new HostOperationError("Protocol", "the tape has no outcome for this host operation"));
      } else if (result.Err !== undefined) {
        const error = result.Err;
        registered.reject(new HostOperationError(String(error.code), String(error.message), error.retryable === true));
      } else if (result.Ok !== undefined) {
        let value: unknown;
        try {
          value = registered.decode ? registered.decode(result.Ok) : decodeValue(result.Ok);
        } catch (error) {
          registered.reject(new HostOperationError("InvalidInput", `host operation output does not decode as the operation's output type: ${error instanceof Error ? error.message : String(error)}`));
          return;
        }
        registered.resolve(value);
      } else {
        registered.reject(new HostOperationError("Protocol", "the tape has no outcome for this host operation"));
      }
    });
  }
}

/**
 * Start an async export for an `invokeRound` request. Nothing is written
 * now; the response is written by a later step (the kernel re-enters the
 * engine after each drain of the job queue).
 */
export function startAsyncRound(handler: AsyncHandler, request: RoundRequest, write: (response: RoundResponse) => void): () => void {
  const run = new RoundRun(request, write);
  const step = () => run.step();
  run.start(handler, run.context());
  return step;
}

/** Install the step hook the async build's step export calls on re-entry. */
export function installStepHook(step: () => void): void {
  Object.defineProperty(globalThis, STEP_HOOK, { value: step, writable: false, enumerable: false, configurable: true });
}

/**
 * The build probe (`probeAsyncBuild`, axiom-typescript-managed-abi finish):
 * a job installs a step hook that answers in the next engine step. Without
 * Javy's event loop the engine aborts with the job pending; without the step
 * export the answer never comes; without the async lowering it names that.
 */
export function probeAsyncBuild(write: (response: unknown) => void): void {
  NativePromise.resolve().then(() => installStepHook(() => write(LOWERED_ASYNC
    ? { kind: "asyncBuildOk" }
    : { kind: "failed", error: failure("InvalidInput", "AXIOM_MANAGED_ASYNC_BUILD: the bundle was not built with async/await lowered to generators (esbuild --supported:async-await=false); the plan order of async exports would depend on microtask hops") })));
}
