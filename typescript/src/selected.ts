import type { AxiomValue, Completed, Effect, ExtensionContext, ExtensionDefinition, ExtensionHandler, InvocationResult, Yielded } from "./index.js";

export type SelectedType =
  | { kind: "scalar"; value: "unit" | "boolean" | "signed64" | "unsigned64" | "unsigned32" | "unsigned16" | "string" | "bytes" }
  | { kind: "named"; value: string }
  | { kind: "list" | "optional"; value: SelectedType }
  | { kind: "result"; value: { ok: SelectedType; error: SelectedType } };
type Definition =
  | { kind: "alias"; target: SelectedType }
  | { kind: "record"; fields: Array<{ name: string; valueType: SelectedType }> }
  | { kind: "variant"; cases: Array<{ name: string; payload?: SelectedType }> };
export interface SelectedInterface {
  format: "axiom-sdk-interface-ir/v2";
  types: Record<string, Definition>;
  exports: Array<{ name: string; input: SelectedType; output: SelectedType }>;
  imports: Array<{ namespace: string; operation: string; input: SelectedType; output: SelectedType; mode: "call" | "stream" }>;
  interfaceSha256?: string;
  continuations?: Array<{ name: string; export: string; state: SelectedType; branches: Array<{ name: string; namespace: string; operation: string }> }>;
}
/** Validate the finite pure projection once, before activating authored code. */
export function validateSelectedInterface(profile: SelectedInterface): void {
  if (profile.format !== "axiom-sdk-interface-ir/v2" || profile.exports.length > 64 || profile.imports.length > 64 || Object.keys(profile.types).length > 128) fail("unsupported selected interface");
  const identifier = (name: string): void => { if (!/^[A-Za-z][A-Za-z0-9_]{0,63}$/.test(name) || ["self", "Self", "super", "crate"].includes(name)) fail("invalid selected name"); };
  const structural = (type: SelectedType, active: Set<string>, depth: number, work: { left: number }): void => {
    if (depth > 16 || --work.left < 0) fail("selected type exceeds finite bounds");
    switch (type.kind) {
      case "scalar": if (!["unit", "boolean", "signed64", "unsigned64", "unsigned32", "unsigned16", "string", "bytes"].includes(type.value)) fail("unknown scalar"); return;
      case "optional": case "list": structural(type.value, active, depth + 1, work); return;
      case "result": structural(type.value.ok, active, depth + 1, work); structural(type.value.error, active, depth + 1, work); return;
      case "named": {
        if (active.has(type.value) || !owns(profile.types, type.value)) fail("cyclic or unknown selected type");
        active.add(type.value);
        const definition = profile.types[type.value];
        if (definition.kind === "alias") structural(definition.target, active, depth + 1, work);
        else if (definition.kind === "record" || definition.kind === "variant") {
          const members = definition.kind === "record" ? definition.fields : definition.cases;
          if (members.length > (definition.kind === "record" ? 64 : 32) || definition.kind === "variant" && members.length < 1) fail("selected members exceed bounds");
          const seen = new Set<string>();
          for (const member of members) {
            identifier(member.name); if (seen.has(member.name)) fail("duplicate selected member"); seen.add(member.name);
            const port = "valueType" in member ? member.valueType : member.payload;
            if (port) structural(port, active, depth + 1, work);
          }
        } else fail("selected values cannot contain handles or lifecycle types");
        active.delete(type.value); return;
      }
      default: fail("unsupported selected type");
    }
  };
  const check = (type: SelectedType): void => structural(type, new Set(), 0, { left: 4096 });
  for (const name of Object.keys(profile.types)) { identifier(name); check({ kind: "named", value: name }); }
  const exports = new Set<string>(), imports = new Set<string>();
  for (const port of profile.exports) {
    identifier(port.name); if (exports.has(port.name) || port.input.kind !== "named" || port.output.kind !== "named") fail("invalid selected export");
    exports.add(port.name); check(port.input); check(port.output);
  }
  for (const port of profile.imports) {
    const key = JSON.stringify([port.namespace, port.operation]);
    if (typeof port.namespace !== "string" || !port.namespace.length || port.namespace === "axiom.selected" || typeof port.operation !== "string" || !port.operation.length || imports.has(key) || !["call", "stream"].includes(port.mode) || port.input.kind !== "named" || port.output.kind !== "named") fail("invalid selected import");
    imports.add(key); check(port.input); check(port.output);
  }
  if ((profile.continuations?.length ?? 0) > 16) fail("continuation count exceeds bounds");
  const names = new Set<string>();
  for (const continuation of profile.continuations ?? []) {
    identifier(continuation.name);
    if (names.has(continuation.name) || !exports.has(continuation.export) || exports.has(`resume_${continuation.name}`) || continuation.branches.length < 1 || continuation.branches.length > 16) fail("invalid continuation");
    names.add(continuation.name); check(continuation.state);
    const branches = new Set<string>();
    for (const branch of continuation.branches) {
      identifier(branch.name);
      if (branches.has(branch.name) || !profile.imports.some(i => i.namespace === branch.namespace && i.operation === branch.operation && i.mode === "call")) fail("invalid continuation branch");
      branches.add(branch.name);
    }
  }
}
const owns = (v: object, key: string): boolean => Object.prototype.hasOwnProperty.call(v, key);
function fail(reason: string): never { throw new Error(`selected interface: ${reason}`); }
function object(value: unknown): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) fail("expected record");
  return value as Record<string, unknown>;
}
function tag(value: unknown, name: string): unknown {
  const record = object(value);
  if (Object.keys(record).length !== 1 || !owns(record, name)) fail("wrong ABI tag");
  return record[name];
}
function bounded(value: unknown): void {
  // Preflight the object graph before recursive JSON serialization. Wire
  // records introduce extra JSON containers, so this limit is separate from
  // the 32-level / 4096-value semantic codec budget below.
  const stack: Array<[unknown, number]> = [[value, 0]];
  let work = 32_768;
  while (stack.length) {
    const [item, depth] = stack.pop()!;
    if (depth > 96 || --work < 0) fail("value exceeds structural bounds");
    if (item === null || typeof item !== "object") continue;
    const keys = Object.keys(item);
    if (keys.length > 256) fail("value exceeds structural bounds");
    for (const key of keys) stack.push([(item as Record<string, unknown>)[key], depth + 1]);
  }
  // JSON bytes are bounded before allocating the managed transport payload.
  const text = JSON.stringify(value);
  if (text === undefined || text.length > 65_536) fail("value exceeds byte bound");
  let bytes = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c < 0x80) bytes++; else if (c < 0x800) bytes += 2;
    else if (c >= 0xd800 && c <= 0xdbff && i + 1 < text.length && text.charCodeAt(i + 1) >= 0xdc00 && text.charCodeAt(i + 1) <= 0xdfff) { bytes += 4; i++; }
    else bytes += 3;
    if (bytes > 65_536) fail("value exceeds byte bound");
  }
}
function convert(profile: SelectedInterface, type: SelectedType, value: unknown, decode: boolean, work: { left: number }, depth = 0): unknown {
  if (depth > 32 || --work.left < 0) fail("value exceeds structural bounds");
  const visit = (t: SelectedType, v: unknown): unknown => convert(profile, t, v, decode, work, depth + 1);
  switch (type.kind) {
    case "scalar": {
      if (type.value === "unit") {
        if (value !== (decode ? "Null" : null)) fail("expected unit");
        return decode ? null : "Null";
      }
      const name = type.value === "boolean" ? "Bool" : type.value === "string" ? "String" : type.value === "bytes" ? "Bytes" : type.value === "signed64" ? "Signed" : "Unsigned";
      const plain = decode ? tag(value, name) : value;
      if (type.value === "boolean") { if (typeof plain !== "boolean") fail("expected boolean"); }
      else if (type.value === "string") { if (typeof plain !== "string") fail("expected string"); }
      else if (type.value === "bytes") {
        if (!Array.isArray(plain) || plain.length > 256 || plain.some(v => !Number.isInteger(v) || v < 0 || v > 255)) fail("expected bounded bytes");
      } else {
        if (typeof plain !== "number" || !Number.isSafeInteger(plain)) fail("integer exceeds portable exact range");
        if (type.value !== "signed64" && plain < 0) fail("expected unsigned integer");
        if (type.value === "unsigned32" && plain > 0xffffffff) fail("unsigned32 overflow");
        if (type.value === "unsigned16" && plain > 0xffff) fail("unsigned16 overflow");
      }
      return decode ? plain : { [name]: plain };
    }
    case "optional":
      return value === (decode ? "Null" : null) ? (decode ? null : "Null") : visit(type.value, value);
    case "list": {
      const values = decode ? tag(value, "List") : value;
      if (!Array.isArray(values) || values.length > 256) fail("expected bounded list");
      const result = values.map(v => visit(type.value, v));
      return decode ? result : { List: result };
    }
    case "result":
      return variant([{ name: "ok", payload: type.value.ok }, { name: "error", payload: type.value.error }]);
    case "named": {
      if (!owns(profile.types, type.value)) fail("unknown selected type");
      const definition = profile.types[type.value];
      if (definition.kind === "alias") return visit(definition.target, value);
      if (definition.kind === "variant") return variant(definition.cases);
      if (definition.kind !== "record" || definition.fields.length > 64) fail("unsupported selected definition");
      let plain: Record<string, unknown>;
      if (decode) {
        const fields = tag(value, "Record");
        if (!Array.isArray(fields) || fields.length !== definition.fields.length) fail("wrong record field count");
        plain = Object.create(null) as Record<string, unknown>;
        let last = "";
        for (const raw of fields) {
          const field = object(raw);
          if (Object.keys(field).length !== 2 || typeof field.name !== "string" || field.name <= last || !owns(field, "value")) fail("record fields must be sorted and unique");
          last = field.name; plain[field.name] = field.value;
        }
      } else plain = object(value);
      if (Object.keys(plain).length !== definition.fields.length) fail("wrong record field count");
      const result: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
      for (const field of definition.fields) {
        if (!owns(plain, field.name)) fail("missing selected field");
        result[field.name] = visit(field.valueType, plain[field.name]);
      }
      return decode ? result : { Record: Object.keys(result).sort().map(name => ({ name, value: result[name] })) };
    }
  }
  function variant(cases: Array<{ name: string; payload?: SelectedType }>): unknown {
    const plain = object(decode ? tag(value, "Variant") : value);
    const selected = cases.find(c => c.name === plain.case);
    if (!selected) fail("unknown variant case");
    if (selected.payload) {
      if (Object.keys(plain).length !== 2 || !owns(plain, "value")) fail("missing variant payload");
      const payload = visit(selected.payload, plain.value);
      return decode ? { case: plain.case, value: payload } : { Variant: { case: plain.case, value: payload } };
    }
    if (decode ? Object.keys(plain).length !== 2 || plain.value !== null : Object.keys(plain).length !== 1) fail("unexpected variant payload");
    return decode ? { case: plain.case } : { Variant: { case: plain.case, value: null } };
  }
}
export function encodeSelected(profile: SelectedInterface, type: SelectedType, value: unknown): AxiomValue {
  if (profile.format !== "axiom-sdk-interface-ir/v2") fail("unsupported profile");
  bounded(value);
  const wire = convert(profile, type, value, false, { left: 4096 });
  bounded(wire);
  return wire as AxiomValue;
}
export function decodeSelected(profile: SelectedInterface, type: SelectedType, wire: AxiomValue): unknown {
  if (profile.format !== "axiom-sdk-interface-ir/v2") fail("unsupported profile");
  bounded(wire);
  return convert(profile, type, wire, true, { left: 4096 });
}
export function selectedCall(profile: SelectedInterface, namespace: string, operation: string, input: unknown): Effect {
  const selected = profile.imports.find(i => i.namespace === namespace && i.operation === operation);
  if (!selected) fail("operation is outside selected interface");
  return { namespace, operation, input: encodeSelected(profile, selected.input, input) };
}
const CONTINUATION_FORMAT = "axiom-selected-continuation/v1";
function wireRecord(value: unknown): Record<string, unknown> {
  const fields = tag(value, "Record");
  if (!Array.isArray(fields) || fields.length > 64) fail("invalid continuation record");
  const result: Record<string, unknown> = Object.create(null); let previous = "";
  for (const value of fields) {
    const field = object(value);
    if (Object.keys(field).length !== 2 || typeof field.name !== "string" || field.name <= previous || !owns(field, "value")) fail("continuation fields must be sorted and unique");
    previous = field.name; result[field.name] = field.value;
  }
  return result;
}
function exactKeys(value: object, keys: string[]): void {
  if (Object.keys(value).sort().join("\0") !== keys.sort().join("\0")) fail("continuation field set differs");
}
function recordWire(fields: Record<string, AxiomValue>): AxiomValue {
  return { Record: Object.keys(fields).sort().map(name => ({ name, value: fields[name] })) };
}
function continuation(profile: SelectedInterface, name: unknown) {
  const selected = profile.continuations?.find(c => c.name === name);
  if (!selected || !profile.interfaceSha256 || !/^[a-f0-9]{64}$/.test(profile.interfaceSha256)) fail("unknown continuation or interface identity");
  return selected;
}
function checkedYield(profile: SelectedInterface, exportName: string, yielded: Yielded): void {
  bounded(yielded);
  const effects = yielded.plan.effects;
  if (!Array.isArray(effects) || effects.length < 2 || effects.length > 17 || effects[0].namespace !== "axiom.selected" || effects[0].operation !== "continue") fail("selected yield requires its versioned descriptor");
  const control = wireRecord(effects[0].input);
  exactKeys(control, ["format", "interfaceSha256", "kind", "state"]);
  if (tag(control.format, "String") !== CONTINUATION_FORMAT || tag(control.interfaceSha256, "String") !== profile.interfaceSha256) fail("continuation format or interface differs");
  const selected = continuation(profile, tag(control.kind, "String"));
  if (selected.export !== exportName || effects.length !== selected.branches.length + 1) fail("continuation is outside selected export");
  decodeSelected(profile, selected.state, control.state as AxiomValue);
  selected.branches.forEach((branch, index) => {
    const effect = effects[index + 1];
    if (effect.namespace !== branch.namespace || effect.operation !== branch.operation) fail("continuation branch order differs");
    const port = profile.imports.find(i => i.namespace === branch.namespace && i.operation === branch.operation)!;
    decodeSelected(profile, port.input, effect.input);
  });
}
function checkedResume(profile: SelectedInterface, context: { requestId?: number; outcomes: unknown[]; events: unknown[] }) {
  bounded(context);
  if (context.outcomes.length !== 1 || context.events.length || !Number.isSafeInteger(context.requestId) || context.requestId! <= 0) fail("selected resume requires one owned host envelope");
  const outcome = object(context.outcomes[0]); exactKeys(outcome, ["index", "result"]);
  if (outcome.index !== 0) fail("selected resume index differs");
  const control = wireRecord(tag(outcome.result, "Ok")); exactKeys(control, ["format", "interfaceSha256", "kind", "outcomes", "owner", "request", "state"]);
  if (tag(control.format, "String") !== CONTINUATION_FORMAT || tag(control.interfaceSha256, "String") !== profile.interfaceSha256 || tag(control.request, "Unsigned") !== context.requestId) fail("selected resume identity differs");
  const owner = tag(control.owner, "Unsigned");
  if (typeof owner !== "number" || !Number.isSafeInteger(owner) || owner <= 0) fail("invalid selected resume owner");
  const selected = continuation(profile, tag(control.kind, "String"));
  const state = decodeSelected(profile, selected.state, control.state as AxiomValue);
  const raw = wireRecord(control.outcomes); exactKeys(raw, selected.branches.map(b => b.name));
  const outcomes: Record<string, unknown> = Object.create(null);
  for (const branch of selected.branches) {
    const variant = object(tag(raw[branch.name], "Variant")); exactKeys(variant, ["case", "value"]);
    if (variant.case === "ok") {
      const port = profile.imports.find(i => i.namespace === branch.namespace && i.operation === branch.operation)!;
      outcomes[branch.name] = { case: "ok", value: decodeSelected(profile, port.output, variant.value as AxiomValue) };
    } else if (variant.case === "error") {
      const failure = wireRecord(variant.value); exactKeys(failure, ["code", "retryable"]);
      const code = tag(failure.code, "String"), retryable = tag(failure.retryable, "Bool");
      if (typeof code !== "string" || !["InvalidInput", "Denied", "Conflict", "Exhausted", "Deadline", "Cancelled", "Host", "Guest", "Protocol"].includes(code) || typeof retryable !== "boolean") fail("invalid selected host failure");
      outcomes[branch.name] = { case: "error", value: { code, retryable } };
    } else fail("unknown selected outcome");
  }
  return { selected, state, outcomes };
}

