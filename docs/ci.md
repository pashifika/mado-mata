# Repository, controlled runtime, and desktop CI

## Install the pinned tools

Use Git, Python 3.11 or newer, Rustup, Node.js **24.18.0** with its bundled npm,
and a native C/C++ build toolchain. Install Node from its
[official release](https://nodejs.org/dist/v24.18.0/) and verify the release's
signed checksums. Rustup installs Rust **1.98.1** with the command below.
Linux needs a C compiler and linker; macOS needs Xcode Command Line Tools;
Windows needs the Visual Studio C++ build tools and Windows SDK for Rust's
MSVC target. The controlled runtime and desktop core do not require OCR models,
capture permissions, or a sibling engine checkout. Only macOS builds the Tauri
desktop shell; Linux and Windows check the frontend and shell-independent core.

The actionlint/lychee installer supports Linux x86_64/aarch64 and macOS
arm64/x86_64. It does not provide Windows assets; use a supported Linux
environment for the full check, or run the documented runtime/desktop-core mode
on Windows. This tooling limitation is not an application support decision.

The pinned binary-only Python requirements cover CPython 3.11 through 3.14.
Other interpreter versions need reviewed wheel hashes; do not fall back to an
unpinned source build when installation refuses them.

Use a Python virtual environment rather than modifying an externally managed
system installation. In a POSIX shell:

```sh
python3 -m venv .cache/repository-ci/venv
. .cache/repository-ci/venv/bin/activate
python3 -m pip install --require-hashes -r tools/ci/requirements.txt
python3 tools/ci/install_tools.py
rustup toolchain install 1.98.1 --profile minimal
python3 tools/ci/check.py
```

The explicit installer downloads the host's pinned native tools, verifies their
SHA-256 values, and installs them under ignored `.cache/repository-ci`.
Installation needs network access; the local-link check does not contact remote
sites. Do not replace the installer with an unpinned package-manager download or
skip integrity verification when a download fails.

The full check runs `npm ci --ignore-scripts --no-audit --no-fund` in the
application-owned compiler and desktop directories inside a temporary
tracked-file snapshot. It verifies Node's exact version and uses the committed
npm lockfiles. Compiler self-checks precede the Rust comparison checks; desktop
state tests, the frontend build, and Rust core tests follow the controlled suite.
macOS then builds the shell with `custom-protocol`, without `webdriver`.
Installation never runs a workload package's installer or dependency lifecycle
scripts; only the trusted application test/build scripts run explicitly.
Cargo and npm need access to public dependency sources on a fresh checkout;
only the Markdown link check is offline.

The version and integrity sources are:

| Dependency | Pin | Authoritative file |
| --- | --- | --- |
| actionlint | 1.7.12 | [toolchain.json](../tools/ci/toolchain.json), including host assets and SHA-256 values |
| lychee | 0.24.2 | [toolchain.json](../tools/ci/toolchain.json), including host assets and SHA-256 values |
| PyYAML | 6.0.3 | [requirements.txt](../tools/ci/requirements.txt), hash-pinned Python distributions |
| Rust | 1.98.1 | [check.py](../tools/ci/check.py), explicit `cargo +1.98.1` |
| Node.js | 24.18.0 | [check.py](../tools/ci/check.py) and [workflow](../.github/workflows/ci.yml) |
| TypeScript | 5.9.3 | [Compiler manifest](../tools/runtime-comparison/compiler/package.json) and [lockfile](../tools/runtime-comparison/compiler/package-lock.json) |
| Desktop frontend | React 19.3.0, TypeScript 5.9.3, Vite 8.3.0, Tauri API 2.11.1 / CLI 2.11.5 | [App manifest](../apps/desktop/package.json) and [lockfile](../apps/desktop/package-lock.json) |
| Desktop Rust | Tauri 2.11.6, tauri-build 2.6.3, tracing 0.1.41, tracing-subscriber 0.3.20 | [App Cargo manifest](../apps/desktop/src-tauri/Cargo.toml) and [lockfile](../apps/desktop/src-tauri/Cargo.lock) |
| GitHub Actions | Full commit SHAs | [Workflow](../.github/workflows/ci.yml) and [toolchain.json](../tools/ci/toolchain.json) |
| actions/upload-artifact | v7.0.1 (`043fb46d1a93c77aae656e7c1c64a875d1fc6a0a`) | [Stable release](https://github.com/actions/upload-artifact/releases/tag/v7.0.1), [tag commit](https://api.github.com/repos/actions/upload-artifact/git/ref/tags/v7.0.1), and [pinned inputs](https://github.com/actions/upload-artifact/blob/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/action.yml) |

CI uses Python 3.13, `ubuntu-24.04`, `macos-15`, and `windows-2025`. The macOS job
prints and requires `arm64`; runtime checks also print their actual host identity.
Update versions, integrity values, workflow references, and this guide together
through a checked Change. A new installer host needs a verified release asset.

## Local check scope

The complete local entrypoint is:

```sh
python3 tools/ci/check.py
```

It covers repository policy, workflow/local-link checks, governance regression
tests, real controlled runtime build/tests/compiler/CLI execution, desktop
frontend tests/build, and Rust application-core tests. On macOS it also compiles
the desktop shell. Repository inputs come from tracked product files, not
recursive discovery of ignored planning, sibling checkouts, installed authoring
packages, or extracted reference applications. New files must enter the intended
tracked change before that inventory covers them; never force-add private directories.

The full check has these responsibilities:

- Validate ruleset JSON and intended semantics, exact required-context agreement,
  action pins, read-only workflow permissions, and checkout credential removal.
- Check the canonical guide/symlink and reject tracked private planning or
  unintended machine-local files.
- Run actionlint against tracked workflows. `-shellcheck=` and `-pyflakes=`
  deliberately disable its optional external integrations rather than depending
  on unpinned executables; Actions syntax and expression checking remain in scope.
- Run lychee in offline mode for local Markdown targets and fragments. External
  website availability is not a merge prerequisite.
- Exercise accepted/refused branch routes, malformed metadata, repository-policy
  failures, and gate outcomes through behavioral tests.
- Install and check the trusted TypeScript compiler with package scripts disabled.
- Build and test the locked Rust comparison executable, then execute its
  controlled `check` suite for direct Rust, JavaScript, TypeScript, and Lua.
  These commands do not enable the optional `engine` feature.
- Install the locked desktop frontend with dependency lifecycle scripts disabled,
  run its state tests, and type-check/build its trusted UI.
- Test the Rust application core with `--no-default-features --lib`: profile
  persistence/validation, bounded logging, and the shared controller contracts
  are checked without loading a desktop shell or obtaining native authority.
- On macOS, build the real Tauri shell with `--features custom-protocol` after
  building frontend assets. The test-only `webdriver` feature is not enabled.
  Optional OCR settings, bounded replay projection, admission races, and
  pre-startup failures are checked without loading a real OCR backend. The
  separate engine artifact and actual [recorded-replay WebView acceptance](desktop.md#recorded-replay-acceptance)
  remain explicit local checks; no private corpus, model, or native permission
  is added to default CI.
  Linux and Windows explicitly report the shell build as unexecuted.

For governance policy and its behavioral tests only, after Python dependency
setup:

```sh
python3 tools/ci/check.py --policy-only
```

This does not run actionlint, lychee, Rust, Node, the TypeScript compiler, the
controlled executable, or any desktop checks. It is not full CI evidence.

For policy, governance tests, the controlled runtime, and desktop checks without
actionlint or lychee:

```sh
python3 tools/ci/check.py --runtime-only
```

On Windows, use `python` in place of `python3` and preserve the tracked symlink
as described in [development guidance](development-guidance.md#claude-symlink).
Hosted Windows enables Git symlink checkout before fetching the repository.
The two narrower modes are mutually exclusive. Missing tools, invalid input,
or failing commands remain failures; neither mode substitutes for the full gate.
These checks do not install remote rulesets or verify live GitHub enforcement.

## Controlled results and concise logs

Both the full check and `--runtime-only` capture the controlled `check` command's
complete stdout and stderr in the original checkout, outside the disposable
tracked-file snapshot:

| File | Contents |
| --- | --- |
| `.cache/repository-ci/runtime-results/runtime-results.json` | Full controlled command stdout, normally JSON case results |
| `.cache/repository-ci/runtime-results/runtime-stderr.log` | Full controlled command stderr |

The console shows total, passed, and failed case counts and the evidence
location instead of the raw JSON. Failure output includes bounded case IDs,
reasons, and critical diagnostics; use the saved files for the complete output.
Malformed or truncated JSON and nonzero command exits remain failures, with
their raw output preserved. Missing results cannot pass. Each runtime invocation
replaces earlier evidence so a failed attempt cannot reuse a previous result.
`--policy-only` does not run the controlled command or produce fresh runtime
evidence; any files from an earlier runtime invocation are not policy-only
evidence.
Desktop checks run after the controlled suite. A frontend/core/shell failure
still fails the job without discarding the already captured runtime evidence.
These two files are not GUI acceptance results or application-local logs.

The Linux, macOS arm64, and Windows jobs each attempt an artifact upload after
the controlled check with `always()`, including when an earlier step fails.
Artifacts are named `controlled-runtime-${{ runner.os }}-${{ runner.arch }}` and
retained for **7 days**. Downloads contain only `runtime-results.json` and
`runtime-stderr.log`. Hidden-file inclusion is explicit because their source
paths are under `.cache`; no directory, private planning files, or native
evidence is uploaded.

If an earlier prerequisite fails before evidence exists, the upload warns about
missing files without replacing the original failure. It does not make the
runtime job or mandatory gate pass. Other upload failures remain job failures.
These artifacts contain controlled results, not native qualification evidence;
artifact upload grants no native capture, OCR, game-launch, or input authority.

Retrieve the short-lived GitHub artifacts only when an investigation needs them.
Do not duplicate raw CI logs, JSON, or artifact ZIPs into private Rasen evidence,
and do not commit raw output to either the product or planning repository.
Record concise findings and the workflow run/artifact reference instead. This
retention policy does not remove historical evidence.

## Direct controlled commands

For iteration in the public working tree, install the trusted compiler and run
the same commands the local check uses:

```sh
npm ci --ignore-scripts --no-audit --no-fund --prefix tools/runtime-comparison/compiler
node tools/runtime-comparison/compiler/compile.mjs --self-check
cargo +1.98.1 build --locked --manifest-path tools/runtime-comparison/Cargo.toml
cargo +1.98.1 test --locked --manifest-path tools/runtime-comparison/Cargo.toml
cargo +1.98.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- check
```
The desktop checks in both full and `--runtime-only` modes additionally run:

```sh
npm ci --ignore-scripts --no-audit --no-fund --prefix apps/desktop
npm test --prefix apps/desktop
npm run build --prefix apps/desktop
cargo +1.98.1 test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --no-default-features --lib
```

On macOS only, after the frontend build:

```sh
cargo +1.98.1 build --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --features custom-protocol
```

By default the runtime and app builds use their respective Cargo `target`
directories. These checks compile the shell but do not launch its WebView.
The disposable snapshot and its binaries are removed after checks; follow the
[desktop guide](desktop.md#build-and-run-from-the-checkout) to build and run in
the working tree. A real run requires the fixed runtime build path there,
regardless of any `CARGO_TARGET_DIR` override used for independent CI builds.


`check` emits JSON evidence for the exercised controlled cases. The explicit
toolchain selector avoids dependence on the user's Rust default. To execute a
specific plan or summarize saved results, the CLI also accepts:

```sh
cargo +1.98.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- run <plan.json> <package-root>
cargo +1.98.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- report <results.json>
```

Replace the angle-bracket arguments with actual paths; they are not shell
redirections. Keep plans containing native paths and raw results private.
See the [runtime guide](runtime-comparison.md) for host and measurement contracts
and the [CI-first comparison report](runtime-comparison-results.md) for verified
scope and remaining qualification blockers.

## Hosted workflow and required gate

The [workflow](../.github/workflows/ci.yml) runs on PRs targeting `main` and
`dev/**`, protected-branch pushes, and manual dispatch. PR events include
`opened`, `synchronize`, `reopened`, `ready_for_review`, and `edited` so a base
change is rechecked. There are no workflow path filters. Manual dispatch becomes
available when the workflow is on the default branch.

The lightweight `dev-push-policy` job runs only on pushes. It suppresses duplicate
`dev/<topic>` push checks only when an open promotion PR to `main` has both its
head and base in this repository, with the exact pushed branch and commit SHA.
The [selector](../tools/ci/select_checks.py) uses paginated GitHub CLI lookup with
a 30-second timeout; invalid metadata, malformed responses, lookup failures, and
an unconfirmed match keep all checks enabled. Main pushes, PRs (including forks),
and manual dispatch never suppress checks or perform this lookup.

Only this selector job receives `pull-requests: read` alongside `contents: read`,
using the read-only `github.token` as `GH_TOKEN`. API responses and credentials
are not logged. Failure to write the required `GITHUB_OUTPUT` is a selector
failure, not a successful selection. A failed selector still allows the four
checks to attempt work, but cannot produce a passing gate.

The stable result is intentionally event-specific:

| Event | Gate check name | Required by branch rulesets |
| --- | --- | --- |
| Pull request | `CI Gate` | Yes |
| Push | `CI Gate (push)` | No |
| Manual dispatch | `CI Gate (manual)` | No |

Unless an exact-head promotion PR suppresses the duplicate push run, the `gate`
job runs with `always()` and needs the selector plus all four mandatory jobs:

| Job | Coverage |
| --- | --- |
| `branch-flow` | Event and branch-route validation |
| `repository` | Full local check, controlled runtime, desktop frontend/core on Linux |
| `runtime-macos` | Policy, governance tests, controlled runtime, desktop frontend/core, and shell build on Apple Silicon macOS |
| `runtime-windows` | Policy, governance tests, controlled runtime, desktop frontend/core on Windows |

Only success from every mandatory job passes. Failure, cancellation, missing
results, unexpected dependencies, and skipped mandatory work cannot produce a
successful gate. The selector must succeed or be skipped (as on PR/manual
events); its output does not excuse skipped mandatory results. There are no
optional lanes. On a confirmed duplicate push, the four jobs and push gate are
skipped, not reported as successful validation.

All four check names gain ` (push)` only on push events so their skipped statuses
cannot satisfy PR checks. A push/manual gate result on the same SHA cannot
replace the PR-required `CI Gate`. The aggregate reads `NEEDS_JSON` as data and
requires exactly the selector plus the four mandatory job IDs; keep that set
synchronized with the workflow when adding a lane.

Branch flow reads `GITHUB_EVENT_NAME`, `GITHUB_EVENT_PATH`, and
`GITHUB_REPOSITORY`; non-PR contexts also use `GITHUB_REF`. PR metadata is JSON
data, never shell source. Non-PR runs explicitly validate their event/ref context
instead of silently skipping the mandatory job. The accepted route policy is
owned by [CONTRIBUTING.md](../CONTRIBUTING.md#select-the-route-before-implementation).

The workflow uses read-only repository authority, credential-free checkout,
pinned actions/tools, bounded jobs, and event-scoped concurrency cancellation.
It does not use secrets, administration tokens, `pull_request_target`, private
Rasen access, or self-hosted interactive desktops. Superseding one PR run must
not cancel another PR's run or turn a cancellation into success.
Local macOS GUI acceptance is separate: exercise package selection, saved
profiles, actual runs, source errors, Stop, window closure, and logs using the
[desktop procedure](desktop.md#local-gui-acceptance). Hosted compilation does not
prove those interactions or additional-OS desktop support. Normal builds and
release builds have no WebDriver listener; CI does not enable the test-only
automation feature or launch a GUI session.

Administrative activation requires an observed successful PR check and its
GitHub Actions app identity; use the
[governance runbook](repository-governance.md#observe-the-pr-check-before-activation).
A source-level policy check is not proof that GitHub is enforcing the payload.

## M0 and native qualification

Hosted jobs exercise the real comparison executable with deterministic controlled
observations and a non-native input sink, plus the desktop checks above.
Cross-platform success is controlled evidence, not replay, native qualification,
GUI acceptance, desktop distribution, or runtime adoption. The optional `engine`
integration and its external prerequisites are separate from these commands; no
CI lane discovers windows, requests permission, captures the desktop, changes
focus, sends OS input, or operates a game.

M0 acceptance still requires explicitly authorized native Windows and Apple
Silicon macOS evidence for capture, template recognition, OCR, input, lifecycle,
and runtime adoption. Controlled success proves none of those native scenarios.
Missing authority, permissions, models, dependencies, or an OS lane leaves native
acceptance `BLOCKED` or `UNEXECUTED`, not passed. The qualification report remains
`Blocked(reason)` when neither candidate has the required native and budget
evidence. The [macOS-first development selection](adr/0001-runtime-comparison-boundaries.md#macos-first-development-selection)
allows manual development to proceed separately; it does not waive native
acceptance requirements or authorize hosted game operations.
