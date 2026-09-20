# M0 CI-first comparison results

## Decision

`Blocked(incomplete automated-native and both-OS qualification)`.
No runtime is selected for production. This delivery provides executable controlled
comparison paths, real recognition smoke evidence, and a strict native bridge.
The initial macOS process-directed probes failed. A separately authorized system
route later completed the requested roundtrip with approved image verification;
the OCR failures and a pre-correction native shutdown crash remain failed records.
Windows game execution and representative recorded-gameplay qualification remain
unexecuted.

## Initial local evidence

The local host was Apple Silicon macOS, kernel 27.0.0, using Rust 1.97.1 and
Node.js 24.18.0. The engine dependency was the public revision
`2c9d57a53e44ffc97315975c3ca46a766d6c8539`.

| Execution | Observed result | Boundary |
| --- | --- | --- |
| Locked default Cargo build | Passed | Actual QuickJS and Lua interpreters; no native engine initialization |
| Cargo behavioral regressions | 15 passed | Host validation, ordering, identities, cancellation, and ownership |
| Trusted compiler self-check | 13 passed | Actual compilation, strict diagnostics, loader policy, and completions |
| Executable `check` corpus | 163 passed | Includes expected refusals, retained successful entry with forced cleanup, intentional exit, parent loss, and target survival |
| Published `run` example | 6 successful runs | 1 warmup and 5 measured samples; controlled input only |
| Published `report` example | Parsed successfully; decision blocked | p50 supported; p95 and p99 unavailable at this sample count |
| Compiler control EOF during pending work | `CompilerContainment`, exit 1 | Owner remained alive; response arrived within 9 ms in this observation |
| Missing budget, missing wait bound, unknown scenario, invalid report status | All refused | No permissive defaults or invented success |
| Locked `--features engine` build and `--help` startup | Passed | Real facade compilation/linking and executable image load, not engine initialization or native/replay qualification |
| Full tracked-repository check | Passed | 16 governance regressions, actionlint, offline links, compiler checks, Rust regressions, and the 163-case executable corpus |
| Post-return ordinary host work | Reproduced, then corrected | Both JS/Lua regressions failed before the guard; observe/query/wait refuse after return while release remains available |

The versioned raw results remain in private execution storage. Each executable
result binds source and Cargo-lock hashes, plan and inventory identities, runtime
versions, platform, and build profile. Repeatable commands are in the
[runtime guide](runtime-comparison.md); hosted outcomes belong to the PR checks.
Do not compare timings across different build, plan, inventory, or host identities.

## Independent Windows engine-build smoke

At source `c420f1a1ac1b9293f07afb37562eee0af1e93564`, a Windows 11 build 26200
AMD64 host built the locked `--features engine` executable and ran that exact
binary with `--help` under the selected DLL environment. Both exited 0; startup
returned the expected help text with empty stderr, without a timeout, output
limit, or cleanup failure.

The recorded environment used Rust/Cargo 1.97.1, MSVC 19.44.35228 x64,
Windows SDK 10.0.26100.0, OpenCV 4.14.0, and libclang 22.1.8. The initial Cargo
libgit2 fetch failed SSH authentication under the workstation's existing Git
configuration. A retry with child-only `CARGO_NET_GIT_FETCH_WITH_CLI=true`
succeeded without changing the pin or global configuration. The build retained
two `unsafe_code` warnings in `inventory.rs`; they were not suppressed.

The worker used its existing checkout, made no tracked changes, and returned
full private build/startup records and selected DLL identities. This establishes
Windows compilation and executable image startup, not ONNX initialization,
model loading, real replay, native capture/input, or a performance budget pass.

## Real engine initialization and static-fixture recognition

An explicitly authorized follow-up uses the pinned upstream Apache-2.0 G-004
`status.png` and its independent text/geometry oracle. The generated static image
is repeated for the host readiness observation and workflow observation under an
artificial offline schedule. It is not a recorded gameplay sequence, application
transition, or postcondition. No input is submitted.

On Windows, both JavaScript and Lua initialized the actual CPU OCR model, then
exposed a shared-host `InvalidHandle` failure on the first readiness release,
before inference. Native IDs include attempt identity; release routing still
assumed an obsolete `engine-` prefix. The host now resolves its own resources
first and delegates remaining releases to the engine registry without relying
on spelling or holding the host-state lock.

With that correction, JavaScript and Lua passed on both macOS and Windows:
real template matching, all six exact OCR text/geometry query oracles, blank-region
absence, and clean attempt teardown. Each run completed 21 explicit managed
releases without failure. All four outer invocations exited 0 with empty stderr,
no timeout/output limit, and completed process cleanup. No input or postcondition
was exercised.