// A private stamp makes compiler-added boundary checking idempotent for an
// immutable SDK-owned wrapper with the exact same descriptor. An authored
// factory cannot mint the stamp or replace a stamped export/resume function.
const checkedProfiles = new WeakMap<ExtensionDefinition, SelectedInterface>();
function copyProfile(source: unknown): SelectedInterface {
  let work = 32768, bytes = 262144;
  const copy = (value: unknown, depth: number): unknown => {
    if (depth > 96 || --work < 0) fail("selected descriptor exceeds structural bounds");
    if (value === null || typeof value === "boolean") { bytes -= 5; return value; }
    if (typeof value === "string") { bytes -= value.length * 3 + 2; if (bytes < 0) fail("selected descriptor exceeds byte bounds"); return value; }
    if (typeof value === "number" && Number.isFinite(value)) { bytes -= 24; return value; }
    if (typeof value !== "object") fail("selected descriptor must contain pure data");
    if (Array.isArray(value)) {
      if (value.length > 256) fail("selected descriptor exceeds item bounds");
      return Object.freeze(value.map(item => copy(item, depth + 1)));
    }
    const fields = Object.keys(value as object);
    if (fields.length > 256) fail("selected descriptor exceeds field bounds");
    const result: Record<string, unknown> = Object.create(null);
    for (const key of fields) {
      bytes -= key.length * 3 + 4; if (bytes < 0) fail("selected descriptor exceeds byte bounds");
      const child = (value as Record<string, unknown>)[key];
      if (child !== undefined) result[key] = copy(child, depth + 1);
    }
    return Object.freeze(result);
  };
  const profile = copy(source, 0) as SelectedInterface;
  if (bytes < 0) fail("selected descriptor exceeds byte bounds");
  validateSelectedInterface(profile);
  return profile;
}
function sameProfile(source: unknown, snapshot: SelectedInterface): boolean {
  let work = 32768;
  const same = (a: unknown, b: unknown, depth: number): boolean => {
    if (depth > 96 || --work < 0) return false;
    if (a === b) return true;
    if (!a || !b || typeof a !== "object" || typeof b !== "object" || Array.isArray(a) !== Array.isArray(b)) return false;
    if (Array.isArray(a) && Array.isArray(b)) return a.length <= 256 && a.length === b.length && a.every((item, index) => same(item, b[index], depth + 1));
    const left = Object.keys(a).filter(key => (a as Record<string, unknown>)[key] !== undefined), right = Object.keys(b);
    return left.length === right.length && left.every(key => owns(b as object, key) && same((a as Record<string, unknown>)[key], (b as Record<string, unknown>)[key], depth + 1));
  };
  return same(source, snapshot, 0);
}

