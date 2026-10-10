# Repository, controlled runtime, and desktop CI

## Install the pinned tools

Use Git, Python 3.11 or newer, Rustup, Node.js **24.18.0** with its bundled npm,
and a native C/C++ build toolchain. Install Node from its
[official release](https://nodejs.org/dist/v24.18.0/) and verify the release's
signed checksums. Rustup installs Rust **1.98.1** with the command below.
Linux needs a C compiler and linker; macOS needs Xcode Command Line Tools;
Windows needs the Visual Studio C++ build tools and Windows SDK for Rust's
MSVC target. The controlled runtime and desktop core do not require OCR models,
capture permissions, or a sibling engine checkout. macOS and Windows build the
Tauri desktop shell; Linux checks the frontend and shell-independent core.

The actionlint/lychee installer supports Linux x86_64/aarch64 and macOS
arm64/x86_64. It does not provide Windows assets; use a supported Linux
environment for the full check, or run the documented runtime/desktop mode on
Windows. This tooling limitation is not native GUI acceptance.

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
macOS and Windows then build the shell with `custom-protocol`, without `webdriver`.
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
| Node.js | 24.18.0 | [check.py](../tools/ci/check.py) and [workflows](#hosted-workflow-and-required-gate) |
| TypeScript | 5.9.3 | [Compiler manifest](../tools/runtime-comparison/compiler/package.json) and [lockfile](../tools/runtime-comparison/compiler/package-lock.json) |
| Desktop frontend | React 19.3.0, TypeScript 5.9.3, Vite 8.3.0, Tauri API 2.11.1 / CLI 2.11.5 | [App manifest](../apps/desktop/package.json) and [lockfile](../apps/desktop/package-lock.json) |
| Script editor | CodeMirror state 6.7.6, view 6.43.13, language 6.12.4, JavaScript 6.2.5, autocomplete 6.20.3, commands 6.11.1; existing TypeScript 5.9.3 language service | Exact [frontend manifest](../apps/desktop/package.json) and [lockfile](../apps/desktop/package-lock.json); trusted ES2020 declarations are bundled by the [build helper](../apps/desktop/build/trusted-libraries.mjs), with no runtime downloads |
| Desktop Rust | Tauri 2.11.6, tauri-build 2.6.3, tracing 0.1.41, tracing-subscriber 0.3.20 | [App Cargo manifest](../apps/desktop/src-tauri/Cargo.toml) and [lockfile](../apps/desktop/src-tauri/Cargo.lock) |
| Saved-image payloads | png 0.18.1; flate2 1.1.9 (default features disabled; `rust_backend`) | [Runtime Cargo manifest](../tools/runtime-comparison/Cargo.toml) and [lockfile](../tools/runtime-comparison/Cargo.lock) |
| Desktop configuration | zip 8.6.0 (default features disabled; `deflate-flate2` for reviewed runtime archives), unicode-normalization 0.1.25, plist 1.10.1 (default features disabled; pinned streaming API feature) | [App Cargo manifest](../apps/desktop/src-tauri/Cargo.toml) and [lockfile](../apps/desktop/src-tauri/Cargo.lock) |
| Desktop OCR acquisition | ureq 3.4.2 with only `rustls`; rustls 0.23.45 in the lockfile; tar 0.4.46 without default features; flate2 1.1.9 with only `rust_backend` | [App Cargo manifest](../apps/desktop/src-tauri/Cargo.toml) and [lockfile](../apps/desktop/src-tauri/Cargo.lock); fixed HTTPS sources, bounded transfers/selected-member extraction, exact hashes, retained notices, no shell or installer |
| Desktop persisted identifiers | Public `pashifika/xid-rs` Git dependency at the immutable `rev` in the [App Cargo manifest](../apps/desktop/src-tauri/Cargo.toml), repeated in its [lockfile](../apps/desktop/src-tauri/Cargo.lock); default features disabled | The public checkout fetches the Fork directly; no maintenance checkout or local path override is required |
| macOS application metadata, picker, and clipboard | objc2 0.6.4, block2 0.6.2; objc2-foundation, objc2-app-kit, objc2-core-foundation, objc2-security, objc2-uniform-type-identifiers 0.3.2 | macOS-target-scoped exact pins in the [App Cargo manifest](../apps/desktop/src-tauri/Cargo.toml) and [lockfile](../apps/desktop/src-tauri/Cargo.lock) |
| Application launch library | libc 0.2.189 on Unix; objc2 0.6.4, block2 0.6.2 and objc2-foundation/objc2-app-kit 0.3.2 on macOS | [Library manifest](../crates/application-launch/Cargo.toml) and [lockfile](../crates/application-launch/Cargo.lock); no Desktop or runtime dependency |
| Shared supervisor/child monotonic deadline | libc 0.2.189 on Unix; windows-sys 0.61.2 with `Win32_System_Performance` on Windows | Default target-scoped dependencies in the [Runtime Cargo manifest](../tools/runtime-comparison/Cargo.toml) and [lockfile](../tools/runtime-comparison/Cargo.lock); absolute boot-clock transport does not refund child startup |
| macOS engine startup lifetime guard | objc2 0.6.4, objc2-foundation/objc2-app-kit 0.3.2; shared Unix libc pin above | Optional Objective-C `engine` dependencies in the [Runtime Cargo manifest](../tools/runtime-comparison/Cargo.toml) and [lockfile](../tools/runtime-comparison/Cargo.lock); read-only selected-process checks, not a capture/input implementation |
| GitHub Actions | Full commit SHAs | [Workflows](#hosted-workflow-and-required-gate) and [toolchain.json](../tools/ci/toolchain.json) |
| actions/upload-artifact | v7.0.1 (`043fb46d1a93c77aae656e7c1c64a875d1fc6a0a`) | [Stable release](https://github.com/actions/upload-artifact/releases/tag/v7.0.1), [tag commit](https://api.github.com/repos/actions/upload-artifact/git/ref/tags/v7.0.1), and [pinned inputs](https://github.com/actions/upload-artifact/blob/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/action.yml) |

CI uses Python 3.13, `ubuntu-24.04`, `macos-15`, and `windows-2025`. The macOS job
prints and requires `arm64`; runtime checks also print their actual host identity.
Update versions, integrity values, workflow references, and this guide together
through a checked Change. A new installer host needs a verified release asset.

Target metadata parsing uses `plist` in-process, with the exactly pinned
`enable_unstable_features_that_may_break_with_minor_version_bumps` feature for
bounded XML/binary streaming. No system plist helper is executed. macOS core
checks exercise public Foundation bundle resolution with isolated filesystem
fixtures, including same-process metadata updates and unsupported alternate
metadata. They also check the current test process's kernel architecture and
invalid-PID refusal without launching an application. Non-macOS bundle resolution
is explicitly unsupported; portable declaration, record, restore, and
observation-policy checks remain cross-platform.
Apple framework dependencies are macOS-target-scoped. Native selection uses
host-owned AppKit sheets for application bundles or saved PNGs, not a general
dialog/filesystem plugin capability. Recognition Copy uses `NSPasteboard` only
on explicit request. Actual macOS WebView selection, clipboard publication, and
authorized running-application observation remain separate from hosted checks
and grant no native execution authority.

The desktop crate denies `unsafe_code` and `unsafe_op_in_unsafe_fn` by default.
Audited native FFI uses narrowly scoped `#[expect(unsafe_code)]` with a reason
and documented safety conditions; no crate-wide warning suppression is used.
The runtime's Windows file-identity FFI follows the same reason-bearing annotation
convention at the function boundary; its crate-wide unsafe-code warning remains
enabled.
Platform-only test imports use `#[cfg]`; intentional cross-platform mutability
and native discovery outcomes use reason-bearing `#[cfg_attr(..., expect(...))]`
only for builds where their Unix/macOS consumers are absent. Global unused-code
warnings remain enabled.

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
- Run the checkout-owned OMP adapter tests without installing OMP or private
  plugins. These exercise registration/schema compatibility, explicit instance/
  owner selection, cancellation, session teardown, notice continuation and
  unknown-outcome reconciliation. The compatibility floor is OMP `18.8.7`, not
  an exact pin or upper bound; simulated API/version cases are not evidence
  that an installed future release works.
- Run the shared SDK catalog unit tests. Runtime `sdk_examples` tests discover
  the public catalog, compile examples with the pinned trusted compiler under
  empty and representative option schemas, and execute applicable behavior on
  the real controlled host. They cover no-match/backend-fault distinctions,
  finite waits, retained-handle release and stage/lane refusals. Native-only
  examples must refuse in the controlled lane, not acquire native authority.
  Separate [Engine publication regressions](runtime-native.md#consumer-publication-regressions)
  cover Engine-specific resource ownership after installing its native build
  dependencies; they are not part of the ordinary CI command.
- Test the independent [application-launch library](../crates/application-launch/Cargo.toml)
  and its literal argument/cwd, refusal and external-child ownership contracts.
  These checks do not launch a game or authorize bundle/native acceptance.
- Build and test the locked Rust comparison executable, then execute its
  controlled `check` suite for direct Rust, JavaScript, TypeScript, and Lua.
  Image regressions cover full PNG validation, original-pixel crops, split
  package quotas, decoded/payload bounds, recognition metadata/maps, and generated
  SDK snippets. These commands do not enable the optional `engine` feature or
  execute real OCR.
  External CLI plans refuse Desktop-only reviewed input authority and phase
  budgets before package/output I/O. Real owned-child regressions cover
  Script-requested startup, no-request cleanup, one-shot/polled preparation,
  typed startup failures, and independent phase deadlines versus explicit Stop.
  Those child fixtures run serially so the CPU-expiry case cannot starve unrelated
  startup assertions; their phase deadlines and protocol checks remain unchanged.
  Submitted receipts and physical cleanup ownership remain separate outcomes.
- Install the locked desktop frontend with dependency lifecycle scripts disabled,
  run its state tests (including per-file history, stale saves, source-diagnostic
  projection, atomic literal replacement, UTF-16/line-ending mapping, real
  restricted SDK/options completion, current-source session refresh and Backspace,
  saved completion preferences, stale request/acceptance fencing, schema
  invalidation, worker fencing and finite failure/explicit retry, Recognition
  geometry/Undo, grouped selection, stale trial/Copy state, and per-Start Native
  consent invalidation), and
  type-check/build its trusted UI and bundled language worker.
  Shared-authoring tests also cover versioned unsaved reads, original-UTF-16
  batch edits, ABA/IME refusal, structured-field provenance, chronological
  Undo, snippet dependencies, publication prefixes and lifecycle resolution.
  These deterministic checks do not prove physical OS IME or editor interaction.
- Test the Rust application core with `--no-default-features --lib`: explicit
  setup/recovery, named Tab ownership, scoped profiles, source-preserving
  historical-layout imports, bounded snapshots, and journaled restore/rollback
  are checked alongside bounded logging and shared controller contracts. Log-content,
  initialization-error and successful receipt-restore assertions join the real
  writer independently of the production shutdown deadline; a separate stalled-worker
  test checks the bounded shutdown and incomplete-cleanup outcome.
  Command-retirement checks accept settled logging or an explicit `LoggingShutdown`
  refusal with retirement and closed admission; they do not assume disk-sync latency.
  Identifier cutover checks must cover canonical-XID lifecycle and unchanged
  ownership, effect-free old active-ID refusal, source-preserving current-XID
  Import/repeat/conflicts and durable-subset reporting. Snapshot/Restore checks
  must cover inactive unassigned originals, opaque ledger bytes or absence,
  exact preimage rollback, supported version-3 recovery, and untouched refusal
  of old/unknown journals, completion-only markers and pending-ledger writes.
  Historical-root staging keeps the managed-path byte budget even when an inert
  source is named `settings.json`. Recovery UI checks distinguish unsupported
  protocols from current-transaction validation failures and preserve repair/Retry.
  OCR resource tests validate the embedded catalog offline, exact-byte model/runtime
  publication, TAR/ZIP extraction bounds, cancellation, filesystem failures,
  staging ownership/file locks, snapshot exclusion, and setup admission without
  turning a temporary busy state into configuration Recovery. Native discovery
  tests exercise bounded Mach-O/PE dependency closure, explicit-root containment,
  loader precedence, ambiguity, missing dependencies and incompatible headers.
  Frontend tests cover independent automatic draft selection, later OCR edits,
  unrelated edits, closed dialogs, picker/cancellation fences and explicit Save.
  Live source hosts, OS folder selection, native link/clipboard behavior and real
  engine OCR remain separate local smoke/acceptance work.
  Completion-preference regressions cover old-file defaults without rewriting,
  strict object/range refusal, atomic write failure, Edit/busy/restore admission,
  and configuration restore/restart. Restore/Import regressions exercise
  discoverable cleanup after restart, not physical power loss. The
  [configuration recovery ADR](adr/0005-desktop-configuration-recovery.md) records
  the storage boundaries and Windows directory-sync qualification limitation.
  Optional OCR settings, bounded replay projection, admission races, and
  pre-startup failures are checked without loading a real OCR backend.
  Native intent/unknown-field rejection, separate launch approval, saved target
  expectations, immutable prelaunch preflight, typed discovery/progress,
  one-launch status probes, launch-admission cancellation and sticky cleanup
  refusal are checked without game launch or native capture/input. Optional
  engine-feature publication, exact-lifetime/window probes and Readiness input
  gating regressions require the separate
  [native build prerequisites](runtime-native.md#consumer-publication-regressions).
  Directory-authoring regressions cover configured ID-only destinations, source
  ownership, configuration-only preservation of `sources` and `pkgs`, snapshot
  source exclusion, revision conflicts, interrupted publication, global Edit
  admission, non-evaluating validation and bounded close/cleanup. Recognition
  regressions cover crop-only publication, retained asset references, source
  conflicts, frame replacement, and confirmation. Frontend checks cover
  structured metadata round trips and numeric draft provenance. Actual
  [Edit WebView acceptance](desktop.md#directory-package-authoring-acceptance),
  including source highlighting/completion, native clipboard/menu Undo/Redo,
  left-tree navigation and contextual file actions, physical OS IME input,
  worker startup/warm-response/memory observations, and storage power-loss
  durability are not hosted CI claims.
  Unix builds additionally exercise private collaboration endpoints with real
  owned sockets, bounded framing/queues, cancellation, owner fencing and
  shutdown. Non-Unix builds do not provide the collaboration transport.
  Recognition generation tests use owned fixtures; no clipboard scraping,
  native acquisition, OCR initialization or model request is authorized.
- On macOS and Windows, build the real Tauri shell with `--features custom-protocol`
  after building frontend assets. Windows also runs non-GUI shell validation and
  real owned-process/Job lifetime regressions; the test-only `webdriver` feature
  is not enabled. Linux reports the shell build as unexecuted.
  The separate engine artifact, actual
  [recorded-replay WebView acceptance](desktop.md#recorded-replay-acceptance),
  [saved-image Recognition acceptance](desktop.md#saved-image-recognition-acceptance)
  and [native authoring acceptance](desktop.md#acquire-a-native-historical-frame)
  remain explicit local checks. No private target, corpus, model, or native
  permission is added to default CI. These checks do not prove native picker or
  clipboard interaction, recognition quality, physical pointer behavior,
  capture-session cleanup under a real backend, or total RSS.

For focused adapter and SDK checks after the pinned compiler setup, run from
the product root:

```sh
npm test --prefix integrations/omp
node --test tools/runtime-comparison/compiler/sdk.test.mjs
cargo +1.98.1 test --locked --manifest-path tools/runtime-comparison/Cargo.toml --test sdk_examples
```

The full and runtime-only entrypoints include these scopes alongside desktop
tests. Actual installed-OMP loading and communication with a macOS WebView
remain separate [local observations](desktop.md#observed-compatibility-boundary),
not hosted-CI claims. Adapter installation and removal belong to the
[desktop guide](desktop.md#install-select-and-remove-the-adapter).

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

Desktop `0.2.0` marks the first rejecting development version, not a runtime
version or saved-schema bump. The retained converter checkout is
`824d1b7bd001efb025e53a3becb9e5521af677cf`, tree
`090c1d9fed7c6470ef31548060a629086fdf9783`; its recorded successful
[CI run](https://github.com/pashifika/mado-mata/actions/runs/37592403434) is baseline
evidence, not acceptance of the rejecting version. No converter release/tag
identified the boundary; `0.1.0` alone is not a converter guarantee.
Use the [two-checkout procedure](desktop.md#development-transition-notes), each
checkout's pinned installation guide, separate frontend/compiler dependencies
and shell outputs, and each checkout's fixed controlled-runner path. Record
revision/tree and executable hashes plus source/archive comparisons privately.
The root and backup upgrade routes, old pending-operation settlement,
owner-selected Import, and actual
[cutover WebView acceptance](desktop.md#identifier-cutover-acceptance) remain
separate local obligations. Neither hosted success nor the presence of these
instructions claims that local acceptance has passed.

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

Hosted checks use three event-exclusive workflows:

- [CI](../.github/workflows/ci.yml): PRs targeting `main` and `dev/**`.
- [CI (push)](../.github/workflows/ci-push.yml): pushes to `main` and `dev/**`.
- [CI (manual)](../.github/workflows/ci-manual.yml): manual dispatch.

PR events are limited to `opened`, `synchronize`, `reopened`, and
`ready_for_review`. The PR workflow does
not subscribe to `edited`: changing a PR title, description, or task-list
checkbox creates no new CI workflow run or skipped checks. A job-level `if`
would only skip jobs after a workflow run already exists, so it is not a
substitute for removing the event subscription. There are no workflow path
filters. Manual dispatch becomes available when the workflow is on the default
branch. GitHub documents activity-type filtering in
[Events that trigger workflows](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#pull_request).

A base-branch retarget is also an `edited` activity and no longer automatically
revalidates the PR. After changing the base, the maintainer must close and reopen
the PR, or push a new commit to its head branch, and confirm that the resulting
PR run validates the new route and passes `CI Gate` before merging. Resolve any
merge conflict first. Do not use an earlier run's success as evidence for the new
base: rerunning an old workflow uses the original event's SHA/ref, and manual
dispatch produces `CI Gate (manual)`, not the required PR check. See
[Re-running workflows and jobs](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/re-run-workflows-and-jobs).
This is an explicit maintainer step, not automatic server-side retarget
protection; branch rulesets and the required `CI Gate` context remain unchanged.

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
| `runtime-windows` | Policy, governance tests, controlled runtime, desktop frontend/core, Windows shell build and non-GUI shell/owned-process contracts |

Only success from every mandatory job passes. Failure, cancellation, missing
results, unexpected dependencies, and skipped mandatory work cannot produce a
successful gate. The selector must succeed or be skipped (as on PR/manual
events); its output does not excuse skipped mandatory results. There are no
optional lanes. On a confirmed duplicate push, the four jobs and push gate are
skipped, not reported as successful validation.

All job display names are literal, including skipped jobs. GitHub can expose
unevaluated name expressions when a job is skipped. Ordinary job names omit
event suffixes because GitHub already labels the event in the check display.
The aggregate gate names remain event-specific.
Event subscriptions, rather than job-level conditions, isolate the gate names:
a push/manual run cannot publish the PR-required `CI Gate`, even as a skipped
check. Policy validates all three workflows, including their event, gate
context, permissions, pinned commands and mandatory dependencies.

The aggregate reads `NEEDS_JSON` as data and requires exactly the selector plus
the four mandatory job IDs; keep that set synchronized across all three
workflows when adding a lane.

Branch flow reads `GITHUB_EVENT_NAME`, `GITHUB_EVENT_PATH`, and
`GITHUB_REPOSITORY`; non-PR contexts also use `GITHUB_REF`. PR metadata is JSON
data, never shell source. Non-PR runs explicitly validate their event/ref context
instead of silently skipping the mandatory job. The accepted route policy is
owned by [CONTRIBUTING.md](../CONTRIBUTING.md#select-the-route-before-implementation).

The workflows use read-only repository authority, credential-free checkout,
pinned actions/tools, bounded jobs, and event-scoped concurrency cancellation.
PR metadata edits create no run and therefore cannot cancel or supersede running
or pending validation. New commits still supersede older runs for the same PR.
They do not use secrets, administration tokens, `pull_request_target`, private
Rasen access, or self-hosted interactive desktops. Superseding one PR run must
not cancel another PR's run or turn a cancellation into success.
Local macOS GUI acceptance is separate: exercise Loading, Setup, Recovery, named
Tabs, current-XID historical-layout imports, snapshot/restore confirmation,
package selection, scoped profiles, actual runs, source errors, Stop, window
closure, and logs using the [desktop procedure](desktop.md#local-gui-acceptance).
Hosted compilation and core
tests do not prove those interactions or additional-OS desktop support. A passed
local setup smoke does not pass the remaining GUI scenarios, full CI, or native
qualification. Normal builds and release builds have no WebDriver listener; CI
does not enable the test-only automation feature or launch a GUI session.
Native review/refusal can be exercised without capture or input. Useful Native
workflow, actual game effect and normal Native Stop/rerun require the separate
[authorized Desktop procedure](desktop.md#reviewed-macos-native-start).

Administrative activation requires an observed successful PR check and its
GitHub Actions app identity; use the
[governance runbook](repository-governance.md#observe-the-pr-check-before-activation).
A source-level policy check is not proof that GitHub is enforcing the payload.

## M0 and native qualification

Hosted jobs exercise the real comparison executable with deterministic controlled
observations and a non-native input sink, plus the desktop checks above.
Cross-platform success is controlled evidence, not replay, native qualification,
GUI acceptance, desktop distribution, or runtime adoption. Real engine
integration and its external prerequisites remain separate from hosted checks;
no CI lane discovers windows, requests permission, captures the desktop, changes
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
