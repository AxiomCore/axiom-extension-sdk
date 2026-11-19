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
// job. The managed kernel calls `_start` again on the same instance; the
// bundle's first statement (an esbuild banner) sees the step hook installed
// below and calls it instead of re-running the bundle. Each step therefore
// begins exactly when the queue is empty. No timers are involved.
import type { AxiomValue, Completed, Effect, ExtensionHandler, Invocation, InvocationResult, SecretHandle, TypedEffect } from "./index.js";
import { PatchBuilder, StateSnapshot, createContext, decodeValue, effectDecoder, encodeValue } from "./index.js";

/** Group format of an ABI 1.1 `Collect` round group (`abi::EFFECT_GROUP_FORMAT`). */
export const EFFECT_GROUP_FORMAT = "axiom-extension-effect-group/v1";
/** At most this many host operations may be pending together in one round. */
export const MAX_GROUP_BRANCHES = 32;
/** The `limit` of a round group is `min(n, MAX_GROUP_LIMIT)`. */
export const MAX_GROUP_LIMIT = 8;
/** Name of the step hook the esbuild banner of an async build calls. */
export const STEP_HOOK = "__axiom_managed_step__";

/**
 * A failed host operation. Awaiting `cx.call(...)` rejects with this error
 * when the host answered the operation with an error; an export that lets it
 * escape fails the invocation with the same `code` (the Rust SDK's `?`).
 */
export class HostOperationError extends Error {
  readonly code: string;
  readonly retryable: boolean;
  constructor(code: string, message: string, retryable = false) {
    super(message);
    this.name = "HostOperationError";
    this.code = code;
    this.retryable = retryable;
  }
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
    throw new Error("async export needs an ABI 1.1 managed build (Javy event loop and the async step banner); this module was invoked through the ABI 1.0 bridge");
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
  private segment: Registered[] = [];
  private cursor = 0;
  private readonly replayed: RoundPlanJson[] = [];
  private settled: { ok: true; value: unknown } | { ok: false; error: unknown } | undefined;
  private answered = false;
  private readonly completions = new WeakSet<object>();

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
  }

  private register(effect: unknown): Promise<unknown> {
    // Operations started after the export returned are never planned or sent.
    if (this.settled || this.answered) return new Promise(() => {});
    const plain = plainEffect(effect);
    const decode = effectDecoder(effect as object);
    return new Promise((resolve, reject) => { this.segment.push({ effect: plain, resolve, reject, decode }); });
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
      this.segment = [];
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
    const segment = this.segment;
    this.segment = [];
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

/** Install the step hook the async build's banner calls on re-entry. */
export function installStepHook(step: () => void): void {
  Object.defineProperty(globalThis, STEP_HOOK, { value: step, writable: false, enumerable: false, configurable: true });
}