/** Generated named handlers use schema-declared state and input tables. */
export function defineNamedSelectedExtension(profile: SelectedInterface, handlers: Record<string, unknown>): ExtensionDefinition {
  validateSelectedInterface(profile);
  const resumes = object(handlers.resume);
  exactKeys(handlers, [...profile.exports.map(e => e.name), "resume"]);
  exactKeys(resumes, (profile.continuations ?? []).map(c => c.name));
  const exports: Record<string, ExtensionHandler> = Object.create(null);
  for (const port of profile.exports) {
    if (typeof handlers[port.name] !== "function") fail("missing selected handler");
    exports[port.name] = context => {
      const effects: Record<string, (state: unknown, calls: unknown) => Yielded> = Object.create(null);
      for (const selected of profile.continuations ?? []) if (selected.export === port.name) {
        effects[selected.name] = (state, input) => {
          const calls = object(input); exactKeys(calls, selected.branches.map(b => b.name));
          const control = recordWire({ format: { String: CONTINUATION_FORMAT }, interfaceSha256: { String: profile.interfaceSha256! }, kind: { String: selected.name }, state: encodeSelected(profile, selected.state, state) });
          return { kind: "yielded", plan: { effects: [{ namespace: "axiom.selected", operation: "continue", input: control }, ...selected.branches.map(b => selectedCall(profile, b.namespace, b.operation, calls[b.name]))] } };
        };
      }
      return (handlers[port.name] as (context: unknown) => Completed | Yielded)({ ...context, effects });
    };
  }
  return checkedExtension(profile, { exports, resume(context) {
    const resume = checkedResume(profile, context);
    const handler = resumes[resume.selected.name];
    if (typeof handler !== "function") fail("missing selected resume handler");
    const port = profile.exports.find(e => e.name === resume.selected.export)!;
    return (handler as (context: unknown) => Completed)({ state: resume.state, outcomes: resume.outcomes, complete(output: unknown): Completed {
      return { kind: "completed", result: { output: encodeSelected(profile, port.output, output), patches: [], transactions: [], emitted_events: [] } };
    } });
  } });
}
/** Process-independent boundary checks; grants and proposals remain host-owned. */
export function checkedExtension(sourceProfile: SelectedInterface, extension: ExtensionDefinition): ExtensionDefinition {
  const existing = checkedProfiles.get(extension);
  if (existing && sameProfile(sourceProfile, existing)) return extension;
  // Retain a private snapshot so later mutation of an authored descriptor
  // cannot change the selected boundary or justify the private stamp.
  const profile = copyProfile(sourceProfile);
  const named = !!profile.continuations?.length;
  if (named ? !extension.resume : !!extension.resume) fail("resume handlers differ from selected profile");
  if (Object.keys(extension.exports).length !== profile.exports.length) fail("export set differs from selection");
  const exports: Record<string, ExtensionHandler> = Object.create(null) as Record<string, ExtensionHandler>;
  for (const selected of profile.exports) {
    if (!owns(extension.exports, selected.name) || typeof extension.exports[selected.name] !== "function" || owns(exports, selected.name)) fail("missing or duplicate selected export");
    exports[selected.name] = (context: ExtensionContext): Completed | Yielded => {
      const input = decodeSelected(profile, selected.input, context.invocation.input);
      const checked = { ...context, input,
        complete(output: unknown, changes: Partial<InvocationResult> = {}): Completed {
          return { kind: "completed", result: { output: encodeSelected(profile, selected.output, output), patches: changes.patches ?? [], transactions: changes.transactions ?? [], emitted_events: changes.emitted_events ?? [] } };
        },
      };
      const response = extension.exports[selected.name](checked);
      if (named && response.kind === "yielded") { checkedYield(profile, selected.name, response); return response; }
      if (response.kind !== "completed") fail("selected immediate export must return a checked completion");
      decodeSelected(profile, selected.output, response.result.output);
      return response;
    };
  }
  const result: ExtensionDefinition = Object.freeze({ exports: Object.freeze(exports), ...(named ? { resume(context: Parameters<NonNullable<ExtensionDefinition["resume"]>>[0]): Completed {
    const resumed = checkedResume(profile, context);
    const response = extension.resume!(context);
    if (response.kind !== "completed") fail("selected resume must complete within its single resume budget");
    const port = profile.exports.find(e => e.name === resumed.selected.export)!;
    decodeSelected(profile, port.output, response.result.output);
    return response;
  } } : {}) });
  checkedProfiles.set(result, profile);
  return result;
}
