import assert from 'node:assert/strict';
import test from 'node:test';
import { createContext } from '../dist/index.js';
import { checkedExtension, encodeSelected, decodeSelected, selectedCall } from '../dist/selected.js';

const scalar = value => ({ kind: 'scalar', value });
const named = value => ({ kind: 'named', value });
const profile = {
  format: 'axiom-sdk-interface-ir/v2',
  types: {
    Draft: { kind: 'record', fields: [{ name: 'offset', valueType: scalar('signed64') }, { name: 'title', valueType: scalar('string') }] },
    Outcome: { kind: 'variant', cases: [{ name: 'success', payload: named('Draft') }, { name: 'failure', payload: scalar('string') }, { name: 'empty' }] },
  },
  exports: [{ name: 'normalize', input: named('Draft'), output: named('Draft') }],
  imports: [{ namespace: 'catalog', operation: 'lookup', input: named('Draft'), output: named('Draft'), mode: 'call' }],
};
const input = { offset: 7, title: ' Synthetic ' };
const wire = { Record: [{ name: 'offset', value: { Signed: 7 } }, { name: 'title', value: { String: ' Synthetic ' } }] };
const plain = value => JSON.parse(JSON.stringify(value));

test('selected codecs preserve exact typed tags and canonical records', () => {
  assert.deepEqual(encodeSelected(profile, named('Draft'), input), wire);
  assert.deepEqual(plain(decodeSelected(profile, named('Draft'), wire)), input);
  for (const value of [{ ...input, secret: 'PRIVATE' }, { ...input, offset: 1.5 }, { ...input, offset: Number.MAX_SAFE_INTEGER + 1 }]) {
    assert.throws(() => encodeSelected(profile, named('Draft'), value));
  }
  for (const bad of [
    { Record: [...wire.Record].reverse() },
    { Record: [wire.Record[0], wire.Record[0]] },
    { Record: [{ name: 'offset', value: { Unsigned: 7 } }, wire.Record[1]] },
    { Record: [{ ...wire.Record[0], private: true }, wire.Record[1]] },
  ]) assert.throws(() => decodeSelected(profile, named('Draft'), bad));
});

test('selected variants, bytes, lists and numeric profiles enforce bounds', () => {
  for (const value of [{ case: 'empty' }, { case: 'success', value: input }, { case: 'failure', value: 'Synthetic failure' }]) {
    assert.deepEqual(plain(decodeSelected(profile, named('Outcome'), encodeSelected(profile, named('Outcome'), value))), value);
  }
  assert.throws(() => encodeSelected(profile, named('Outcome'), { case: 'empty', value: null }));
  assert.throws(() => decodeSelected(profile, named('Outcome'), { Variant: { case: 'empty', value: { String: 'PRIVATE' } } }));
  assert.throws(() => encodeSelected(profile, scalar('unsigned16'), 65536));
  assert.throws(() => encodeSelected(profile, scalar('bytes'), [256]));
  assert.throws(() => encodeSelected(profile, { kind: 'list', value: scalar('boolean') }, Array(257).fill(true)));
  assert.throws(() => encodeSelected(profile, scalar('string'), 'é'.repeat(32768)));
  const recursive = { ...profile, types: { Loop: { kind: 'alias', target: named('Loop') } } };
  assert.throws(() => encodeSelected(recursive, named('Loop'), null), /structural bounds/);
});

test('checked entry and completion cannot bypass input or output selection', () => {
  let calls = 0;
  const extension = checkedExtension(profile, { exports: {
    normalize(context) { calls++; return context.complete({ ...context.input, title: context.input.title.trim() }); },
  } });
  const invoke = input => extension.exports.normalize(createContext({ export: 'normalize', input, snapshots: [], deadline_unix_ms: 5000 }));
  assert.deepEqual(invoke(wire).result.output, encodeSelected(profile, named('Draft'), { offset: 7, title: 'Synthetic' }));
  assert.equal(calls, 1);
  assert.throws(() => invoke({ Record: [{ name: 'offset', value: { Unsigned: 7 } }, wire.Record[1]] }));
  assert.equal(calls, 1, 'invalid input never enters the authored handler');
  const bypass = checkedExtension(profile, { exports: { normalize: () => ({ kind: 'completed', result: { output: { String: 'PRIVATE' }, patches: [], transactions: [], emitted_events: [] } }) } });
  assert.throws(() => bypass.exports.normalize(createContext({ export: 'normalize', input: wire, snapshots: [], deadline_unix_ms: 5000 })));
});

test('generated calls carry checked values and unsupported resume profiles fail closed', () => {
  assert.deepEqual(selectedCall(profile, 'catalog', 'lookup', input), { namespace: 'catalog', operation: 'lookup', input: wire });
  assert.throws(() => selectedCall(profile, 'network', 'fetch', input));
  assert.throws(() => selectedCall(profile, 'catalog', 'lookup', { title: 'Synthetic' }));
  assert.throws(() => checkedExtension(profile, { exports: {} }));
  assert.throws(() => checkedExtension(profile, { exports: { normalize() {} }, resume() {} }), /resume handlers differ/);
  assert.throws(() => checkedExtension({ ...profile, continuations: [{ name: 'resume', export:'absent', state:named('Draft'), branches:[] }] }, { exports: { normalize() {} } }), /invalid continuation/);
});
