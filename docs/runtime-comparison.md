# M0 runtime comparison

This standalone executable compares JavaScript, Lua, TypeScript authoring into the
JavaScript VM, and an independent direct-Rust workload. It is not the desktop
application. Controlled checks exercise actual interpreters and owned processes;
their input sink is **non-native**. They do not capture a screen, launch a game,
change focus, request permissions, or send operating-system input.

**Runtime adoption remains blocked.** The pinned engine cannot establish the
required executable-path/process-lifetime binding for native targets. Both-OS
native evidence, an authorized workload, and private OCR/replay prerequisites
are still required. See [native prerequisites](runtime-native.md) and the
[decision record](adr/0001-runtime-comparison-boundaries.md).

## Setup and commands

Run from the repository root with Rust **1.97.1** and Node.js **24.18.0**. The
controlled build needs a C toolchain for the vendored interpreters, but no OpenCV,
ONNX models, sibling checkout, private planning repository, or game installation.

```sh
npm ci --ignore-scripts --no-audit --no-fund --prefix tools/runtime-comparison/compiler
node tools/runtime-comparison/compiler/compile.mjs --self-check
cargo +1.97.1 build --locked --manifest-path tools/runtime-comparison/Cargo.toml
cargo +1.97.1 test --locked --manifest-path tools/runtime-comparison/Cargo.toml
cargo +1.97.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- check
```

`check` executes the built-in behavioral corpus and prints versioned JSON. Its
exit status reflects the controlled oracles, including expected refusals and
forced/incomplete cleanup. A passing controlled check is not runtime adoption.
Compiler checks include actual strict diagnostics and LanguageService completion
results, not source-text assertions. [Repository CI](ci.md) invokes these checks
from a tracked-only snapshot on Linux, Apple Silicon macOS, and Windows x64.

A bounded, non-native example plan is provided:

```sh
mkdir -p .cache/runtime-comparison
cargo +1.97.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- run tools/runtime-comparison/fixtures/controlled-plan.json tools/runtime-comparison/fixtures/javascript > .cache/runtime-comparison/sample.json
cargo +1.97.1 run --locked --manifest-path tools/runtime-comparison/Cargo.toml -- report .cache/runtime-comparison/sample.json
```

Use a private copy of the plan to select `rust`, `javascript`, `lua`, or
`typescript`, with the corresponding fixture directory (`rust` uses the
JavaScript package's common data). Select `template-first` or `ocr-first`; their
independent expected keys are `A` and `D`. Declare budgets **before** sampling.
The example budgets are controlled-run ceilings, not native qualification budgets.
Only the predeclared controlled scenarios are accepted; misspellings cannot
silently become successful samples.

`child`, `parent-probe`, and `target-probe` are harness-internal modes. The last is
an inert owned process used to prove that containment does not terminate the
separate target. It is not a game launcher.

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
imports are checked before entry execution. Computed imports are checked when
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
RSS is an observed periodic maximum, not an OS peak or proof of cleanup. VM
allocation and live-owner values are explicitly endpoint measurements, not peaks.
Missing values remain unavailable rather than becoming zero.
CPU/RSS describe the Rust supervisor and runtime child, not a process-tree sum.
The separate TypeScript compiler process is not included in those CPU/RSS values;
its compilation time is included in preflight.

Stop receipt and admission closure use supervisor-clock latency upper bounds,
including pipe delivery and polling. Child-relative timestamps are separate.
A normal return has no external Stop latency. Cleanup completion and forced exit
remain distinct. Nearest-rank p50 requires 2 samples, p95 requires 20, and p99
requires 100; insufficient samples produce `null`. Warmups are excluded.

## Evidence and handoff

The checked-in coverage catalog enumerates all four capability specifications.
Result statuses are `PASS`, `FAIL`, `BLOCKED`, `UNEXECUTED`, and
`NOT_APPLICABLE`. An expected refusal can pass its behavioral oracle while its
underlying execution remains `FAIL`; both are retained. Missing native authority
is `BLOCKED`, not a skipped success. Language-only inapplicability does not waive
shared host or lifecycle obligations.
The catalog is exhaustive, but the current executable corpus is not: unmapped
controlled requirements remain `UNEXECUTED`. A green CI job means its executed
oracles passed, not that every controlled or native acceptance row passed.

Keep raw plans, model/runtime paths, screenshots, OCR text, loader diagnostics,
and full execution records in private evidence storage. The `report` command
emits aggregate, content-identified results rather than raw native diagnostics.
Do not publish raw `run` output merely because it is JSON.

The comparison does not select a production runtime from compilation or hosted
checks. Initial M0 qualification still needs Windows and Apple Silicon macOS.
After that gate and an explicit runtime decision, primary-OS M1–M4 iteration can
follow its separate policy. Keeping comparison adapters here does not require a
multi-runtime desktop product.
