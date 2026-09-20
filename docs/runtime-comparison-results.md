# M0 CI-first comparison results

## Decision

`Blocked(exact-target integration prerequisite and incomplete both-OS qualification)`.
No runtime is selected for production. This delivery provides executable controlled
comparison paths and explicitly scoped real-engine smoke evidence, not completion
of every M0 acceptance scenario. Live capture/input, exact-target attachment, and
representative recorded-gameplay qualification remain unexecuted.

## Verified local evidence

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

Live-native rows remain blocked by the missing exact-target facade contract and
absent operator-approved target/workload, capture/input authority, and prospective
per-OS qualification budgets. Representative replay still needs identified
recorded frames beyond the generated static fixture. Validated model/runtime
identities now support the limited smoke above; raw execution records and
machine-local paths remain private.

## Candidate and authoring observations

| Path | Verified characteristics | Unresolved comparison |
| --- | --- | --- |
| Direct Rust | Independent expected decisions and shared-host oracle | Real recognition/input costs and native ownership |
| JavaScript | ES module caching/live cycles, bounded jobs, interruptible computation, frozen options | Native integration, full lifecycle corpus, representative performance |
| Lua | Restricted secondary loading, coroutine interruption, cycle refusal, readonly values | Native integration, recovery/PID-reuse cases, representative performance |
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

No confirmed MadoPilot implementation defect was established: its current public
facade lacks a contract required for safe exact-target admission. No sibling
source was edited and no unverified engine repair was claimed. The
[native guide](runtime-native.md) identifies the prerequisite without substituting
a title/PID match or a private platform escape hatch.

Complete the remaining controlled rows, supply authorized real replay/native
inputs, resolve and requalify the exact-target dependency, and collect both-OS
evidence before choosing a runtime. Only then does a primary-OS M1–M4 development
plan become the subsequent policy; retaining comparison adapters does not impose
a multi-runtime desktop product.
