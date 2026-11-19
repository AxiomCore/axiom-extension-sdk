# Axiom Rust extension SDK

The working 0.1.2 candidate includes generated selected finite codecs and named
continuations. Selected package schemas v2/v3 remain separate from ABI v1. A
named yield carries bounded typed state and declared branch inputs; the host
authorizes and executes those effects, validates their outcomes, and invokes a
fresh named resume. It does not preserve guest heap state or infer extra grants.
The complete selected example lives in
`examples/advanced-language-dx/selected-extensions/rust` in the AxiomCore workspace.
Use that cohort's generated bindings and exact ABI/derive dependencies. Candidate
source capability does not imply a published version or target qualification.

Use the typed prelude, reviewed macros, and permission-scoped bindings generated
from `AxiomDeps.toml`:

```rust
use crate::axiom_bindings::ui::Cart;
use axiom_extension_sdk::prelude::*;

#[derive(Default)]
struct Pricing;

#[derive(AxiomType)]
struct PriceResult {
    discount: Cents,
    total: Cents,
}

#[extension]
impl Pricing {
    #[export]
    fn apply(&mut self, invocation: TypedInvocation) -> Result<ExtensionResponse> {
        let cart = Cart::from(&invocation)?;
        let subtotal = cart.subtotal_cents::<Cents>()?;
        let discount = Percentage::from_whole(15)?.of(subtotal)?;
        let total = subtotal.checked_sub(discount)?;

        Ok(ExtensionResponse::new(PriceResult { discount, total })
            .patch(cart.patch().discount_cents(discount).total_cents(total)))
    }
}
```

`#[extension]` checks and generates the typed dispatch, declared export table,
unknown-export error, and host factory. `#[export(name = "...")]` can be used
when the wire export should differ from the Rust method name. Export names are
checked against the manifest before compilation. Authors do not write
`TypedExtension`, `axiom_extension()`, or ABI messages. Projects do maintain a
small `Cargo.toml` and `Cargo.lock` with an exact SDK dependency so
rust-analyzer and the Axiom build resolve the same package.

`Cart`, its readable fields, and its writable patch methods exist only when the
extension's resolved authority contains those capabilities. Extension code
never receives a reference to host state; reads decode an authorized snapshot
and writes produce a revision-checked proposal for the host to validate.

The prelude provides:

- canonical `AxiomEncode`, `AxiomDecode`, and `#[derive(AxiomType)]` records;
- `TypedInvocation`, `TypedExtension`, `ExtensionResponse`, and structured
  `SdkError`/`Result`;
- typed UI state, backend store patches and transactions;
- generated contract/resource operations, batched effects, resume outcomes,
  and typed stream batches;
- deterministic `Cents`, `Percentage`, `TimestampMs`, `DurationMs`, `Id`, and
  `Revision` values; and
- safe defaults for initialization, resume rejection, cancellation, and
  shutdown.

Advanced compatibility code can explicitly import
`axiom_extension_sdk::raw::*`. The raw namespace is not needed for ordinary
extensions and does not create authority: the source driver rejects authored
construction of generated host bindings and capability handles.

The typed layer lowers to the unchanged `axiom-extension-abi@1.0.0` AXE1 wire
format. It is an authoring API, not a second runtime protocol.

## Typed tickets

```rust
use axiom_extension_sdk::{prelude::*, Binding};

const LOOKUP: Operation<u64, String> = Operation::generated(Binding {
    namespace: "contract:catalog", operation: "query_product", input: "Unsigned", output: "String",
});

#[derive(Default)]
struct Guest { name: Option<Ticket<String>> }

impl TypedExtension for Guest {
    fn invoke(&mut self, _: TypedInvocation) -> Result<ExtensionResponse> {
        let mut fx = Effects::new();
        self.name = Some(fx.push(LOOKUP.call(42)));
        Ok(ExtensionResponse::yield_for(fx))
    }
    fn resume(&mut self, resume: ResumeContext) -> Result<ExtensionResponse> {
        let name: String = resume.get(self.name.take().unwrap())?;
        Ok(ExtensionResponse::new(name))
    }
}
```

`fx.push(op.call(input))` returns a `Ticket<O>` (`Copy`, no public
constructor); `resume.get(ticket)` decodes the outcome as `O`. The typed
adapter records the plan it yielded, so a ticket from another plan or an
earlier yield step is an `InvalidInput` error, never another call's value.
`ResumeContext::outcome(i)` is deprecated. The by-value chain
`Effects::new().call(a).call(b)` still works but returns no tickets; use
`push`, the spelling shared with TypeScript and Python. (In those managed
languages the resume rebuilds the plan in a fresh engine; see the root
README.)

