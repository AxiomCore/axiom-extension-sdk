// M1/L3 (w1-f1): the managed host echoes the yielded plan on resume
// (`yielded_plan`, host-internal bridge JSON), so a plan rebuilt in a fresh
// engine is checked call by call: namespace, operation and input.
import assert from 'node:assert/strict';
import test from 'node:test';
import { createContext, createResumeContext, operation } from '../dist/index.js';

const context = () => createContext({ deadline_unix_ms: 0, export: 'run', input: 'Null', snapshots: [] });
const ok = (index, value) => ({ index, result: { Ok: value } });
// The kernel's per-call record: canonical input text inline, or only its length.
const call = (namespace, op, canonical) => ({ namespace, operation: op, input_length: canonical.length, input: canonical });
const lengthOnly = (namespace, op, canonical) => ({ namespace, operation: op, input_length: canonical.length });
const eff = (op, item) => ({ namespace: 'catalog', operation: op, input: { Unsigned: item } });
// The kernel's record of a yielded plan [name(1), stock(1)].
const yielded = [call('catalog', 'name', '{"Unsigned":"1"}'), call('catalog', 'stock', '{"Unsigned":"1"}')];
const outcomes = [ok(0, { String: 'Linen' }), ok(1, { Bool: true })];
// A fresh engine: this heap never yielded the plan being resumed.
const fresh = (record, values = outcomes) => { context(); return createResumeContext(values, [], 7, record); };

test('fresh engine: a correct rebuild passes the host record check', () => {
  const resume = fresh(yielded);
  const plan = resume.plan();
  const name = plan.push(eff('name', 1));
  const stock = plan.push(eff('stock', 1));
  assert.equal(resume.get(name), 'Linen');
  assert.equal(resume.get(stock), true);
  // Any plan object may carry the rebuild, not only resume.plan().
  const other = fresh(yielded);
  const typed = context().plan();
  const n = typed.push(operation('catalog', 'name').call(1));
  typed.push(operation('catalog', 'stock').call(1));
  assert.equal(other.get(n), 'Linen');
});

test('fresh engine: swapped calls are rejected (verifier fg_swap)', () => {
  const resume = fresh(yielded);
  const plan = resume.plan();
  const stock = plan.push(eff('stock', 1));
  plan.push(eff('name', 1));
  assert.throws(() => resume.get(stock), /rebuilt effect plan differs from the yielded plan: call 0 is catalog\.stock, the yielded call 0 was catalog\.name/);
});

test('fresh engine: different operations are rejected (verifier fg_ops)', () => {
  const resume = fresh(yielded);
  const plan = resume.plan();
  plan.push(eff('name', 1));
  const owner = plan.push(eff('owner', 1));
  assert.throws(() => resume.get(owner), /call 1 is catalog\.owner, the yielded call 1 was catalog\.stock/);
  const ns = fresh(yielded);
  const p2 = ns.plan();
  const t = p2.push({ ...eff('name', 1), namespace: 'other' });
  p2.push(eff('stock', 1));
  assert.throws(() => ns.get(t), /call 0 is other\.name, the yielded call 0 was catalog\.name/);
});

test('fresh engine: swapped inputs are rejected (verifier fg_inputs)', () => {
  const record = [call('catalog', 'name', '{"Unsigned":"1"}'), call('catalog', 'name', '{"Unsigned":"2"}')];
  const resume = fresh(record);
  const plan = resume.plan();
  const second = plan.push(eff('name', 2));
  plan.push(eff('name', 1));
  assert.throws(() => resume.get(second), /call 0 \(catalog\.name\) has a different input than the yielded call 0/);
});

test('fresh engine: a different call count is rejected (verifier fg_count)', () => {
  const resume = fresh(yielded);
  const one = resume.plan().push(eff('name', 1));
  assert.throws(() => resume.get(one), /the rebuilt plan has 1 call\(s\), the yielded plan had 2/);
});

test('old kernel (no yielded_plan field): falls back to the call-count check', () => {
  for (const record of [undefined, null]) {
    const resume = fresh(record);
    const plan = resume.plan();
    const stock = plan.push(eff('stock', 1));
    plan.push(eff('name', 1));
    // Without the host record a same-length swap is not detectable (documented).
    assert.equal(resume.get(stock), 'Linen');
    const short = fresh(record);
    const one = short.plan().push(eff('name', 1));
    assert.throws(() => short.get(one), /plan of 1 call\(s\), but this resume carries 2 outcome\(s\)/);
  }
});

test('a malformed host record fails closed', () => {
  for (const bad of [{}, 'x', [{ namespace: 'catalog', operation: 'name' }], [{ namespace: 'c', operation: 'o', input_length: -1 }],
    [{ namespace: 'c', operation: 'o', input_length: 3, input: 'four' }], [{ namespace: 'c', operation: 'o', input_length: 1, input: 7 }]]) {
    context();
    assert.throws(() => createResumeContext(outcomes, [], 1, bad), /malformed yielded plan record/);
  }
});

