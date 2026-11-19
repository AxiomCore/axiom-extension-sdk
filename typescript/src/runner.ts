import { createResumeContext, createContext, type ExtensionDefinition, type ExtensionResponse } from "./index.js";
import { asyncHandlerOf, installStepHook, startAsyncRound, type RoundRequest, type RoundResponse } from "./async.js";

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

export function runExtension(extension: ExtensionDefinition): void {
  const request = JSON.parse(new TextDecoder().decode(readAll()));
  if (request.kind === "invokeRound") {
    runRound(extension, request as RoundRequest, write);
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
      response = { kind: "described", asyncExports: Object.keys(extension.exports).filter((name) => asyncHandlerOf(extension.exports[name]) !== undefined).sort() };
    } else {
      throw new Error(`unsupported extension bridge message: ${request.kind}`);
    }
  } catch (error) {
    response = failed(error);
  }
  write(response);
}

function failed(error: unknown): ExtensionResponse {
  return {
    kind: "failed",
    error: {
      code: "Guest",
      message: error instanceof Error ? error.message : "managed-language extension failed",
      retryable: false,
    },
  };
}

function write(response: unknown): void {
  Javy.IO.writeSync(1, new TextEncoder().encode(JSON.stringify(response)));
}

/**
 * One ABI 1.1 round (managed kernel `begin_round`). An async export starts
 * here and answers from a later step; a 1.0 handler in the same module runs
 * `invoke` in round 1 and `resume` (with the newest tape record) after, and
 * its answer carries no `replayed` plans: the kernel vouches for the plans it
 * recorded, as the Rust SDK does for sync exports in a 1.1 module.
 */
export function runRound(extension: ExtensionDefinition, request: RoundRequest, write: (response: RoundResponse) => void): void {
  const handler = extension.exports[request.invocation.export];
  const asyncHandler = asyncHandlerOf(handler);
  if (asyncHandler) {
    let step: () => void;
    try {
      step = startAsyncRound(asyncHandler, request, write);
    } catch (error) {
      write(failed(error) as RoundResponse);
      return;
    }
    installStepHook(step);
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