## Async exports: rounds, replay, determinism

An `async fn` export gets several dependent host results in one invocation
(ABI 1.1, ADR 0001). Write straight-line code; independent calls awaited
together are sent as one batch automatically:

```rust
use axiom_extension_sdk::{prelude::*, Binding};

const LOAD: Operation<u64, u64> = Operation::generated(Binding {
    namespace: "catalog", operation: "load", input: "Unsigned", output: "Unsigned",
});
const PRICE: Operation<u64, u64> = Operation::generated(Binding {
    namespace: "catalog", operation: "price", input: "Unsigned", output: "Unsigned",
});
const STOCK: Operation<u64, u64> = Operation::generated(Binding {
    namespace: "catalog", operation: "stock", input: "Unsigned", output: "Unsigned",
});

#[derive(Default)]
struct Checkout;

#[extension]
impl Checkout {
    #[export]
    async fn apply(cx: &Cx, id: u64) -> Result<u64> {
        let user = cx.call(LOAD.call(id)).await?;                          // round 1
        let (price, stock) =
            join!(cx.call(PRICE.call(user)), cx.call(STOCK.call(user))).await; // round 2: one group
        Ok(price? * stock?)                                                // completes
    }
}
```

- **Signature.** `#[export] async fn name(cx: &Cx, input: In) -> Result<Out>`
  inside an `#[extension]` impl; `In: AxiomDecode`, `Out: AxiomEncode`. No
  `self`: no guest memory survives from one round to the next. `cx.call(op)`
  takes the typed call of a generated binding (`OPERATION.call(input)`) and
  resolves to the operation's declared output type (`Result<O>`), so output
  types are checked as with tickets. `cx.patch(..)`, `cx.transaction(..)`,
  `cx.emit(..)` add to the final result; `cx.invocation()` gives the input and
  the round-1 snapshots (`UiState::generated(cx.invocation())` as in sync
  exports). The host revision-checks final patches against the round-1
  snapshots. When the input parameter is `Value`, the input is moved into the
  argument (no copy every round); in such an export read it from the
  argument: `cx.input()` returns `InvalidInput` and `cx.invocation().input()`
  sees `Null`. Typed inputs are decoded by reference and stay readable both
  ways.
- **ABI version.** An impl with at least one async export makes
  `axiom_extension()` return an `AsyncExtension`; the module then exports ABI
  1.1 (`axiom_extension_abi_version`, a constant read at package time) and
  negotiates 1.1 when the host offers it. Its sync exports keep working
  inside 1.1 sessions. An impl with only sync exports is unchanged: ABI 1.0,
  identical frames, and its module contains no round-handling code (the export
  macros build the 1.0-only machine). On a host that offers only 1.0, an async
  export may yield once (one batched resume) and fails `Exhausted` if it needs
  more.