test('the canonical value text matches the kernel golden text', () => {
  // Same value and text as the kernel test canonical_value_text_and_digest_are_golden;
  // integers arrive in every form the bridge allows (number or exact string),
  // and object keys in any order.
  const golden = '{"Record":[{"name":"big","value":{"Unsigned":"18446744073709551615"}},{"name":"neg","value":{"Signed":"-9223372036854775808"}},{"name":"txt","value":{"String":"é\\"\\\\\\n\\u0001😀/"}},{"name":"bytes","value":{"Bytes":[0,255]}},{"name":"list","value":{"List":["Null",{"Bool":true}]}},{"name":"var","value":{"Variant":{"case":"some","value":{"Bool":false}}}},{"name":"none","value":{"Variant":{"case":"none","value":null}}},{"name":"h","value":{"Handle":{"id":"9007199254740993","generation":2,"kind":"Secret"}}}]}';
  assert.equal(golden.length, 508, 'UTF-16 length pinned by the kernel test');
  const input = { Record: [
    { name: 'big', value: { Unsigned: '18446744073709551615' } },
    { name: 'neg', value: { Signed: '-9223372036854775808' } },
    { name: 'txt', value: { String: 'é"\\\n\u0001😀/' } },
    { name: 'bytes', value: { Bytes: [0, 255] } },
    { name: 'list', value: { List: ['Null', { Bool: true }] } },
    { name: 'var', value: { Variant: { value: { Bool: false }, case: 'some' } } },
    { name: 'none', value: { Variant: { case: 'none', value: null } } },
    { value: { Handle: { kind: 'Secret', generation: 2, id: 9007199254740993n.toString() } }, name: 'h' },
  ] };
  const record = [call('catalog', 'put', golden)];
  const resume = fresh(record, [ok(0, 'Null')]);
  const ticket = resume.plan().push({ namespace: 'catalog', operation: 'put', input });
  assert.equal(resume.get(ticket), null);
  // Safe integers may arrive as numbers.
  const numbers = fresh([call('c', 'o', '{"List":[{"Unsigned":"7"},{"Signed":"-7"}]}')], [ok(0, 'Null')]);
  assert.equal(numbers.get(numbers.plan().push({ namespace: 'c', operation: 'o', input: { List: [{ Unsigned: 7 }, { Signed: -7 }] } })), null);
  // One changed byte anywhere in the input is a different input.
  const changed = structuredClone(input);
  changed.Record[3].value.Bytes[1] = 254;
  const again = fresh(record, [ok(0, 'Null')]);
  const t2 = again.plan().push({ namespace: 'catalog', operation: 'put', input: changed });
  assert.throws(() => again.get(t2), /different input/);
});

test('inputs beyond the inline budget are compared by canonical length', () => {
  const big = { String: 'x'.repeat(9000) };
  const text = `{"String":"${'x'.repeat(9000)}"}`;
  const same = fresh([lengthOnly('store', 'put', text)], [ok(0, 'Null')]);
  assert.equal(same.get(same.plan().push({ namespace: 'store', operation: 'put', input: big })), null);
  const longer = fresh([lengthOnly('store', 'put', text)], [ok(0, 'Null')]);
  const t = longer.plan().push({ namespace: 'store', operation: 'put', input: { String: 'x'.repeat(9001) } });
  assert.throws(() => longer.get(t), /call 0 \(store\.put\) has a different input/);
});

test('same heap (persistent engine): the shape check compares input too', () => {
  const c = context();
  const y = c.plan();
  y.push(eff('name', 1));
  y.push(eff('name', 2));
  c.effects(y);
  const r = createResumeContext([ok(0, { String: 'item-1' }), ok(1, { String: 'item-2' })], [], 4);
  const z = r.plan();
  const second = z.push(eff('name', 2));
  z.push(eff('name', 1));
  assert.throws(() => r.get(second), /call 0 \(catalog\.name\) has a different input than the yielded call 0/);
});

// The verifier's repro/ts/stale_heap.mjs, as assertions (L3).
test('stale heap: an abandoned request\'s plan never answers a later raw-effects resume', () => {
  const name = operation('catalog', 'name');
  // 1. Request A yields plan P and is cancelled; request B yields raw effects.
  let c = context();
  const p = c.plan();
  const stale = p.push(name.call(1));
  c.effects(p);
  c = context();
  c.effects(name.call(2));
  let r = createResumeContext([ok(0, { String: 'B-result' })], [], 2);
  // P was yielded by an earlier invocation: rejected even without a host record.
  assert.throws(() => r.get(stale), /another effect plan or yield step/);
  // Also with the host record of B's plan present.
  r = createResumeContext([ok(0, { String: 'B-result' })], [], 2, [call('catalog', 'name', '{"Unsigned":"2"}')]);
  assert.throws(() => r.get(stale), /another effect plan or yield step/);
  // A fresh rebuild of P's call (not B's) fails on the host record.
  const wrong = r.plan().push(name.call(1));
  assert.throws(() => r.get(wrong), /call 0 \(catalog\.name\) has a different input/);
  // 2. A correct rebuild of B's raw plan is accepted (was falsely rejected).
  c = context();
  const q = c.plan();
  q.push(operation('x', 'other').call(1));
  c.effects(q);
  c = context();
  c.effects(name.call(3));
  r = createResumeContext([ok(0, { String: 'B2' })], [], 3);
  const t = r.plan().push(name.call(3));
  assert.equal(r.get(t), 'B2');
  // 3. A raw yield in the same request also clears an earlier plan record.
  c = context();
  const q2 = c.plan();
  q2.push(operation('x', 'other').call(1));
  c.effects(q2);
  c.effects(name.call(4));
  r = createResumeContext([ok(0, { String: 'B3' })], [], 4);
  assert.equal(r.get(r.plan().push(name.call(4))), 'B3');
});
