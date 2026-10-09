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