- **Rounds.** Each host operation registers in the current round when first
  polled. When nothing else can run, the executor sends **one** plan with
  every pending operation, in registration order: one operation is a plain
  plan, several are a `Collect` group (branches `b0`, `b1`, …; all results
  returned, no fail-fast; at most 32 branches and the package's `maxEffects`).
  Sequential awaits cost one round each; `join!` / `join_all` cost one round
  for the whole batch. `try_join!` / `async_rt::try_join_all` are the
  fail-fast forms (TypeScript `Promise.all`, Python `gather`): after a poll
  pass in which a child failed they return the first error in argument order
  and drop the others (operations already registered still run); `join!` is
  TypeScript `Promise.allSettled` / Python `gather(..., return_exceptions=True)`. `maxRounds` (authority, default 1, at most 16) bounds
  the number of rounds that yield; one more yield is refused (`Exhausted`) and
  nothing commits. Request it per extension in `AxiomDeps.toml`:

  ```toml
  [extensions.checkout.budgets]
  maxRounds = 3   # the number of sequential await rounds the export needs
  ```

  Raising it is a reviewed permission increase. A source build warns
  (`AXIOM_ASYNC_MAX_ROUNDS`) when a module has async exports and requests the
  default of 1. An export that never yields more than once needs nothing.
- **Replay.** Every round re-runs the export from the top with the same input.
  Awaits the host already answered resolve immediately from its tape, in
  order; the first unanswered batch becomes the next plan. The SDK reports the
  hashes of the replayed plans and checks them against the tape; a guest that
  plans differently on replay fails with `Protocol` ("non-deterministic
  guest") and nothing commits.
- **Determinism rules.** Time, randomness and state come only from host
  operations (they are on the tape). Do not branch on `static`/global state
  or on `cx.round()` before an await. `join!` polls in argument order. There
  is no `select!`: if a hand-written combinator drops an operation after it
  was polled, it still runs in its round and its result is discarded;
  operations still pending when the export returns are never sent. Awaiting a
  non-host future (a timer, a channel) fails with `InvalidInput`.
- **Errors.** A failed or denied operation resolves its future with `Err`
  (use `?`, or handle it and continue). In a `join!` group every branch gets
  its own outcome: a domain failure or a branch the broker refuses (no grant,
  revoked; `Denied`) is returned to that branch only and the others get their
  values (ADR 0001 third clarification). A panic traps as in sync exports.
  Effects run in earlier rounds have happened even if a later round fails;
  only patches and transactions are atomic. Prefer idempotent operations.
- **Cost.** Replay re-executes the prefix every round, so guest fuel grows
  roughly with the square of the number of rounds. Fuel is one budget for
  the whole invocation, and the host sizes it as `fuel_per_invocation ×
  (maxRounds + 1)` (ADR 0001 fourth clarification); the deadline and the
  host-call and byte budgets are not scaled. The SDK hashes each replayed
  plan once per instance and moves `Value` inputs, so the exit-gate scenario
  costs about 1.08× the hand-written raw guest (see
  `planning/extension-sandbox/contracts/x8c-rust-async.md` for measurements).

## Third-party crates

Dependencies are declared per extension in `AxiomDeps.toml`, using an exact
version and a named, reproducible environment:

```toml
format = "axiom-deps/v2"
targets = ["android", "ios", "web", "server"]

[dependencyEnvironments.rust-release]
language = "rust"
profile = "release"
runtimeVersion = "1.82.0"
toolchainSha256 = "<64 lowercase hex>"
engine = "cargo"
engineSha256 = "<64 lowercase hex>"
abi = "axiom-extension-abi@1"
sdk = "axiom-extension-sdk@1"
bindingSchema = "axiom-bindings@1"
targetFamily = "wasm32"

[dependencyRegistries.crates]
language = "rust"
index = "https://registry.example/axiom-rust-index.json"
allowedHosts = ["packages.example", "registry.example"]

[extensions.pricing]
language = "rust"
source = "sandbox/member_pricing.rs"
exports = ["apply"]
targets = ["android", "ios", "web", "server"]
environment = "rust-release"

[extensions.pricing.packages.rust-decimal]
version = "1.36.0"
registry = "crates"
features = ["maths"]
```

Run `axiom dependencies check` before resolution; `format`, `migrate`,
`schema`, `completions`, and `hover` expose the same v2 model to developers,
editors, and agents. `axiom dependencies resolve` creates the canonical
`AxiomDeps.lock`. During a
source build, Axiom verifies the lock and content-addressed package bytes, then
materializes one hidden Cargo workspace with one member for the extension. The
workspace is built frozen and offline. Extension-local manifests, path or Git
dependencies, build scripts, native links, proc-macro packages, workspace
takeover, and undeclared features are rejected. Two compatible extensions can
reuse the same verified package bytes without sharing instances or authority.
Package dependencies grant no runtime permission; `[extensions.*.permissions]`
remains a separate, default-deny capability request.

## Deterministic unit tests

The standard-library SDK includes `axiom_extension_sdk::test::TestHost`. It
holds typed UI/store snapshots, applies revision-checked atomic proposals,
supplies declared effect results, and records a deterministic audit trail:

```rust
use axiom_extension_sdk::{prelude::*, test::TestHost};

let mut extension = axiom_extension();
let mut host = TestHost::new()
    .ui_state("cart", 7, CartState { subtotal_cents: 23_700 })
    .allow_ui_write("cart", "discount_cents");

let run = host.invoke(extension.as_mut(), "apply", 23_700_u64)?;
let cart: CartState = host.ui("cart")?;

assert_eq!(host.revision("ui:cart"), Some(8));
host.assert_wrote("ui:cart", "discount_cents");
host.assert_no_denials();
```

The host also supports backend store objects and transactions, declared host
effect outcomes, event counts, continuation counts, and denied-operation
assertions. It never gives the extension a mutable application object.

## Inspect what Axiom generates

```bash
laxiom extensions rust-inspect pricing \
  --deps AxiomDeps.toml

laxiom extensions rust-inspect pricing \
  --deps AxiomDeps.toml \
  --view macros

laxiom extensions rust-inspect pricing --view interface
laxiom extensions rust-inspect pricing --view workspace
laxiom extensions rust-inspect pricing --view bindings
laxiom extensions rust-inspect pricing --json
```

The views expose the semantic macro lowering, canonical language-neutral
interface, generated Cargo member, and exact authority-shaped bindings. The
generated workspace is evidence, not an author-owned file.
