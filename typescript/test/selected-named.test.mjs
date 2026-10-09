import assert from 'node:assert/strict';
import test from 'node:test';
import { createContext } from '../dist/index.js';
import { checkedExtension, defineNamedSelectedExtension, encodeSelected } from '../dist/selected.js';

const named = value => ({ kind: 'named', value });
const profile = {
  format: 'axiom-sdk-interface-ir/v2', interfaceSha256: 'a'.repeat(64),
  types: {
    Key: { kind: 'record', fields: [{ name: 'id', valueType: { kind: 'scalar', value: 'string' } }] },
    Draft: { kind: 'record', fields: [{ name: 'title', valueType: { kind: 'scalar', value: 'string' } }] },
  },
  exports: [{ name: 'load', input: named('Key'), output: named('Draft') }],
  imports: [{ namespace: 'catalog', operation: 'lookup', input: named('Key'), output: named('Draft'), mode: 'call' }],
  continuations: [{ name: 'loaded', export: 'load', state: named('Key'), branches: [{ name: 'task', namespace: 'catalog', operation: 'lookup' }] }],
};
const record = fields => ({ Record: Object.keys(fields).sort().map(name => ({ name, value: fields[name] })) });
const envelope = (result = { Variant: { case: 'ok', value: encodeSelected(profile, named('Draft'), { title: 'Synthetic' }) } }) => ({
  requestId: 7, events: [], outcomes: [{ index: 0, result: { Ok: record({
    format: { String: 'axiom-selected-continuation/v1' }, interfaceSha256: { String: profile.interfaceSha256 },
    kind: { String: 'loaded' }, owner: { Unsigned: 9 }, request: { Unsigned: 7 },
    state: encodeSelected(profile, named('Key'), { id: 'synthetic' }), outcomes: record({ task: result }),
  }) } }],
});
const invoke = extension => extension.exports.load(createContext({ export: 'load', input: encodeSelected(profile, named('Key'), { id: 'synthetic' }), snapshots: [], deadline_unix_ms: 5000 }));
const make = onResume => defineNamedSelectedExtension(profile, {
  load: ctx => ctx.effects.loaded(ctx.input, { task: ctx.input }),
  resume: { loaded: ctx => { onResume?.(ctx); return ctx.complete({ title: ctx.outcomes.task.case === 'ok' ? ctx.outcomes.task.value.title : 'Safe failure' }); } },
});

test('named state and outcomes survive constructing a fresh extension', () => {
  const yielded = invoke(make());
  assert.equal(yielded.kind, 'yielded');
  assert.equal(yielded.plan.effects.length, 2);
  assert.equal(yielded.plan.effects[0].namespace, 'axiom.selected');
  assert.equal(yielded.plan.effects[1].namespace, 'catalog');
  assert.equal(yielded.plan.effects[1].operation, 'lookup');
  let calls = 0;
  const result = make(ctx => { calls++; assert.equal(ctx.state.id, 'synthetic'); assert.equal(ctx.outcomes.task.value.title, 'Synthetic'); }).resume(envelope());
  assert.equal(calls, 1);
  assert.deepEqual(result.result.output, encodeSelected(profile, named('Draft'), { title: 'Synthetic' }));
});

test('malformed host envelopes are rejected before entering authored resume', () => {
  let calls = 0;
  const extension = make(() => calls++);
  const mutations = [
    c => { c.requestId = 8; },
    c => { c.outcomes.push(c.outcomes[0]); },
    c => { c.outcomes[0].index = 1; },
    c => { c.events.push({ stream_id: 1, sequence: 1, events: [] }); },
    c => { c.outcomes[0].result.Ok.Record.find(f => f.name === 'owner').value = { Unsigned: 0 }; },
    c => { c.outcomes[0].result.Ok.Record.find(f => f.name === 'interfaceSha256').value.String = 'b'.repeat(64); },
    c => { c.outcomes[0].result.Ok.Record.find(f => f.name === 'outcomes').value.Record[0].name = 'unselected'; },
    c => { c.outcomes[0].result.Ok.Record.find(f => f.name === 'outcomes').value.Record[0].value.Variant.value = { String: 'PRIVATE_OUTPUT_MARKER' }; },
    c => { c.outcomes[0].result.Ok.Record.reverse(); },
  ];
  for (const mutate of mutations) {
    const c = envelope(); mutate(c);
    assert.throws(() => extension.resume(c), /selected interface/);
  }
  assert.equal(calls, 0);
});

