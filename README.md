# Axiom extension SDKs

Public, independently versioned authoring SDKs for Axiom extensions. This
repository is the source of truth for the SDK packages; the Axiom CLI contains
the compiler and binding generator, not a copy of these SDK sources.

| Language | Package | Source | Registry |
| --- | --- | --- | --- |
| Rust ABI | `axiom-extension-abi` | [`rust/abi`](rust/abi) | crates.io |
| Rust derive | `axiom-extension-sdk-derive` | [`rust/sdk-derive`](rust/sdk-derive) | crates.io |
| Rust SDK | `axiom-extension-sdk` | [`rust/sdk`](rust/sdk) | crates.io |
| TypeScript | `@axiomcore/extension-sdk` | [`typescript`](typescript) | npm |
| Python | `axiom-extension-sdk` | [`python`](python) | PyPI |

The packages are **not yet published**. Until the first coordinated release,
install from an explicit local checkout for development; do not imply that
`0.1.0` is already available from a registry. A release first publishes the
Rust ABI and derive crates, then the Rust SDK, and can publish the independent
TypeScript and Python packages. A release operator must verify all package
registries and mirror the public archives into AxiomCore Releases before
considering the release complete.

## Project setup

Extension projects own ordinary package-manager manifests and lockfiles. This
lets rust-analyzer, TypeScript, Pyright, and the Axiom source compiler resolve
the **same installed SDK**. `AxiomDeps.toml` still declares Axiom authority,
targets, exports, and any separately approved authored dependencies.

- Rust: add `axiom-extension-sdk = "=0.1.0"` to project `Cargo.toml` and
  commit `Cargo.lock`. Before publication, use a local `path` dependency in a
  development-only project. The Axiom compiler resolves its exact package
  root through `cargo metadata --locked --offline`.
- TypeScript: add `@axiomcore/extension-sdk` to `package.json`, commit the
  lockfile, and run `npm ci`. Before publication, use an explicit local
  package path. Run the SDK's `npm run build` before linking it locally.
- Python: add `axiom-extension-sdk==0.1.0` to the project's Python dependency
  file and install it into project `.venv`. Before publication, install the
  local `python/` package. The `.pyi` and `py.typed` files serve IDE typing;
  Axiom's compiler still statically lowers the authored Python source.

Before the first publication, use the setup helper with this repository's
local path. For example, from this checkout:

```sh
python3.12 scripts/setup_project.py ../examples/axiom-shopping-app/frontend pricing \
  --sdk-root . --cli laxiom
python3.12 scripts/setup_project.py ../examples/axiom-shopping-app/frontend pricing \
  --check --cli laxiom
```

The helper installs the package, records the project lockfile, and
materializes editor bindings. Rust gets an `.axiom/ide/<alias>` Cargo example
with a crate-root `axiom_bindings` module; TypeScript gets a generated
`@axiomcore/extension-bindings` declaration package; Python gets
`axiom_bindings.pyi`. Run `--check` in project CI and rerun setup whenever
`AxiomDeps.toml` or a relevant `.acore` state declaration changes.

For a local `.acore` state such as `state cart { subtotal_cents: Int = 1 }`,
the permission-scoped Rust binding contains `ui::Cart`, and `Cart` exposes a
`subtotal_cents_typed() -> Result<u64>` method alongside its generic adapter.
TypeScript and Python selectors carry `number`/`int`, `string`/`str`, or
`boolean`/`bool` types for supported scalar fields. A state scope supplied by
another host, or a field type outside this scalar subset, stays generic; the
helper never invents types or runtime authority for it.

The CLI's `axiom extensions {rust,typescript,python}-inspect` commands expose
the exact generated bindings for each extension. Keep generated IDE bindings
out of the committed source tree unless a project intentionally checks in a
reproducible generated snapshot.

## Build and release

Run `bash scripts/build.sh` to test all packages and create local package
archives. The GitHub Actions validation workflow runs the same checks. The
manual release workflow requires a protected `package-release` GitHub
environment, a `CARGO_REGISTRY_TOKEN` secret, an npm trusted publisher for
`.github/workflows/release.yml`, a PyPI trusted publisher for the same workflow
and environment, and an `AXIOMCORE_RELEASE_ASSET_TOKEN` with contents-write
permission on `AxiomCore/AxiomCore`. Configure each registry identity before
its first release. No push to this repository automatically publishes a package.

SDK APIs carry no ambient network, filesystem, process, clock, random, or
native-addon authority. Runtime permissions are still enforced by Axiom's
separate host capability system.
