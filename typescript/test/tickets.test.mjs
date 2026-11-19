import assert from 'node:assert/strict';
import test from 'node:test';
import { createContext, createResumeContext, Effects, operation, yieldFor } from '../dist/index.js';
import { selectedCall, selectedOperation } from '../dist/selected.js';

const context = () => createContext({ deadline_unix_ms: 0, export: 'run', input: 'Null', snapshots: [] });
const ok = (index, value) => ({ index, result: { Ok: value } });
const getName = operation('catalog', 'name');
const inStock = operation('catalog', 'stock');
const raw = { namespace: 'catalog', operation: 'raw', input: { Record: [{ name: 'id', value: { Unsigned: 3 } }] } };

test('tickets round-trip typed outcomes in declaration order', () => {
  const ctx = context();
  const plan = ctx.plan();
  const name = plan.call(getName.call({ id: 7 }));
  const stock = plan.call(inStock.call(7));
  const other = plan.call(raw);
  assert.deepEqual([name.index, stock.index, other.index, plan.length], [0, 1, 2, 3]);
  assert.ok(Object.isFrozen(name));
  const yielded = ctx.effects(plan);
  assert.equal(yielded.kind, 'yielded');
  assert.equal(yielded.plan.effects.length, 3);
  const resume = createResumeContext([ok(0, { String: 'Linen' }), ok(1, { Bool: true }), ok(2, 'Null')], [], 1);
  assert.equal(resume.get(name), 'Linen');
  assert.equal(resume.get(stock), true);
  assert.equal(resume.get(other), null);
  assert.equal(resume.get(name), 'Linen', 'tickets can be read again');
});

test('plan bytes are identical with and without tickets', () => {
  const effects = () => [getName.call({ id: 7 }), inStock.call(7), raw];
  const before = JSON.stringify(context().effects(...effects()));
  const ctx = context();
  const plan = ctx.plan();
  for (const effect of effects()) plan.call(effect);
  assert.equal(JSON.stringify(ctx.effects(plan)), before);
  const again = new Effects();
  for (const effect of effects()) again.call(effect);
  assert.equal(JSON.stringify(yieldFor(again)), before);
  createResumeContext([], []); // clear this heap's yielded-plan record
});

test('a ticket from another plan or an earlier yield step is rejected', () => {
  const ctx = context();
  const first = ctx.plan();
  const a = first.call(getName.call(1));
  const notYielded = new Effects().call(getName.call(1));
  ctx.effects(first);
  const resume = createResumeContext([ok(0, { String: 'a' })], [], 1);
  assert.throws(() => resume.get(notYielded), /another effect plan or yield step/);
  assert.equal(resume.get(a), 'a');
  // Second step: a new plan is yielded; the first step's ticket is stale.
  const second = ctx.plan();
  const b = second.call(getName.call(2));
  ctx.effects(second);
  const next = createResumeContext([ok(0, { String: 'b' })], [], 1);
  assert.throws(() => next.get(a), /another effect plan or yield step/);
  assert.equal(next.get(b), 'b');
});

test('a plan rebuilt in the resume is accepted only with the yielded shape', () => {
  const ctx = context();
  const yielded = ctx.plan();
  yielded.call(getName.call(1));
  yielded.call(inStock.call(1));
  ctx.effects(yielded);
  const resume = createResumeContext([ok(0, { String: 'n' }), ok(1, { Bool: true })], [], 1);
  const same = resume.plan();
  const name = same.call(getName.call(1));
  same.call(inStock.call(1));
  assert.equal(resume.get(name), 'n');
  // The rebuild must repeat the inputs too (w1-f1, M1/L3).
  const ctx2 = context();
  const y2 = ctx2.plan();
  y2.call(getName.call(1));
  y2.call(inStock.call(1));
  ctx2.effects(y2);
  const other = createResumeContext([ok(0, { String: 'n' }), ok(1, { Bool: true })], [], 1);
  const changed = other.plan();
  const changedName = changed.call(getName.call(0));
  changed.call(inStock.call(1));
  assert.throws(() => other.get(changedName), /call 0 \(catalog\.name\) has a different input/);
  const again = ctx.plan();
  again.call(getName.call(1));
  again.call(inStock.call(1));
  ctx.effects(again);
  const next = createResumeContext([ok(0, { String: 'n' }), ok(1, { Bool: true })], [], 2);
  const swapped = next.plan();
  const first = swapped.call(inStock.call(0));
  swapped.call(getName.call(0));
  assert.throws(() => next.get(first), /rebuilt effect plan differs from the yielded plan: call 0 is catalog\.stock, the yielded call 0 was catalog\.name/);
});

