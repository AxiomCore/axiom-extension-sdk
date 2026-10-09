import { createContext, type ExtensionDefinition, type ExtensionResponse } from "./index.js";

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
      response = extension.resume({ requestId: request.request_id, outcomes: request.outcomes, events: request.events });
    } else {
      throw new Error(`unsupported extension bridge message: ${request.kind}`);
    }
  } catch (error) {
    response = {
      kind: "failed",
      error: {
        code: "Guest",
        message: error instanceof Error ? error.message : "managed-language extension failed",
        retryable: false,
      },
    };
  }
  Javy.IO.writeSync(1, new TextEncoder().encode(JSON.stringify(response)));
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