The selected CPU runtime/model identities are validated before execution.
Prospective finite smoke bounds are not representative performance budgets;
one cold sample per candidate supports no distribution. Attempt cleanup reports
zero native/script/in-flight owners separately from the retained runner
engine/model baseline. Process exit is not a library-unload or warm-reuse claim.

Setup refusals are preserved separately: the first smoke plan exceeded the
existing snapshot ceiling and was reduced before execution; Windows local paths
needed Rust's exact extended canonical form. The package uses expected text only
through the documented query API. None of these corrections changes fixture
pixels, oracle text/geometry, the engine pin, or native authority.

## Retained-provenance integration and macOS native probes

A separately authorized source update pins the public engine revision
`85ccc580cd28ffb9b0b52271f6c87f1af0109a04`. It adds optional retained-process
path/lifetime provenance, preserves native open/dispatch guards, and connects the
consumer to real native sessions. The earlier pin and smoke results above remain
historical evidence; they are not silently relabeled with the new revision.

The changed upstream capture package passed 103 tests on each of macOS and
Windows. Two additional macOS provenance-boundary tests passed, as did the changed
platform/capture lint checks and facade builds. These are source/build checks,
not an upstream full-workspace or native-live qualification claim.

The consumer passed 18 default and 21 engine-feature regressions and rebuilt the
native executable. On Apple Silicon macOS 27, an authorized exact-target
capture/OCR run passed without input. Its first setup attempt had exhausted the
existing JavaScript promise allowance; that failed record was retained. A fresh
plan raised only the enclosing VM allowance, leaving the native input event cap
unchanged. This smoke does not qualify macOS 27 as a supported upstream release.

Two separately authorized, bounded one-click probes then used `process_directed`:
first preserving focus, then requiring focus after manual operator foregrounding.
Each returned three submitted SDK events, `invocation_only` evidence, no fallback,
and no held-state release obligation. In both cases, a strictly newer-frame OCR
postcondition was false and a separate target-only image still showed no menu.
Both attempts failed; neither completed even the first navigation step.
Native close and outer process cleanup completed. At that checkpoint, no further
click or alternate route was sent and Windows game operations remained on hold.

The private records bind the native executable to source SHA-256
`e94ed07f89ace95f11416de08a35953cda86030c1cd6e6e3f693f4bc6a6c5aa7`
and Cargo-lock SHA-256
`cca5d4a0613412220c07adc4d7aea24033a60eb54be15728db2310c386fc05de`.
The target, runtime paths, images, OCR content, plans, and raw logs remain private.
Invocation without observed effect is not yet a diagnosed SDK or game defect.

### System-route continuation and shutdown correction

The operator subsequently authorized at most 20 additional logical clicks using
`system` with `require_focused`, without fallback or programmatic foregrounding.
The continuation submitted 12 logical clicks / 36 SDK events against the retained
target; 13 click reservations included one name-query failure that sent no input.
It reached the requested character, activated the target switch, returned once,
reopened the guide with that switch still active, deactivated it, and returned
twice to the initial menu. The final returns used explicit 1500 ms waits and
separate image checks. Name and title OCR checks failed in some attempts; the
operator explicitly approved image-based verification rather than relabeling
those failed checks as passes. This is an assisted, segmented UI result, not an
unattended OCR success or representative performance qualification.

Each action-stage invocation ended its child and capture session; separate
read-only inspector sessions then captured the resulting screen. The operator
observed the target's capture indicator appearing and disappearing. This does not
validate the intended single continuous capture session across a whole run.
Repeated capture start/stop is a confounder for the input/OCR observations, not
evidence that the SDK reopens capture on every frame. The consumer retains one
session across `observe` calls inside an execution and closes it during `finish`.
Those segmented attempts did not establish continuous execution. A separate
single-session follow-up is recorded below; Windows target authority remains open.

One earlier child returned its script and reported clean native-session cleanup,
then aborted during normal process exit. The crash stack placed the failing
`recursive_mutex::lock()` in ONNX Runtime 1.29.0's `Microsoft::Applications::Events`
HTTP-response worker while the main thread ran `PosixTelemetry::Shutdown()`.
The library's [POSIX implementation](https://github.com/microsoft/onnxruntime/blob/v1.29.0/onnxruntime/core/platform/posix/telemetry.cc)
keeps its uploader and `ProcessInfo` active despite the existing API-level
telemetry opt-out. The crash record does not establish upload success or payload.

Both consumer child-launch sites now enforce `ORT_DISABLE_TELEMETRY=1` before
initialization, preventing that uploader from being created. Two input-free
static-fixture OCR executions, with the parent variable unset and set to `0`,
both passed with child exit 0 and clean cleanup. Subsequent native executions
did not repeat the shutdown abort; semantic OCR failures still exited as failures.
The runtime, model bytes, input route, and cleanup semantics were not replaced
or weakened. The final native sessions and outer processes completed cleanup;
macOS input stopped before the serial slot was handed to Windows preparation.

