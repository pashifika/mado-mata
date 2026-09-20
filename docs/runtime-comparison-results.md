# M0 CI-first comparison results

## Decision

`Blocked(exact-target integration prerequisite and incomplete both-OS qualification)`.
No runtime is selected for production. This delivery provides executable controlled
comparison paths, not completion of every M0 acceptance scenario. Native capture,
OCR, input, target attachment, and real replay were not exercised.

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
independently passed the original 111-case corpus on Linux, Apple Silicon macOS,
and Windows, plus branch-flow validation and `CI Gate`. Hosted checks grant no
authority to exercise an installed game or private OCR configuration.

Native rows are blocked by the missing exact-target facade contract and absent
operator-approved workload, paths, OCR/runtime/model identity, recorded corpus,
and prospective per-OS authority/budgets. Replay rows are blocked by missing real
recorded frames and validated private recognition configuration. No screenshots,
model paths, target names, or credentials were collected for this delivery.

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
