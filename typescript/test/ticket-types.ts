// Strict type test (tsc --noEmit --strict): resume.get(ticket) infers the
// operation's output type, and misuse does not compile.
import { createContext, createResumeContext, Effects, operation, type Ticket } from '../dist/index.js';
import { selectedOperation, type SelectedInterface } from '../dist/selected.js';

const getName = operation<{ id: number }, string>('catalog', 'name');
const inStock = operation<number, boolean>('catalog', 'stock');
const context = createContext({ deadline_unix_ms: 0, export: 'run', input: 'Null', snapshots: [] });
const plan: Effects = context.plan();
const name = plan.call(getName.call({ id: 1 }));
const stock = plan.call(inStock.call(1));
const untyped = plan.call({ namespace: 'catalog', operation: 'raw', input: 'Null' });
context.effects(plan);

const resume = createResumeContext([], []);
const n: string = resume.get(name);
const s: boolean = resume.get(stock);
const u: unknown = resume.get(untyped);
const typed: Ticket<string> = name;
// @ts-expect-error a string ticket does not decode as a number
const wrong: number = resume.get(name);
// @ts-expect-error a boolean ticket is not a string ticket
const swapped: Ticket<string> = stock;
// @ts-expect-error a raw index is not a ticket
resume.get(0);
// @ts-expect-error tickets cannot be forged from an object literal
const forged: Ticket<string> = { index: 0 };
// @ts-expect-error operation input is typed
getName.call({ id: 'x' });
// @ts-expect-error an untyped ticket yields unknown, not a string
const notString: string = resume.get(untyped);

declare const profile: SelectedInterface;
const lookup = selectedOperation<{ title: string }, { title: string; offset: number }>(profile, 'catalog', 'lookup');
const draft = resume.get(resume.plan().call(lookup.call({ title: 'x' })));
const title: string = draft.title;
void [n, s, u, typed, wrong, swapped, forged, notString, title];
