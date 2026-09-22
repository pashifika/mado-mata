# M0 runtime comparison

This standalone executable compares JavaScript, Lua, TypeScript authoring into the
JavaScript VM, and an independent direct-Rust workload. The separate
[macOS desktop application](desktop.md) reuses its supervised runner with
controlled and optional recorded-replay lanes; the CLI remains independently
usable. Controlled checks exercise
actual interpreters and owned processes; their input sink is **non-native**.
They do not capture a screen, launch a game, change focus, request permissions,
or send operating-system input.

**macOS development uses JavaScript on QuickJS with TypeScript authoring.**
The [selection decision](adr/0001-runtime-comparison-boundaries.md#macos-first-development-selection)
allows manual development before full both-OS qualification. Native qualification
remains incomplete; invocation-only receipts do not prove application effect.
See [native prerequisites](runtime-native.md) before any real capture or input.

## Setup and commands

Run from the repository root with Rust **1.98.1** and Node.js **24.18.0**. The
controlled build needs a C toolchain for the vendored interpreters, but no OpenCV,
ONNX models, sibling checkout, private planning repository, or game installation.

```sh
npm ci --ignore-scripts --no-audit --no-fund --prefix tools/runtime-comparison/compiler
node tools/runtime-comparison/compiler/compile.mjs --self-check
cargo +1.98.1 build --locked --manifest-path tools/runtime-comparison/Cargo.toml
cargo +1.98.1 test --locked --manifest-path tools/runtime-comparison/Cargo.toml
cargo +1.98.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- check
```

`check` executes the built-in behavioral corpus and prints versioned JSON. Its
exit status reflects the controlled oracles, including expected refusals and
forced/incomplete cleanup. A passing controlled check is not runtime adoption.
Compiler checks include actual strict diagnostics and LanguageService completion
results, not source-text assertions. [Repository CI](ci.md) invokes these checks
from a tracked-only snapshot on Linux, Apple Silicon macOS, and Windows x64.
The same full/runtime-only entrypoints also check desktop frontend state/build
and the Rust application core on all three hosts, then compile the shell on
macOS only. Those checks do not launch the GUI or qualify another desktop OS.

A bounded, non-native example plan is provided:

```sh
mkdir -p .cache/runtime-comparison
cargo +1.98.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- run tools/runtime-comparison/fixtures/controlled-plan.json tools/runtime-comparison/fixtures/javascript > .cache/runtime-comparison/sample.json
cargo +1.98.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- report .cache/runtime-comparison/sample.json
```

Use a private copy of the plan to select `rust`, `javascript`, `lua`, or
`typescript`, with the corresponding fixture directory (`rust` uses the
JavaScript package's common data). Select `template-first` or `ocr-first`; their
independent expected keys are `A` and `D`. Declare budgets **before** sampling.
The example budgets are controlled-run ceilings, not native qualification budgets.
Only the predeclared controlled scenarios are accepted; misspellings cannot
silently become successful samples.

`child`, `parent-probe`, `parent-stop-probe`, and `target-probe` are harness-internal
modes. The last is an inert owned process used to prove that containment and
intentional supervisor exit do not terminate the separate target.

## Manual script testing

After the setup above, start one controlled TypeScript invocation:

```sh
mkdir -p .cache/runtime-comparison
cargo +1.98.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- manual tools/runtime-comparison/fixtures/manual-plan.json tools/runtime-comparison/fixtures/typescript .cache/runtime-comparison/manual-result.json
```

Review the candidate, lane, and limits, then enter `start`. While it runs, enter
`stop` to request cancellation. Closing the control input also requests Stop.
The supervisor keeps its normal cleanup and containment deadlines; a Stop request
is not a physical-cleanup confirmation. An invocation that finishes first retains
its actual outcome. Use the reported cleanup fields, not the prompt, to determine
whether cleanup completed.

Manual mode executes exactly once without warmups or repetitions. It does not
expand native authority or retry failed input. It captures an immutable package
before Start; edit the source and invoke the command again for another test.
The destination must not already exist and must be outside the package; choose
a new filename for each invocation. Results use the same private version-1
`runs` format as `run`, and can be summarized with `report`.
Declining Start saves an empty `runs` array with `CancelledBeforeStart`, not an
executed run. Only a completed passing invocation exits 0; cancellation and
failed or blocked invocations exit 1 while preserving their distinct outcomes.

The shipped plan has a **non-native controlled input sink**. It does not operate
a game or need an OCR installation. Use an engine-enabled executable and a
separately reviewed private plan/package for real replay or native testing.
Confirming Start for a native plan authorizes only that plan's exact target,
route, and finite operation limits, not automatic focus, elevation, or fallback.
Scripts must evaluate their postconditions explicitly; `Submitted` alone is not
application success. Raw result files can contain private execution details.
For the package/profile GUI, use the [desktop build and run guide](desktop.md).
It retains app-local named profiles separately from package source and captures
one immutable run at Start. Draft edits and later profile saves affect only a
subsequent run. Package/schema mismatches are refused without migrating stored
data, and Stop/cleanup remain independent of the bounded GUI and file logs.
The shell uses fixed checkout-owned runner/compiler paths, not a release bundle.

## Package and host contracts

A package declares every source, profile, schema, asset, and source map in
`package.json`. Use the shipped fixtures as executable examples. Snapshot capture
rejects undeclared files, missing assets, traversal, nonportable/colliding IDs,
links inside the package, unsupported runtime/SDK contracts, and finite-limit
violations. The selected root may lie below an OS path alias; capture anchors its
canonical location. Captured content, not later edits or ambient `node_modules`,
is used for compilation, preflight, module loading, and assets.

The application approves `@mado/helper` 1.0.0 and its private transitive helper
`@mado/order` 1.0.0. Packages request a catalog entry; they cannot approve code,
versions, transitive permissions, compiler plugins, or install scripts. Static
imports and statically visible literal `import()` dependencies are checked before
entry execution through the same inventory resolver. JavaScript uses the pinned
Node/TypeScript parser for this inspection only; it does not gain TypeScript type
checking or ambient Node capabilities. Computed imports are checked when
requested; a caught refusal still closes admission and remains the primary fault.
JavaScript uses ES module live bindings and cycles. Lua rejects cycles with an
import chain. Each attempt has new module instances and mutable `host.state`.

Effective `host.options` combine validated schema defaults with **top-level
replacement**, not recursive merging. Nested values and priority arrays are
readonly through aliases. Unknown fields, coercion, missing replacement fields,
and package-identity mismatches fail before execution.

Both named entry exports must be callable. Guarded module instantiation permits
no ordinary observation/input work. Readiness has its own finite bound and must
return the literal string `Ready`; truthy alternatives do not start workflow.
The host establishes an eligible observation before readiness. The selected
workflow performs observation, template/OCR recognition, priority choice, ordered
submission, receipt handling, and an independent strictly newer-frame condition.

`host.call(method, arguments)` exposes compact JSON values and managed identities,
not native payloads. Operations include `asset`, `observe`, `recognize`, `query`,
`query_wait`, `submit`, `settle`, `postcondition`, `wait`, `release`, and bounded
`log`. Recognition absence is `null`; query exhaustion is a typed timeout.
`Submitted`, `Partial`, and `Uncertain` receipts are not application-effect proof.
No uncertain action is automatically replayed. Release remains available after
admission closes. The controlled-only `fixture` operation injects declared state
transitions; it is not native authority.

Input actions retain the `key_down`/`key_up` forms and also accept a `click` with
capture-pixel `x`, `y`, and `left`/`right`/`middle` button. Native sequences balance
their keys, charge expanded events against finite authority, and preserve actual
SDK receipt/cleanup outcomes. The TypeScript SDK describes these action variants
and optional native receipt facts. See the [native contract](runtime-native.md#native-target-selection-and-finite-authority).

## Containment and measurements

Control and the watchdog never acquire the VM or native-work lock. Interpreter
hooks are installed before module evaluation, including supported Lua coroutines
and JavaScript jobs. Normal entry settlement also closes ordinary admission;
detached jobs do not prolong an attempt. No timer, Node, filesystem, network,
Lua `io`/`os`, native module, or FFI capability is exposed to source packages.

The controlled hold seam retains a real Rust frame owner and work lock. Logical
cancellation can finish while that owner remains live. Missing physical cleanup
requires finite child containment and an incomplete outcome, never a fabricated
clean acknowledgement. A separate observer checks child exit after parent death
and verifies the inert target survives. Only owned harness processes are killed.
A controlled `release_hold` also reaches a worker registering concurrently;
registration cannot lose a release that arrived before the worker was listed.

Each sample starts a fresh child. The tool reports cold process behavior; it does
not claim that these samples measure warm engine reuse. The optional engine cache
is runner-owned, but repeated native reuse still needs explicit qualified evidence.
The trusted compiler has a separately bounded worker, checks the expected parent
PID before startup, and retains a framed control pipe until completion. Control
EOF terminates compilation independently of PID reuse and worker progress.

Results bind plan, inventory, emitted compiler inventory, application-source hash,
Cargo lock hash, engine revision, compiler/interpreter versions, build profile,
OS/kernel, and architecture. Keep materially different identities separate.
Metrics distinguish startup, preflight, workflow, fixed host-operation timing,
cleanup, process CPU, sampled supervisor/child RSS, VM allocation, and live owners.
Here `preflight_us` covers common host preparation and TypeScript compilation;
`workflow_us` covers the adapter invocation, including JavaScript dependency
inspection and module/readiness overhead. It is not an isolated script-body
benchmark.
RSS is an observed periodic maximum, not an OS peak or proof of cleanup. VM
allocation and live-owner values are explicitly endpoint measurements, not peaks.
Missing values remain unavailable rather than becoming zero.
CPU describes the runtime child, not a process-tree total; RSS is sampled
separately for the supervisor and child. The separate trusted parser/compiler
worker's CPU/RSS are not included. TypeScript compilation time is in preflight;
JavaScript dependency-inspection time is in the adapter/workflow measurement.

Entry settlement is recorded before cleanup can block. A retained `Returned`
entry with forced/incomplete cleanup still has a failed overall outcome; missing
settlement remains `Unobserved`, never an inferred successful return.
`EntrySettled` also retains known receipts and an explicitly labeled pre-cleanup
ownership snapshot. If the final record never arrives, those facts survive while
cleanup remains incomplete. Diagnostic detail is byte-bounded with omission
metadata; a transport failure must not bypass cleanup.

A forced exit may truncate the final protocol frame after a successful
`EntrySettled`. That incomplete-tail diagnostic remains in
`metrics.protocol_fault`, but is not promoted into a new entry failure. The run
still fails with forced/incomplete cleanup. Oversized frames, incomplete frames
without a forced exit, and missing entry settlement are not exempted by this
rule; an existing entry failure also retains precedence.

Stop receipt and admission closure use supervisor-clock latency upper bounds,
including pipe delivery and polling. Child-relative timestamps are separate.
A normal return has no external Stop latency. For incomplete cleanup without
an external Stop, containment uses the supervisor's entry-settlement receipt
through observed exit. Cleanup completion and forced exit remain distinct.

Controlled Stop checks start their delay only after reaching the operation under
test: `VmHookReached` for CPU-only VM work, control EOF, and intentional supervisor
exit; `HostWaitEntered` for the first host delay/query wait; or `WorkHeld` for
retained work ownership. Parser startup and build-identity collection are not
evidence that the operation has started. The EOF and intentional-exit checks
require VM-hook evidence before the cancellation receipt, along with a clean
child exit; they do not extend the cleanup or containment deadlines.
These bounded notifications do not block the VM or host on stdout;
the checks still require cancellation, rejected continuation input, and cleanup.

Nearest-rank p50 requires 2 samples, p95 requires 20, and p99
requires 100; insufficient samples produce `null`. Warmups are excluded.

## Evidence and handoff

The checked-in coverage catalog enumerates all four capability specifications.
Result statuses are `PASS`, `FAIL`, `BLOCKED`, `UNEXECUTED`, and
`NOT_APPLICABLE`. An expected refusal can pass its behavioral oracle while its
underlying execution remains `FAIL`; both are retained. Missing native authority
is `BLOCKED`, not a skipped success. Language-only inapplicability does not waive
shared host or lifecycle obligations.
The catalog retains requirements beyond the currently qualified lanes. A green
CI job means its executed oracles passed, not that every native acceptance row
passed.

Requirement coverage is evaluated within a compatible qualification cohort:
complete build/runtime/engine/environment identity plus
`qualification: {version: 1, configuration_sha256, corpus_sha256}`. The two
SHA-256 values bind the suite's complete configuration and corpus manifests,
including its declared profile/scenario/package variations. Individual plan
hashes may differ inside that suite; unrelated source builds or configurations
cannot supply each other's missing checks.
The actual runtime child's startup record supplies its complete build identity,
including CPU brand/count, physical memory, and executed binary SHA-256, without
publishing a hostname, serial number, or executable path. The supervisor checks
the owned PID and run correlation. Missing child identity stays `null`, not the
supervisor's identity; it cannot establish qualified coverage.
Suite identity also binds observed parser/compiler and emitted-inventory
identities, not only declared version strings.

`check` stamps its controlled suite identity automatically. Old or imported rows
without sufficient identity remain readable but cannot create coverage `PASS`.
The coverage matrix exposes per-cohort missing checks and `unqualified_evidence`;
historical failures remain visible even when another cohort passes. Producer
digests identify claimed content, not a signature or authentication of imported
JSON. Do not invent missing qualification metadata for historical native results.

Keep raw plans, model/runtime paths, screenshots, OCR text, loader diagnostics,
and full execution records in private evidence storage. The `report` command
emits aggregate, content-identified results rather than raw native diagnostics.
Do not publish raw `run` output merely because it is JSON.

Full native qualification still needs Windows and Apple Silicon macOS evidence.
The separately approved macOS development selection is JavaScript/QuickJS with
TypeScript authoring; remaining qualification does not prevent manual development.
It does not promise untested platform support or turn the comparison into a
multi-runtime desktop product.
