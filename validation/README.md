# Local validation and release

Builds, checks and artifact uploads are owned by the local release cycle.
GitHub Actions is reserved for SDK publication; push, pull-request, scheduled
and release-tag CI jobs are disabled.

Run release operations from the sibling private `Axiom-release-plane` checkout:

```sh
just release containers
just release web
```

Useful repository checks:

```sh
cargo test --workspace --locked
(cd typescript && npm ci --ignore-scripts && npm test)
python3.12 -B -m unittest discover -s scripts -p "test_*.py"
python3.12 -B -m unittest discover -s validation -p "test_*.py"
uv build python --out-dir /tmp/axiom-extension-release-archives --no-create-gitignore
```

`former-github-actions/` preserves the previous command recipes for reference;
these files are outside GitHub's workflow directory and cannot start Actions.
Setup steps and GitHub expressions in those recipes are historical, not a
local execution interface. Use the commands above and the controller's operator guide. Native/editor acceptance on this Mac covers macOS ARM64;
Windows and other host-platform qualification must run on those platforms.
Upstream Lynx qualification remains available through the private controller's
qualification scripts when evaluating a new engine version.

The SDK publication workflow retained under `.github/workflows/` keeps its
existing repository identity, protected publication environment and OIDC trust.
Package archives and inspection reports stay under `RUNNER_TEMP` in every job.
Downloading reviewed packages therefore preserves the clean committed checkout
required by the publisher; tracked edits and unexpected source files still fail.
