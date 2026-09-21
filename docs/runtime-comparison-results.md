# M0 CI-first comparison results

## Decision

**Development selection: JavaScript on QuickJS, with TypeScript authoring,
for macOS manual development.**

**Full native qualification:**
`Blocked(incomplete automated-native and both-OS qualification)`.
The operator-approved macOS-first decision is recorded in the
[ADR](adr/0001-runtime-comparison-boundaries.md#macos-first-development-selection).
It reuses the working host/compiler and does not claim Windows input support or
completed release qualification.
The initial macOS process-directed probes failed. A separately authorized system
route later completed the requested roundtrip with approved image verification;
the OCR failures and a pre-correction native shutdown crash remain failed records.
The Windows system-route acceptance attempt was refused before submitting any
input event. Recorded-frame replay now exercises all four authoring/control paths
on both OSes; neither that replay nor one native workflow establishes adoption.

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
The OS privacy-indicator animation was not independently recorded. The subsequent
Windows preparation and refused native attempt are recorded below.

## Windows preparation and closed native attempt

An operator supplied an exact executable, process lifetime, window, and native
placement. Authorized reference captures established Windows-specific regions,
independent switch-state templates, and the full selected-character OCR field.
No assumed aspect ratio or fixed portrait index was used. Ten original reference
images are retained privately as lossless PNGs; archive CRCs and reconstructed
RGBA hashes were independently verified.

The script's initial-state guard requires either a verified menu or a matching
idle-state template on the same frame as the single dismissal request. Missing
menu OCR alone never authorizes input. Eight actual-JavaScript boundary cases
passed with a nonnative host that intercepts submission before any backend.

The sole authorized `system` / `require_focused` acceptance attempt reached
readiness and verified the idle state. Its first dismissal request returned
`PolicyRefused`, `Unexecuted`, and zero submitted events. No later workflow stage
or post-refusal capture ran. The final visible application state is unverified.
The outer process exited 1 without timeout or forced containment; session close
and physical cleanup completed with zero attempt/native/script owners. The
runner-scoped engine/model baseline was still visible before process teardown.

No retry, focus change, elevation, or route fallback occurred. An earlier
`window_message` refusal remains separate evidence. The receipt does not identify
the exact rejecting policy check or measured token levels; static analysis is
not a runtime root-cause determination. Both native execution slots are closed.

## Background-input compatibility disposition

The 2026-09-21 investigation retained MadoPilot
`85ccc580cd28ffb9b0b52271f6c87f1af0109a04` and the MadoMata input implementation
from `3acde7f72c0a67189fc4cd529174197b52ef04a1`. Rust was upgraded separately to
**1.98.1**; historical results above retain their original toolchain identities.
The routes already exist and are wired into the consumer. Available transport
does not establish target acceptance.

| OS / operation | Route / address scope | Advertised support / receipt ceiling | Observed effect and disposition |
| --- | --- | --- | --- |
| macOS 27.0 (`26A428`), arm64; one idle-screen dismissal click | `process_directed` / owning process | `Unknown` / `invocation_only` | Three SDK events submitted while the application was observed in the background; expected menu absent. **Unqualified**, consumption unresolved. |
| Retained macOS focused workflow | `system` / focused system | `Supported` / `invocation_only` | Separately observed workflow effects. Focused baseline only, not background support. |
| Retained Windows ordinary-window attempt | `window_message` / exact window | `Unknown` / target queue admission | Refused; no background success. Exact rejecting check remains unknown. |
| Retained Windows focused workflow attempt | `system` / focused system | `Supported` / system input admission | `PolicyRefused`, `Unexecuted`, zero submitted events. No new Windows run. |

The new macOS diagnostic used the pinned public Rust facade directly, not a new
interpreter workflow. It retained the exact process lifetime and window, used
capture-pixel move/press/release with unchanged source geometry, selected only
`process_directed` with `preserve`, and performed no activation or fallback.
Application-level `NSWorkspace` observations at admission, immediately before
submission, and at effect observation all reported inactive/non-frontmost.
These are boundary samples, not an atomic per-event foreground guarantee.

The expected result was the initial menu becoming visible. Two target-only
frames came from one capture session: sequence 1 before input and sequence 145
after a 1500 ms observation wait, with the same epoch and geometry. Visual
inspection still showed the idle screen. The receipt reported three submitted
events, possible native effect, no partial effect, no held state, and no cleanup
owed. Session close succeeded; the outer process exited 0 without containment
after approximately 16 seconds. That exit confirms diagnostic completion,
**not** the failed application postcondition.

An earlier preparation found no approved menu control and sent no input.
A subsequent source-image review timed out before dispatch, also with zero
input and confirmed session close. Both remain separate failed/unexecuted
preparations. No input was replayed after the completed no-effect attempt.
The finite diagnostic slots are closed; the historical M0 slots remain closed.
Images, target identities, local configuration, and raw results remain private.

Non-native verification on Rust 1.98.1 passed the full repository check
(36 governance tests, 38 Rust unit tests, two terminal tests, 199 controlled
cases, compiler self-checks, actionlint, and offline links), five engine-feature
consumer input tests, and 84 pinned macOS SDK input/geometry unit tests.
The latter use scripted drivers and geometry sources: they do not prove native
event consumption. Windows SDK unit tests were inspected but not executed here.

No consumer or SDK contract violation was reproduced, so there is no input
implementation repair, speculative upstream Change, or SDK pin update.
Remaining questions are target consumption of the process-addressed event
representation and the exact historical Windows policy-refusal site/conditions.
Neither no-effect nor zero submission alone proves an SDK defect; Windows
integrity levels were not measured by this investigation.

Controlled M1 package/profile work may proceed. M1 must not advertise background
support or derive application success from a receipt. M2/native work inherits
explicit route selection, independent newer-frame effect checks, no automatic
fallback or activation, and these unresolved compatibility questions.
Full both-OS native qualification and the independent capture-terminal/publication
guard prerequisite remain unchanged. macOS 27 results do not extend the SDK's
qualified OS range.

## Recorded replay and ownership qualification

The current pinned facade recognizes two authorized recorded frame crops under
an explicit four-acquisition schedule. Repeated references and replay timestamps
are synthetic ordering, not recorded frame rate or application response latency.
Template-first and OCR-first profiles select independently expected controlled
actions and verify the later recorded full-field postcondition.

On both macOS and Windows, Rust, JavaScript, Lua, and TypeScript-to-JavaScript
each passed both profiles: one warmup plus five measured cold-child invocations
per profile. All 48 invocations per OS closed physically with zero attempt
owners. Independent oracles verify decisions, ordered controlled effects, and
explicit postcondition results; a successful script return alone is insufficient.
Both OSes used matching package inventory identities and comparison-source
identity, with separately identified platform-local OCR configurations. No native
capture or input occurred. Five samples support p50, not p95 or p99.

The macOS release-build workflow p50 values are:

| Path | Template-first (ms) | OCR-first (ms) |
| --- | ---: | ---: |
| Direct Rust | 220.876 | 222.721 |
| JavaScript | 231.442 | 232.288 |
| Lua | 232.503 | 235.005 |
| TypeScript to JavaScript | 231.587 | 231.707 |

The separately sampled Windows debug-build workflow p50 values are:

| Path | Template-first (ms) | OCR-first (ms) |
| --- | ---: | ---: |
| Direct Rust | 418.108 | 416.114 |
| JavaScript | 529.829 | 537.155 |
| Lua | 529.205 | 526.077 |
| TypeScript to JavaScript | 533.462 | 529.050 |

Compare candidates with the direct-Rust control within each OS/build cohort.
The differing build profiles, hardware, and native libraries do not support a
cross-OS speed comparison; the report keeps those identities separate.

These include recognition and host work, not isolated interpreter overhead.
The release validation used process-local `RUSTFLAGS="-C strip=none"` after
macOS 27 rejected a stripped Rust proc-macro library. Optimization was unchanged;
the failed build and workaround remain recorded separately.

A separate same-process probe passed ten real-replay attempts, retained-query
access after the original observation was released, cancellation observed during
real OCR, and runner-resource release. Replacing fixture-owned model or runtime
content at the same path refused reuse; restoring the original bytes permitted
the original configuration again. The eight post-warmup RSS samples grew by
1,196,032 bytes within the predeclared 32 MiB bound. This is a scoped replay
ownership result, not live-target lifecycle or general leak qualification.

The Windows owned-copy probe also completed ten attempts, retained-query and
in-flight OCR cancellation checks, and model-content replacement refusal. Runtime
replacement then failed with `AccessDenied`; its disposable restoration guard
also panicked. That probe is a retained failure, not a complete Windows ownership
pass. Runtime replacement, restoration, and runner-release phases were not
reached. Installed resource hashes remained unchanged. The observed file-operation
refusal does not establish its exact OS cause or a product/SDK cleanup defect.

Eight prerequisite cases preserved unset, malformed, missing, unsupported,
content-mismatch, and initialization-stage failures before workflow admission.
An owned copy of the actual executable, linked to an intentionally absent
fixture dependency, failed in the OS loader before Rust startup. The outer
invocation retained the loader diagnostic and process cleanup; dependent native
scenarios remained blocked. Installed libraries and game processes were untouched.

Preparation failures remain retained: an incorrect engine-manifest entry and
an insufficient frame schedule were corrected before successful sampling. The
initial TypeScript fixture stored its postcondition without exposing evidence;
its later revision emits the result explicitly and was sampled separately.

## Coverage, not just case count

The catalog contains 112 requirement scenarios across four capabilities. The
pre-repair executable corpus passed 192 checks on each of macOS and Windows.
That historical report had 78 passing JavaScript controlled requirement rows
and 72 passing Lua rows per OS, with no applicable controlled row unexecuted.
Language-specific inapplicability is separate; case counts are not requirement counts.

Behavioral success does not waive prospective performance bounds. The combined
report retains three macOS controlled cleanup-budget failures: 1,000,926,
1,000,874, and 1,001,092 microseconds against a 1,000,000-microsecond limit.
Missing instrumentation remains `UNEXECUTED`, not a budget pass.

The added shared-host PID-reuse injection changes only the observed lifetime,
not PID, attempt, session, or geometry. The explicit successor scheduler exercises
Stop before queueing, while queued, and after admission before VM entry; it
cannot reopen cancelled admission or bypass incomplete cleanup. These are
controlled seams, not actual OS PID recycling or production game recovery.
Windows and native rows cannot inherit local macOS outcomes. The initial
[hosted run](https://github.com/pashifika/mado-mata/actions/runs/35492214355)
independently passed the original 111-case corpus. The expanded
[163-case run](https://github.com/pashifika/mado-mata/actions/runs/35494185934)
then passed on Linux, Apple Silicon macOS, and Windows, plus branch-flow
validation and `CI Gate`. Hosted checks grant no authority to exercise an
installed game or private OCR configuration.

Live-native qualification remains incomplete despite the exact-target contract
and assisted macOS roundtrip. The initial failed probes, later OCR failures, and
native shutdown abort remain evidence, not discarded samples. Windows native
input was refused; later workflow postconditions and the remaining both-OS
native lifecycle matrix are unexecuted. The recorded-crop replay above is not
representative end-to-end game performance. Required per-OS native budgets,
tail samples, and some instrumentation remain unavailable. Raw execution
records and machine-local paths remain private.

## Review repair verification

A subsequent local Apple Silicon macOS repair pass passes 199 controlled checks,
35 default Rust unit regressions, and two real supervisor/child terminal
regressions. The optional engine suite passes 42 unit regressions, including
shared-handle-limit checks through the public in-memory replay facade. These
engine regressions perform no native capture, OCR, or OS input. Manual Start
completes successfully; immediate manual Stop retains `Cancelled` and clean
cleanup without forced termination.

The pass corrects six review findings:

- A 9 MiB script exception after submitted controlled input now retains its
  `Script` primary, explicit diagnostic truncation, submission receipt, and
  verified controlled key release.
- A held query with a 1 ms cleanup budget retains submitted-input and pre-cleanup
  owner facts through watchdog exit 124, without claiming clean termination.
- Literal JavaScript dynamic imports are inspected before readiness/input using
  the trusted parser and immutable resolver. Missing, escaped, template,
  parenthesized, and transitive literals are refused; computed imports retain
  their runtime-only refusal contract.
- Host input and engine observations/results/queries share one atomic managed
  handle budget; queued, active, and retained receipt ownership remains charged.
- Failed native session opening retains unverified rollback independently from
  successful runner-cache release. This is conservative reporting, not a fresh
  native rollback qualification.
- Coverage is established inside compatible cohorts, including non-sensitive
  hardware, the executed artifact, and observed compiler/parser identities.
  Incompatible partial cohorts cannot manufacture passing requirements.

The local 199-case cohort supplies 78 JavaScript and 72 Lua controlled passing
requirement rows on macOS. Windows and native rows remain non-passing in this
fresh evidence. The retained split-cohort reproduction, which formerly created
three passing rows only when combined, now creates none. Legacy evidence without
the new cohort identity remains readable but cannot be silently upgraded.

One native finding remains unresolved: target loss during recognition can race
with result publication because the pinned public facade exposes completed
closure, not capture terminal state or an atomic publication guard. Known
`TargetLost`/closed errors are latched and prior receipts survive, but this does
not close that race. See the [native limitation](runtime-native.md).
No Windows rerun, live native operation, permission request, game launch, input,
focus change, or adoption decision was performed by this repair pass.

## Candidate and authoring observations

| Path | Verified characteristics | Unresolved comparison |
| --- | --- | --- |
| Direct Rust | Independent decisions, shared-host safety, real recorded template/OCR and newer-frame postconditions | Representative native input costs and full live-target ownership matrix |
| JavaScript | ES module caching/live cycles, bounded jobs, interruptible computation, frozen options, recorded replay | Full native lifecycle corpus and representative performance |
| Lua | Restricted secondary loading, coroutine interruption, cycle refusal, readonly values, recorded replay | Native workload coverage and representative performance |
| TypeScript to JavaScript | Inventory-bound compiler, strict SDK/schema types, actual completions, original helper source locations | Separate compiler CPU/RSS are not included in runtime-child measurements |

The timings alone do not establish a speed winner. Corpus timings are cold-child
measurements; the separate ownership probe establishes only its scoped warm
engine reuse. RSS is sampled rather than a platform peak; VM allocation and
live-owner values are endpoints. Missing measurements remain unavailable.
Controlled or replay ceilings are not retroactively chosen native budgets.

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

Proceed with the selected JavaScript/TypeScript path and the
[manual runner](runtime-comparison.md#manual-script-testing). Remaining native
qualification is deferred, not a blocker on this macOS development path and not
silently passed. Preserve the Windows refusal and the measurement limitations.
Lua remains available in the comparison tool; no multi-runtime product
requirement is introduced.
