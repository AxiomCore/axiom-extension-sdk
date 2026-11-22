// The ABI 1.1 runner: async exports on rounds (contract x8d-managed-async.md).
// The async build bundles this module instead of `./runner`; a sync-only
// build keeps the ABI 1.0 runner and none of the async runtime (w2-c2 L-5).
import { HostOperationError, createResumeContext, createContext, type ExtensionDefinition, type ExtensionResponse } from "./index.js";
import { LOWERED_ASYNC, asyncHandlerOf, installStepHook, parseResumeRequest, probeAsyncBuild, startAsyncRound, type AsyncHandler, type ResumeRequest, type RoundRequest, type RoundResponse } from "./async.js";
/** The async build's realm prelude installs the plan-order hooks before it freezes the intrinsics. */
export { installOrderingHooks } from "./async.js";

declare const Javy: {
  IO: {
    readSync(fd: number, destination: Uint8Array): number;
    writeSync(fd: number, source: Uint8Array): number;
  };
};
declare class TextEncoder {
  encode(input?: string): Uint8Array;
}
declare class TextDecoder {
  decode(input?: Uint8Array): string;
}

/** What the async build tells the runner. */
export interface AsyncRunnerOptions {
  /**
   * Exports the build found to be async (the runner's `describe` answer on
   * the unlowered probe build). The async build lowers `async` functions to
   * generators, after which `extension.export(async fn)` can no longer tell
   * an async function from a plain one; these names keep it an async export.
   */
  asyncExports?: readonly string[];
}

/** The async handler of export `name`, if it is async. */
function asyncHandlerFor(extension: ExtensionDefinition, name: string, options: AsyncRunnerOptions): AsyncHandler | undefined {
  if (!Object.prototype.hasOwnProperty.call(extension.exports, name)) return undefined;
  const handler = extension.exports[name];
  const marked = asyncHandlerOf(handler);
  if (marked) return marked;
  return options.asyncExports?.includes(name) && typeof handler === "function" ? (handler as unknown as AsyncHandler) : undefined;
}

export function runExtension(extension: ExtensionDefinition, options: AsyncRunnerOptions = {}): void {
  const request = JSON.parse(new TextDecoder().decode(readAll()));
  if (request.kind === "invokeRound") {
    if (!LOWERED_ASYNC && asyncHandlerFor(extension, (request as RoundRequest).invocation.export, options)) {
      write(failedWith("InvalidInput", "AXIOM_MANAGED_ASYNC_BUILD: async exports need the ABI 1.1 async build (async/await lowered to generators, esbuild --supported:async-await=false); this bundle was not built that way"));
      return;
    }
    runRound(extension, request as RoundRequest, write, options);
    return;
  }
  if (request.kind === "probeAsyncBuild") {
    probeAsyncBuild(write);
    return;
  }
  let response: ExtensionResponse | { kind: "described"; asyncExports: string[] };
  try {
    if (request.kind === "invoke") {
      const handler = extension.exports[request.invocation.export];
      if (!handler) throw new Error(`unknown extension export: ${request.invocation.export}`);
      response = handler(createContext(request.invocation));
    } else if (request.kind === "resume" && extension.resume) {
      response = extension.resume(createResumeContext(request.outcomes, request.events, request.request_id, request.yielded_plan));
    } else if (request.kind === "describe") {
      // Build-time probe: which exports are async (ABI 1.1 rounds).
      response = { kind: "described", asyncExports: Object.keys(extension.exports).filter((name) => asyncHandlerFor(extension, name, options) !== undefined).sort() };
    } else {
      throw new Error(`unsupported extension bridge message: ${request.kind}`);
    }
  } catch (error) {
    response = failed(error);
  }
  write(response);
}

function failedWith(code: string, message: string): ExtensionResponse {
  return { kind: "failed", error: { code, message, retryable: false } } as ExtensionResponse;
}

function failed(error: unknown): ExtensionResponse {
  // H5 (w4-h): a host operation's error keeps its code (the Rust SDK's `?`).
  if (error instanceof HostOperationError) return { kind: "failed", error: { code: error.code, message: error.message, retryable: error.retryable } } as ExtensionResponse;
  return failedWith("Guest", error instanceof Error ? error.message : "managed-language extension failed");
}

function write(response: unknown): void {
  Javy.IO.writeSync(1, new TextEncoder().encode(JSON.stringify(response)));
}

/**
 * One ABI 1.1 round (managed kernel `begin_round`). An async export starts
 * here and answers from a later step; a 1.0 handler in the same module runs
 * `invoke` in round 1 and `resume` (with the newest tape record) after, and
 * its answer carries no `replayed` plans: the kernel vouches for the plans it
 * recorded, as the Rust SDK does for sync exports in a 1.1 module (the kernel
 * refuses that for exports the build recorded as async).
 */
export function runRound(extension: ExtensionDefinition, request: RoundRequest, write: (response: RoundResponse) => void, options: AsyncRunnerOptions = {}): void {
  const handler = extension.exports[request.invocation.export];
  const asyncHandler = asyncHandlerFor(extension, request.invocation.export, options);
  if (asyncHandler) {
    let step: (resume?: ResumeRequest) => void;
    try {
      step = startAsyncRound(asyncHandler, request, write);
    } catch (error) {
      write(failed(error) as RoundResponse);
      return;
    }
    // p-live: on a host that keeps the engine (`request.live`), a step whose
    // stdin carries a `resumeRound` continues the suspended run; every other
    // step reads nothing (the replay path never gives a step input).
    // (A caller that is not the step export, such as the SDK's tests, may
    // pass the message directly.)
    installStepHook(request.live === true ? (resume?: ResumeRequest) => step(resume ?? readResume()) : step);
    return;
  }
  let response: ExtensionResponse;
  try {
    if (!handler) throw new Error(`unknown extension export: ${request.invocation.export}`);
    if (request.round <= 1 || request.tape.length === 0) {
      response = handler(createContext(request.invocation));
    } else if (extension.resume) {
      const last = request.tape[request.tape.length - 1];
      response = extension.resume(createResumeContext(last.outcomes, last.events, request.request_id, request.yielded_plan));
    } else {
      throw new Error("unsupported extension bridge message: resume");
    }
  } catch (error) {
    response = failed(error);
  }
  switch (response.kind) {
    case "yielded":
      write({ kind: "yieldedRound", plan: { Plan: response.plan } });
      break;
    case "completed":
      write({ kind: "completedRound", result: response.result });
      break;
    default:
      write(response);
  }
}

/** p-live: the `resumeRound` on this step's stdin, if the host sent one. */
function readResume(): ResumeRequest | undefined {
  if (typeof Javy === "undefined") return undefined;
  const bytes = readAll();
  if (bytes.length === 0) return undefined;
  return parseResumeRequest(new TextDecoder().decode(bytes));
}

function readAll(): Uint8Array {
  const chunks: Uint8Array[] = [];
  let length = 0;
  while (true) {
    const chunk = new Uint8Array(4096);
    const read = Javy.IO.readSync(0, chunk);
    if (read === 0) break;
    const used = chunk.slice(0, read);
    chunks.push(used);
    length += read;
  }
  const result = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.length;
  }
  return result;
}