test('named errors expose only closed safe failure fields', () => {
  const result = { Variant: { case: 'error', value: record({ code: { String: 'Deadline' }, retryable: { Bool: true } }) } };
  const response = make(ctx => assert.deepEqual(JSON.parse(JSON.stringify(ctx.outcomes.task)), { case: 'error', value: { code: 'Deadline', retryable: true } })).resume(envelope(result));
  assert.deepEqual(response.result.output, encodeSelected(profile, named('Draft'), { title: 'Safe failure' }));
  result.Variant.value.Record.push({ name: 'secret', value: { String: 'PRIVATE_FAILURE_MARKER' } });
  assert.throws(() => make().resume(envelope(result)), /continuation field set/);
});

test('raw factory output, second yields and undeclared calls cannot bypass selection', () => {
  const raw = checkedExtension(profile, { exports: { load: () => invoke(make()) }, resume: () => ({ kind: 'completed', result: { output: { String: 'PRIVATE' }, patches: [], transactions: [], emitted_events: [] } }) });
  assert.throws(() => raw.resume(envelope()), /selected interface/);
  const twice = checkedExtension(profile, { exports: { load: () => invoke(make()) }, resume: () => invoke(make()) });
  assert.throws(() => twice.resume(envelope()), /single resume budget/);
  const wrong = checkedExtension(profile, { exports: { load: () => {
    const yielded = invoke(make()); yielded.plan.effects[1].operation = 'write'; return yielded;
  } }, resume: () => {} });
  assert.throws(() => invoke(wrong), /branch order/);
  assert.throws(() => defineNamedSelectedExtension(profile, { load() {}, resume: {} }), /field set/);
  assert.throws(() => make().exports.load(createContext({ export: 'load', input: { String: 'PRIVATE' }, snapshots: [], deadline_unix_ms: 5000 })), /selected interface/);
});

test('codec preflight bounds cyclic and deeply nested caller values', () => {
  const cycle = {}; cycle.loop = cycle;
  assert.throws(() => encodeSelected(profile, named('Key'), cycle), /structural bounds/);
  let deep = 'Synthetic'; for (let i = 0; i < 1000; i++) deep = [deep];
  assert.throws(() => encodeSelected(profile, named('Key'), deep), /structural bounds/);
});

test('compiler boundary wrapping preserves exact immutable SDK checks without duplicate wrappers', () => {
  const extension = make();
  assert.equal(checkedExtension(structuredClone(profile), extension), extension);
  assert.equal(checkedExtension(Object.fromEntries(Object.entries(profile).reverse()), extension), extension);
  assert.ok(Object.isFrozen(extension) && Object.isFrozen(extension.exports));
  assert.throws(() => { extension.resume = () => ({ kind: 'completed' }); }, TypeError);
  assert.throws(() => { extension.exports.load = () => ({ kind: 'completed' }); }, TypeError);
  const different = structuredClone(profile); different.interfaceSha256 = 'b'.repeat(64);
  assert.notEqual(checkedExtension(different, extension), extension);
  const mutable = structuredClone(profile);
  const raw = checkedExtension(mutable, { exports: { load: () => invoke(make()) }, resume: () => ({ kind: 'completed', result: { output: { String: 'PRIVATE' }, patches: [], transactions: [], emitted_events: [] } }) });
  mutable.exports[0].output = { kind: 'named', value: 'Key' };
  assert.throws(() => raw.resume(envelope()), /selected interface/);
});
