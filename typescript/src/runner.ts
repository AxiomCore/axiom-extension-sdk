import { HostOperationError, createResumeContext, createContext, type ExtensionDefinition, type ExtensionResponse } from "./index.js";

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
  let response: ExtensionResponse;
  try {
    if (request.kind === "invoke") {
      const handler = extension.exports[request.invocation.export];
      if (!handler) throw new Error(`unknown extension export: ${request.invocation.export}`);
      response = handler(createContext(request.invocation));
    } else if (request.kind === "resume" && extension.resume) {
      response = extension.resume(createResumeContext(request.outcomes, request.events, request.request_id, request.yielded_plan));
    } else {
      throw new Error(`unsupported extension bridge message: ${request.kind}`);
    }
  } catch (error) {
    // H5 (w4-h): a host operation's error keeps its code (the Rust SDK's `?`).
    response = {
      kind: "failed",
      error: error instanceof HostOperationError
        ? { code: error.code, message: error.message, retryable: error.retryable }
        : {
          code: "Guest",
          message: error instanceof Error ? error.message : "managed-language extension failed",
          retryable: false,
        },
    };
  }
  Javy.IO.writeSync(1, new TextEncoder().encode(JSON.stringify(response)));
}

function readAll(): Uint8Array {
  // A request that fits one chunk (EOF on the second read) needs no list.
  const first = new Uint8Array(4096);
  const read = Javy.IO.readSync(0, first);
  if (read === 0) return first.subarray(0, 0);
  const next = new Uint8Array(4096);
  const more = Javy.IO.readSync(0, next);
  if (more === 0) return first.subarray(0, read);
  return readRest([first.subarray(0, read), next.subarray(0, more)], read + more);
}

/** The general read loop, continued after the chunks already read. */
function readRest(chunks: Uint8Array[], length: number): Uint8Array {
  while (true) {
    const chunk = new Uint8Array(4096);
    const read = Javy.IO.readSync(0, chunk);
    if (read === 0) break;
    // The chunk is not reused: a view of its first `read` bytes is a copy's equal.
    chunks.push(chunk.subarray(0, read));
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

// The ABI 1.1 runner (async exports, rounds) lives in `./runner-async`; this
// re-export keeps `runRound` importable from here without bundling the async
// runtime into a sync-only build (`sideEffects: false`).
export { runRound } from "./runner-async.js";
