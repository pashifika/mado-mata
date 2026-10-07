# ADR 0008: Reviewed macOS Native Run admission

- Status: Accepted for the development checkout; native acceptance remains separate
- Date: 2026-09-30
- Scope: One saved macOS application bundle, attach or approved launch, optional one-time exit recovery

## Decision

Extend the existing Desktop Start/preparation worker and fixed engine child.
Controlled and recorded replay retain their non-native input sink. Authoring
Capture remains a separate input-free owner; neither its historical frames nor
Target Check authorize a Script run.

The trusted Run page submits current package/profile/target expectations, separate
launch-if-absent/capture/input/recovery consent, reviewed operation/postcondition text, and finite limits.
Approval is transient and spent on every Start. Relevant edits, including a target
draft changed back to its saved value, invalidate it. The text describes the
operator's review, not a hostile-script sandbox. IPC cannot supply a Plan, process
identity, executable, environment override, or native configuration.

Reserve Preparing before slow work. Capture saved profile, target record and App
environment under existing serialization, then release command/Store locks.
Complete immutable package/profile/asset validation, static compilation/import
linking, required OCR resource capture and engine-artifact preflight without
evaluating package code or acquiring native capture/input. Then run the existing
`readiness()` in the existing supervised QuickJS runner, before target attachment.
Reuse those captured inputs throughout the operation.

Typed discovery separates confirmed absence, a unique verified process,
ambiguity, unverifiable candidates and OS failure. Only approved absence permits
one submission after recipe preparation/revalidation, a captured-resource
checkpoint and the final correspondence recheck, in that order. A newly appearing
unique game is attached instead. A missing window never means an absent game.
Preserve candidate limits, signature/architecture and signed-relocated-copy safeguards.
Give the child the actual runtime executable, PID/lifetime, exact window title
and saved input policy, not the launcher's identity or an OS receipt. Script
status probes drive bounded preparation for the first exact eligible window.
Read-only Foundation/libproc checks retain the selected lifetime while windowless;
the public SDK independently binds its window. Process loss/replacement refuses
the attempt, not a new target selection.

## One-time confirmed-exit recovery

`max_exit_recoveries` is per-Start authority: absent means zero; only zero or one
is accepted. One requires separate recovery consent and launch-if-absent approval.
It is not saved in settings or packages and grants no termination authority.
Recovery reruns the same captured `readiness()` and `workflow()` from entry;
it does not resume a statement or promise exactly-once business actions.

Only attempt 1's positively confirmed bound-process exit after Workflow entry
is eligible. The existing macOS adapter compares the retained kernel start
identity: explicit process absence, a verified reused PID or the bound zombie
lifetime produces typed `TargetExited` evidence. Failed/partial lookups,
unverifiable metadata, window loss, permission/provider errors and launcher exit
do not prove game exit. No error message or Script log is parsed for eligibility.
Generic facade target loss is refined only by an independent positive lifetime
proof. The public facade's first capture fault wins over a later process exit;
capture, recognition and input remain facade-owned.

Keep one reservation, immutable snapshot and irreversible outer Stop owner.
Before admitting attempt 2, require clean native/input settlement, child reap,
joined parent startup work and complete resource accounting. Forced, incomplete,
uncertain or missing outcomes refuse recovery. A fresh child/VM/state, resolver,
session and handle namespace receive only remaining credits; nothing is retargeted
in place. The captured resolver factory cannot reread mutable settings.
The same one-shot startup contract rechecks correspondence: attach a unique
external restart, otherwise submit the reviewed recipe once on confirmed absence.

Recovery admission and launch share Stop's serialization fence. Stop cannot be
cleared by fresh attempt construction. Late work stays attributed to its original
attempt and cannot advance the successor. Retain at most two attempt summaries
independently of logs, including the original exit and separate final Script,
receipts and cleanup outcomes. Bounded diagnostic truncation preserves the typed
exit reason. Start remains reserved and Stop available through settlement and
recovery, including Workspace navigation.

## Script-requested startup

Only Desktop Native Readiness can call `host.call("target_start", {})` and
`host.call("target_status", {})`. Both require an exact empty object; neither
accepts a path, target, recipe, process, input policy or authority override.
No request means no target acquisition or launch. One start request returns
`pending` promptly without waiting for a process/window; duplicates refuse.
Status polling drives bounded work, with at most one preparation step in flight,
not an unbounded queue or a host-owned game-readiness loop.

