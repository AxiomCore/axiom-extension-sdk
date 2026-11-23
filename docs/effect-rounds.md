# Effect rounds: await independent effects together

An async export runs in **effect rounds** (ABI 1.1). Every time the guest waits
for host results, the host runs that round's effects and then calls the guest
again. Each round costs one full guest round trip. For TypeScript and Python
guests, each round also starts the guest's engine again and replays the earlier
rounds. So the number of rounds, not the number of effects, sets most of an
async export's latency.

Effects that are awaited **together** run in **one** round. Effects awaited
**one after another** cost one round each, even when the second one does not
need the first one's result.

## The rule

If two effects do not depend on each other's results, await them together:

| Language | One round per effect (slow) | One round for both (fast) |
|---|---|---|
| TypeScript | `const a = await cx.call(load.call(1));`<br>`const b = await cx.call(price.call(2));` | `const [a, b] = await Promise.all([cx.call(load.call(1)), cx.call(price.call(2))]);` |
| Python | `a = await cx.call(load(1))`<br>`b = await cx.call(price(2))` | `a, b = await gather(cx.call(load(1)), cx.call(price(2)))` |
| Rust | `let a = cx.call(LOAD.call(1)).await?;`<br>`let b = cx.call(PRICE.call(2)).await?;` | `let (a, b) = try_join!(cx.call(LOAD.call(1)), cx.call(PRICE.call(2))).await?;` |

`Promise.all`, `gather(...)` and `try_join!` fail fast: when one effect fails,
the whole call fails. To wait for every result, use `Promise.allSettled`,
`gather(..., return_exceptions=True)` or `join!`. The language READMEs
("Async exports") describe the exact ordering rules.

## When you must keep them sequential

Keep the awaits one after another when:

- **the second effect needs the first one's result**, for example
  `price.call(a)` after `const a = await ...`. This is a real data dependency.
- **either effect writes**: a contract mutation, a store write, a UI-state
  write or a log line. Writes are ordered, and running a write together with a
  read changes what the read can observe.
- **the effects touch the same key and one of them writes it.**

Only contract **queries** may run as a group in one round. A group of guest
effects holds contract queries only (the broker refuses anything else).

## The build warning

`axiom extension build` warns when a TypeScript or Python async export awaits
two contract queries one right after the other, and the second one's
arguments do not use anything the first statement assigned:

```
warning[AXIOM_EFFECT_SEQUENTIAL_AWAIT]: extension `pricing`: pricing.ts:12 and pricing.ts:13 (in `quote`):
the contract queries `catalog.load` and `catalog.price` are awaited one after the other, so each costs
its own effect round (a full guest round trip), although the second one's input does not use the first
one's result. Fix: await them together with `Promise.all`, ...
```

The warning is guidance. It does not change the build output, the module, the
effect summary or any hash. The build **never rewrites** your code: grouping
changes when effects run, and the compiler cannot prove that is safe in
general.

The check is deliberately narrow. It does not warn when:

- either effect is a write or not a granted contract query (both effects must
  resolve to `contract-runtime` permissions with `access = "query"` in the
  requested authority);
- the effect is built dynamically (the namespace or operation is not a string
  literal, or the helper that builds it is reassigned);
- other statements stand between the two awaits;
- the second effect's arguments use a name the first statement assigned, or
  contain an `await`, a function literal or a `yield`;
- the second effect's arguments call a function (it could read what the first
  statement stored), other than the conversions `Number`, `String`, `Boolean`,
  `BigInt` (TypeScript) or `int`, `str`, `len` and similar builtins (Python);
- a parameter or local shadows the effect helper or the SDK constructor;
- the awaits go through different context objects.

The check works on syntax, so it can miss independent awaits, for example
awaits inside helper calls or loops. Missing one costs latency only; the
program still runs correctly.

Rust guests are not checked yet. The same rule applies to them: use `join!` or
`try_join!`.
