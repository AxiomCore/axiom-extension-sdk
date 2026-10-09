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