### Single-session macOS workflow smoke

A later, separately authorized trial executed one complete JavaScript workflow
against the same retained macOS target. Preparation used authorized screenshots
to establish OCR regions and control locations, then verified distinct switch
templates on independent recorded captures. The native run used those prepared
assets, not replay frames.

The single invocation completed all six requested steps within its ten-click,
30-event and 60-second bounds. It exercised the one-time menu-reveal path and
character selection, verified the selected character with OCR, verified switch
ON and OFF states with image templates, and verified the final menu with OCR.
The script's session/lifetime continuity guards passed. No independent inspector
capture, automatic input retry, route fallback, or programmatic focus change
occurred during or after this trial. The child exited 0; session close and physical
cleanup were confirmed with no remaining script handles or input sequences.
Input receipts remain invocation-only; the separate recognition postconditions
provide the observed application effects.
The operator also confirmed visible workflow success.

This is one real, bounded macOS workflow smoke, not representative performance,
Windows qualification, runtime adoption, or proof of a future editor integration.
The OS privacy-indicator animation was not independently recorded. Windows game
execution remains unexecuted pending an explicitly approved executable path and
its own target-specific preparation.

## Coverage, not just case count

The catalog contains 112 requirement scenarios across four capabilities. The
local result expands them into candidate/lane/OS rows. Its Apple Silicon
controlled coverage has 76 passing JavaScript rows and 70 passing Lua rows;
two controlled rows per candidate remain `UNEXECUTED`. Language-specific
inapplicability is separate. Several checks support more than one requirement;
163 passing checks do not mean 112 requirements passed.

The remaining controlled rows require target-PID reuse injection and a scheduled
recovery transition racing with Stop. Those comparison interfaces do not exist;
same harness PID or a hand-written conditional is not substitute evidence.
Windows and native rows cannot inherit local macOS outcomes. The initial
[hosted run](https://github.com/pashifika/mado-mata/actions/runs/35492214355)
independently passed the original 111-case corpus. The expanded
[163-case run](https://github.com/pashifika/mado-mata/actions/runs/35494185934)
then passed on Linux, Apple Silicon macOS, and Windows, plus branch-flow
validation and `CI Gate`. Hosted checks grant no authority to exercise an
installed game or private OCR configuration.

Live-native qualification remains incomplete despite the exact-target contract
and assisted macOS roundtrip. The initial failed probes, later OCR failures, and
native shutdown abort remain evidence, not discarded samples. Windows game
execution awaits exact target authority, and representative per-OS budgets and
sufficient comparable samples remain absent. Representative replay still needs
identified recorded frames beyond the generated static fixture. Validated
model/runtime identities support only the scoped evidence above; raw execution
records and machine-local paths remain private.

## Candidate and authoring observations

| Path | Verified characteristics | Unresolved comparison |
| --- | --- | --- |
| Direct Rust | Independent expected decisions and shared-host oracle | Real recognition/input costs and native ownership |
| JavaScript | ES module caching/live cycles, bounded jobs, interruptible computation, frozen options | Native application effect, full lifecycle corpus, representative performance |
| Lua | Restricted secondary loading, coroutine interruption, cycle refusal, readonly values | Native workload coverage, recovery/PID-reuse cases, representative performance |
| TypeScript to JavaScript | Inventory-bound compiler, strict SDK/schema types, actual completions, original helper source locations | Separate compiler CPU/RSS are not included in runtime-child measurements |

The observations do not establish a winner. Timings are cold-child measurements;
warm engine reuse is not qualified. RSS is sampled rather than a platform peak;
VM allocation and live-owner values are endpoints. Missing measurements remain
unavailable. Controlled plan ceilings are not retroactively chosen native budgets.

## Corrections and handoff

Evidence corrected the QuickJS allocator choice, first-fault latching before
script-observable error construction, exact retained Promise identities, trusted
root alias handling, compiler control ownership, and fresh replay handle identity.
Additional regressions exposed ordinary host work after entry return and the risk
of losing settled entry results during forced cleanup. These corrections are in
the [ADR](adr/0001-runtime-comparison-boundaries.md).

The additive upstream provenance API and consuming native adapter resolved the
earlier integration prerequisite. Input invocation failures, the later ONNX Runtime
shutdown crash, and semantic OCR failures are separate findings. The startup
telemetry correction addresses the diagnosed uploader lifecycle without claiming
a general SDK, OCR, or game repair. The [native guide](runtime-native.md) records
the current strict admission, startup environment, and receipt contracts.

Complete the remaining controlled rows, resolve unattended native OCR verification,
supply representative replay/native inputs, and collect both-OS
evidence before choosing a runtime. Only then does a primary-OS M1–M4 development
plan become the subsequent policy; retaining comparison adapters does not impose
a multi-runtime desktop product.
