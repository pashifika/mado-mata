# Agent instructions

## Current scope

MadoMata contains a controlled macOS desktop application and the standalone M0
runtime comparison. The desktop uses the existing supervised QuickJS/TypeScript
runner with a non-native input sink. Follow [the desktop guide](docs/desktop.md)
for real checkout build/run commands and [the comparison guide](docs/runtime-comparison.md)
for the independent CLI. Native qualification, R6, runtime adoption, release
packaging, and additional-OS desktop support remain unresolved. Do not invent
game-launch commands or treat controlled results as native acceptance.

## Setup and verification

Run commands from the product repository root. Follow the pinned installation
procedure in [docs/ci.md](docs/ci.md) before running the full check, which includes
the desktop frontend/core and the macOS shell build on macOS:

```sh
python3 tools/ci/check.py
```

For repository policy and behavioral checks without external native tools:

```sh
python3 tools/ci/check.py --policy-only
```

The policy-only result is not a substitute for the full check. Public checks
must work without private planning, sibling checkouts, or personal skill paths.
Report the command, outcome, and unexecuted scope; never convert a missing
prerequisite or skipped native scenario into a pass.
Actual GUI acceptance is a separate local macOS procedure in the desktop guide;
hosted builds do not verify WebView interaction or satisfy missing native/replay
prerequisites.

## Change discipline

- Read the relevant source and existing conventions before editing. Keep work
  within approved scope; fix the underlying contract instead of hiding failures.
- Record the Change or issue, integration topic, and PR base before implementation.
  Ordinary work follows `change/` or `fix/` into `dev/<topic>`, then `main`.
  Follow [CONTRIBUTING.md](CONTRIBUTING.md) for checked emergency and sync routes.
- Preserve unrelated work. Do not reset shared history, force-push protected
  branches, bypass checks, or change live repository settings without explicit
  administrative authorization.
- Keep public documentation in English. Update real command guidance and CI in
  the same delivery that introduces executable application code.
- Add behavioral regression coverage where a plausible failure warrants it;
  do not assert source wording or add dummy applications as verification.
- Keep `CLAUDE.md` as a relative symlink to `AGENTS.md`. Put detailed procedures
  in their owning document, not another independently maintained agent manual.

## Native authority and privacy

- Native capture, OCR, game launch, and input require an explicitly authorized
  target, environment, and operation. Repository CI grants none of that authority.
  Do not prompt for permissions, elevate, change focus, or widen an input route
  merely to obtain a passing result.
- Preserve wrong-target refusal, cancellation, and incomplete-cleanup outcomes.
  Do not claim input submission proves application effect or that Stop proves
  physical cleanup has completed.
- Initial M0 qualification still requires Windows and macOS evidence. Hosted
  checks do not replace it; later primary-OS iteration is a separate policy.
- Keep credentials, local game/model paths, private screenshots, execution
  evidence, and machine-specific configuration out of public commits and logs.
  Do not force-add ignored directories.
- `rasen/` is an independent private planning Git repository. Never stage its
  files through the product repository or change its upstream for product work.

## Further guidance

- [Contributor workflow](CONTRIBUTING.md): branches, PRs, verification, retirement.
- [CI](docs/ci.md): installation, check scope, stable gate, native limitations.
- [Desktop](docs/desktop.md): pinned checkout build/run, profiles, logs, local GUI acceptance.
- [Repository governance](docs/repository-governance.md): intended settings,
  explicit administrative installation, readback, and recovery.
- [Development guidance](docs/development-guidance.md): instruction ownership,
  root-launched OMP discovery, and maintainer-only planning setup.
