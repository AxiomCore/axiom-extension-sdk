# Axiom Python extension SDK 0.1.0

This SDK describes the checked Python 3.12 portable authoring surface. The
source driver consumes the type stubs and lowers authored code ahead of time;
the SDK is not imported by a host Python interpreter at runtime.

Supported code is deterministic, has no ambient I/O, and can interact with the
application only through the generated `Context`, selectors, effect plans, and
atomic patches. Unsupported Python syntax fails at build time with a source
location. Exact third-party dependencies must be universal
`py3-none-any.whl` artifacts in `AxiomDeps.lock` and are parsed, never installed
or executed.
