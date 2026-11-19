# Axiom Python extension SDK

This SDK describes the checked Python 3.12 portable authoring surface. The
source driver consumes the type stubs and lowers authored code ahead of time;
the SDK is not imported by a host Python interpreter at runtime.

Supported code is deterministic, has no ambient I/O, and can interact with the
application only through the generated `Context`, selectors, effect plans, and
atomic patches. Unsupported Python syntax fails at build time with a source
location. Exact third-party dependencies must be universal
`py3-none-any.whl` artifacts in `AxiomDeps.lock` and are parsed, never installed
or executed.

The working 0.1.1 candidate also supports generated selected interfaces and
named finite continuations. A selected source imports the generated
`Context_<export>` and `Resume_<continuation>` types. It returns a checked
completion or uses `context.effects.<continuation>(state, inputs)`; a handler
marked `@extension.resume` consumes typed branch outcomes. The host owns the
effect execution and fresh resume invocation. Python interpreter state is not
preserved across that boundary.

Selected pure packages use the v2 package schema; named-continuation packages
use v3. These package versions are separate from ABI v1. State, arguments,
replies and outputs remain bounded and checked by the host. Handles, arbitrary
async/asyncio work, streams in named continuations and ambient I/O are rejected
by the initial managed source profile. Generated imports do not grant authority.

The complete source and selection are in
`examples/advanced-language-dx/selected-extensions/python` in the AxiomCore
workspace. Candidate source support does not imply a published package version
or browser/mobile qualification; use the exact generated interface and managed
toolchain selected by the CLI.

## Typed tickets

```python
from typing import TYPE_CHECKING

from axiom_extension_sdk import Ticket, extension, typed_effect

if TYPE_CHECKING:
    from axiom_extension_sdk import Context, Effects, ResumeContext


def lookup(plan: "Effects", item: int) -> "tuple[Ticket[str], Ticket[bool]]":
    # One function builds the plan, for the export and for the resume.
    name = plan.push(typed_effect("catalog", "name", {"Unsigned": item}, str))
    stock = plan.push(typed_effect("catalog", "stock", {"Unsigned": item}, bool))
    return name, stock


@extension.export
def run(ctx: "Context"):
    plan = ctx.plan()
    lookup(plan, 7)
    return ctx.effects(plan)


@extension.resume
def resume(ctx: "ResumeContext"):
    name, stock = lookup(ctx.plan(), 7)  # same calls, same inputs, same order
    return ctx.complete({"name": ctx.get(name), "stock": ctx.get(stock)})
```

`typed_effect(namespace, operation, input, output)` returns the plain effect
dict (`input` in wire form, as for a hand-written effect) typed
`TypedEffect[output]`, so `plan.push` gives `Ticket[output]` and `ctx.get`
returns `output` under pyright strict, with no hand annotation. `Ticket`,
`TypedEffect`, `Effects` and `typed_effect` import at runtime as well as under
`TYPE_CHECKING`; importing a name the SDK does not provide fails at compile
time. Use `plan.push`: host objects never expose a member named `call`.
`ctx.outcome(i)` is deprecated.

In TypeScript and Python every exchange runs in a fresh engine, so tickets
do not survive from the export to the resume. The resume rebuilds the plan it
answers with `resume.plan()`: the **same calls (namespace, operation and
input) in the same order**, usually by calling the same function that built
the yielded plan. The managed host echoes its record of the yielded plan with
the resume, and `get` rejects a rebuilt plan that differs, naming the first
differing call (for example `call 0 is catalog.stock, the yielded call 0 was
catalog.name`). Inputs are compared exactly up to 8 KiB per call and 64 KiB
per plan; larger inputs are compared by their canonical length. A plan of more
than about 1,700 calls, or a host that predates the echo, gets only the
call-count check. When a resume cannot rebuild its plan deterministically, use
a named continuation, whose state the host carries.
