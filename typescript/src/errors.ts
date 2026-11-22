// p-py: the host-operation error on its own, so a sync (ABI 1.0) bundle, whose
// runner needs only this class, does not evaluate the async runtime's top
// level (`async.ts` re-exports it; it is the same class everywhere).
/**
 * A failed host operation. Awaiting `cx.call(...)` rejects with this error
 * when the host answered the operation with an error; an export that lets it
 * escape fails the invocation with the same `code` (the Rust SDK's `?`).
 */
export class HostOperationError extends Error {
  // A class field *defines* the own property: an assignment (`this.name = …`)
  // throws in strict code once `Error.prototype.name` is frozen (the override
  // mistake). The realm freeze leaves Error prototypes writable, so this is
  // defence in depth for guests that freeze them.
  override readonly name = "HostOperationError";
  readonly code: string;
  readonly retryable: boolean;
  constructor(code: string, message: string, retryable = false) {
    super(message);
    this.code = code;
    this.retryable = retryable;
  }
}