The result separates target `status` (`not_requested`, `pending`, `capture_ready`),
preparation `phase` and launch disposition. Confirmed process/window absence is
pending only within the admitted attempt. Ambiguity, unverifiable/lost targets,
permissions, OS failure and rejected/uncertain submission remain typed faults,
never a request to retry. Acquisition and early `"Ready"` refuse until capture
is available. The Script supplies its waits and real image/template/OCR criteria;
`capture_ready` alone is not game readiness. Ordinary input opens only in Workflow
after Readiness explicitly returns `"Ready"`.

Module evaluation, Workflow, Controlled and Replay cannot use these calls.
The independent M0 CLI keeps its explicitly supplied `native_config`; it cannot
select Desktop-only phase budgets or acquire this preparation authority.

## Launch library and OS semantics

`crates/application-launch` owns prepared launch requests, literal arguments,
recipient-specific cwd semantics, submission disposition and request settlement.
Its consuming Interface has no Desktop, runner, game-identity or approval
dependency. The Desktop Adapter derives the recipe from the saved binding,
revalidates it and fences submission against Stop. This permits a future Windows
Adapter without a second Desktop launch flow; Windows Native remains out of scope.

For an application bundle, the library calls Apple's
[`NSWorkspace.openApplication`](https://developer.apple.com/documentation/appkit/nsworkspace/openapplication(at:configuration:completionhandler:))
with the outer URL, including supported wrapped bundles. It does not spawn the
inner executable, force a new instance, substitute another installed copy, request
activation, or enable authentication/error prompts. The
[`OpenConfiguration`](https://developer.apple.com/documentation/appkit/nsworkspace/openconfiguration)
Interface has no cwd override: bundle recipients use OS-defined cwd, and an
explicit saved bundle cwd refuses before launch. This corrects the original
design's unsupported promise, with explicit operator approval. An app can still
change foreground state itself, and Gatekeeper UI is not suppressed by this flag.

A direct-executable separate launcher uses the literal ordered argument vector
without a shell, with explicit cwd or its executable's parent as default.
The launcher controls onward game arguments/cwd. Its exit is neither game failure
nor readiness, and its continued lifetime does not retain the automation slot.
Owned direct-child handles have independent non-terminating reaping ownership.
The library rejects Windows `.bat`/`.cmd` recipients rather than allowing Rust's
[`Command` batch-shell path](https://doc.rust-lang.org/std/process/index.html#windows-argument-splitting).
This preserves its no-implicit-shell contract; it does not enable Windows Native.
Configuration Check/Save, manual running-app Check, authoring, Controlled and
Replay remain unable to launch a game.

## Placement and result commitment

Desktop has no operator-entered physical placement. Latch the first accepted
frame's authoritative target placement and refuse later origin/size/scale changes;
missing placement is not guessed. Copied recognition bases keep their existing
frame/content checks. External CLI plans still require explicit geometry and
representative input kinds; `run` and `manual` reject Desktop-only
`reviewed_operation` before package/output I/O. Internal child transport retains
the host-projected form without inventing representative input actions.

At public SDK revision `4b4f3296838a9eecdcb00e9d2bb3121a25cdc240`,
[`Session::commit_frame`](https://github.com/pashifika/mado-pilot/blob/4b4f3296838a9eecdcb00e9d2bb3121a25cdc240/crates/automation/runtime/src/session.rs#L323-L344)
is a historical ordering guarantee. The same source's `recognize`,
`scan_ocr_zones`, and `find_template` commit completed results, including empty
results. No upstream defect was established. Keep those SDK commitments; commit
the exact retained frame/operation before publishing new consumer observations
or query sources, and again for fresh input admission. Do not substitute
`is_closed` polling, a second capture, or an invented frame token. A later terminal
fault does not revoke historical results or turn them into current input authority.

## Bounds and termination

The reviewed host tuple has positive defaults/ceilings of **60 s Startup**,
**30 s Readiness** and **30 s Workflow**. Smaller valid tuples are accepted;
invalid tuples refuse, without clamping. Startup begins at reservation and
includes preflight, Script work before capture availability, launch, bounded
process/window preparation and native initialization. Readiness starts once at
`capture_ready`; Workflow starts once after `"Ready"`. No phase borrows unused
time or renews its own budget.

At reservation the host fixes an absolute outer deadline. For stage sum `B` and
admitted recovery count `r`, it is `(1 + r) * B + r * 3000 ms`: **120 s** by
default, or **243 s** with one recovery. The extra 3 s is the existing inter-attempt
cleanup/containment allowance, not permission to continue after incomplete cleanup.
Each fresh attempt retains the same stage ceilings capped by that deadline.
Authenticated entry settlement ends the ordinary stage clock. Cleanup and
containment retain their independent limits; the original outer Stop/deadline
still governs successor admission. A stage timeout earned before settlement
remains terminal.
The supervisor transfers shared-OS-monotonic deadlines conservatively; spawn,
payload transfer, validation and later phase transitions cannot extend it.
The former shared 30-second policy
expired during native initialization in an observed cold start, before Script
entry. Desktop Native no longer inherits the generic hidden 2-second Readiness
bound. The pinned SDK accepts the finite operation duration; its separate
2-second maximum for an individual macOS native wait is unchanged.

Other bounds remain 300 acquired frames across both attempts, 1 s waits, 100 ms
pacing, and 64 cumulative expanded input events. A click consumes three events,
or four with a hold; each key press/release consumes one. Only verified terminal
accounting transfers unused credits. Partial or uncertain input never permits
recovery or refunds authority. Final cleanup retains its separate restricted
path, with 1 s cleanup and 2 s containment bounds.

Preserve explicit cancellation versus whole-operation timeout through the typed
supervisor verdict. The child closes admission at its deadline, but keeps its own
operation-timeout fault provisional until the control reader has adopted the
verdict and emitted its receipts. Reconcile only that captured fault, retaining
its original diagnostic; never relabel an earned stage/resource failure or reset
the closure/cleanup clock. Do not rewrite a returned record to hide its receipts,
entry outcome or cleanup. Retain worker and
child ownership until physical settlement/reaping. Forced or unverified Native
cleanup blocks another Native run and in-process configuration reconstruction;
the operator must reconcile the target and restart the application.
Native initialization evidence starts at SDK initializer admission, not Host
construction or worker scheduling. A later unverified acquisition/rollback or
worker panic remains a sticky cleanup obligation even when a Script fault or Stop
is already primary. Execution/containment errors retain the callback's final
launch disposition after physical settlement; they cannot restore an earlier
"not submitted" state.

Launch admission and cancellation share a linearization fence. If Stop wins,
no request is submitted. After admission the OS can finish opening the app after
Stop or timeout; the pending callback remains owned until physical completion,
but its late result cannot resume Script work, start capture/input or enter
Workflow. Script return/throw and app close retain the same ownership obligations.
Stop never terminates the game/launcher or claims to undo launch. Retain target
status, typed preparation phase and launch disposition independently of primary
outcome, input receipts, visible effect and cleanup, including accepted-launch-then-timeout.

## Acceptance boundary

Portable admission/snapshot/CLI/supervisor regressions, launch-library checks and
public-facade controlled replay establish consumer behavior, not live game effect.
Actual WebView review/refusal and controlled Stop/rerun are distinct from the
separately authorized authored native workflow, newer-frame visible effect,
normal Stop and clean rerun. Useful acceptance covers both already-running and
initially absent games, plus an authorized installed separate-launcher recipe.
OS acceptance alone is insufficient. Recovery acceptance additionally requires
the initial Script to remain active through a separately authorized normal game
close, then the same Script in a fresh attempt to establish its own usable-screen
postcondition. A newer compatible image after any reviewed startup click, complete
receipts and independent physical cleanup are required; `Ready`, launch acceptance
and `capture_ready` alone are not recovered usability. Verify a second separately
reviewed run, default-off no-recovery and normal Stop during recovery. Deterministic
recognition corpora must exercise the authored predicates, not a Ready echo.
Missing authority, current geometry/images or a supported launcher recipe remains
explicitly incomplete. Windows Native Desktop, requested activation, arbitrary
retries, the broader M0/R6 matrix and release qualification remain out of scope.
See the [Desktop procedure](../desktop.md#reviewed-macos-native-start)
and [engine prerequisites](../runtime-native.md).