test('in a fresh engine the resume rebuilds the plan and cannot mix plans', () => {
  // No plan was yielded in this heap (fresh store per exchange).
  createResumeContext([], []);
  const outcomes = [ok(0, { String: 'x' }), ok(1, { Bool: false })];
  const resume = createResumeContext(outcomes, [], 4);
  const plan = resume.plan();
  const name = plan.call(getName.call(0));
  const stock = plan.call(inStock.call(0));
  assert.equal(resume.get(name), 'x');
  assert.equal(resume.get(stock), false);
  assert.throws(() => plan.call(raw), /already yielded or read/);
  const other = resume.plan();
  const foreign = other.call(getName.call(0));
  other.call(inStock.call(0));
  assert.throws(() => resume.get(foreign), /different effect plans/);
  const short = createResumeContext(outcomes, [], 5);
  const one = short.plan().call(getName.call(0));
  assert.throws(() => short.get(one), /plan of 1 call\(s\), but this resume carries 2 outcome\(s\)/);
});

test('forged tickets, host failures and sealed plans fail closed', () => {
  createResumeContext([], []);
  const resume = createResumeContext([{ index: 0, result: { Err: { code: 'Denied', message: 'no', retryable: false } } }], [], 1);
  assert.throws(() => resume.get({ index: 0 }), /expected an effect ticket/);
  assert.throws(() => resume.get(0), /expected an effect ticket/);
  const plan = resume.plan();
  const ticket = plan.call(getName.call(0));
  assert.throws(() => resume.get(ticket), /host operation denied/);
  const ctx = context();
  const yielded = ctx.plan();
  yielded.call(raw);
  ctx.effects(yielded);
  assert.throws(() => yielded.call(raw), /already yielded/);
  assert.throws(() => ctx.effects(yielded, raw), /one Effects plan/);
  createResumeContext([], []);
});

test('deprecated positional outcome(i) still works', () => {
  createResumeContext([], []);
  const resume = createResumeContext([ok(0, { String: 'a' }), ok(1, { Unsigned: 2 })], [], 1);
  assert.equal(resume.outcome(0), 'a');
  assert.equal(resume.outcome(1), 2);
  assert.throws(() => resume.outcome(2), /unavailable/);
});

test('selected operations encode and decode through the selected interface', () => {
  const scalar = value => ({ kind: 'scalar', value });
  const named = value => ({ kind: 'named', value });
  const profile = {
    format: 'axiom-sdk-interface-ir/v2',
    types: { Draft: { kind: 'record', fields: [{ name: 'offset', valueType: scalar('signed64') }, { name: 'title', valueType: scalar('string') }] } },
    exports: [{ name: 'normalize', input: named('Draft'), output: named('Draft') }],
    imports: [{ namespace: 'catalog', operation: 'lookup', input: named('Draft'), output: named('Draft'), mode: 'call' }],
  };
  const lookup = selectedOperation(profile, 'catalog', 'lookup');
  assert.throws(() => selectedOperation(profile, 'catalog', 'missing'), /outside selected interface/);
  const draft = { offset: 7, title: 'T' };
  assert.equal(JSON.stringify(lookup.call(draft)), JSON.stringify(selectedCall(profile, 'catalog', 'lookup', draft)));
  createResumeContext([], []);
  const wire = { Record: [{ name: 'offset', value: { Signed: 7 } }, { name: 'title', value: { String: 'T' } }] };
  const resume = createResumeContext([ok(0, wire)], [], 1);
  const ticket = resume.plan().call(lookup.call(draft));
  assert.deepEqual({ ...resume.get(ticket) }, draft);
  const bad = createResumeContext([ok(0, { Record: [{ name: 'offset', value: { Unsigned: 7 } }, wire.Record[1]] })], [], 2);
  const badTicket = bad.plan().call(lookup.call(draft));
  assert.throws(() => bad.get(badTicket), /selected interface/);
});

test('strict type test: resume.get(ticket) infers the output type and misuse does not compile', async () => {
  const { execFileSync } = await import('node:child_process');
  const { fileURLToPath } = await import('node:url');
  execFileSync(process.execPath, [fileURLToPath(new URL('../node_modules/typescript/bin/tsc', import.meta.url)), '--noEmit', '--strict', '--target', 'ES2022', '--module', 'NodeNext', '--moduleResolution', 'NodeNext', fileURLToPath(new URL('./ticket-types.ts', import.meta.url))], { stdio: 'pipe' });
});
