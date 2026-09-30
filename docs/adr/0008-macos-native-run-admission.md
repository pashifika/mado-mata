# ADR 0008: Reviewed macOS Native Run admission

- Status: Accepted for the development checkout; native acceptance remains separate
- Date: 2026-09-30
- Scope: One saved macOS application bundle, attach or approved initial launch; no recovery

## Decision

Extend the existing Desktop Start/preparation worker and fixed engine child.
Controlled and recorded replay retain their non-native input sink. Authoring
Capture remains a separate input-free owner; neither its historical frames nor
Target Check authorize a Script run.

The trusted Run page submits current package/profile/target expectations, separate
launch-if-absent/capture/input consent, reviewed operation/postcondition text, and finite limits.
Approval is transient and spent on every Start. Relevant edits, including a target
draft changed back to its saved value, invalidate it. The text describes the
operator's review, not a hostile-script sandbox. IPC cannot supply a Plan, process
identity, executable, environment override, or native configuration.

Reserve Preparing before slow work. Capture saved profile, target record and App
environment under existing serialization, then release command/Store locks.
Complete immutable package/profile/asset validation, static compilation/import
linking, required OCR resource capture and engine-artifact preflight before
launch admission. No package module is evaluated and no capture/input is acquired
to establish launch prerequisites. Reuse those captured inputs after launch.

Typed discovery separates confirmed absence, a unique verified process,
ambiguity, unverifiable candidates and OS failure. Only approved absence permits
one submission after recipe preparation/revalidation, a captured-resource
checkpoint and the final correspondence recheck, in that order. A newly appearing
unique game is attached instead. A missing window never means an absent game.
Preserve candidate limits, signature/architecture and signed-relocated-copy safeguards.
Give the child the
actual runtime executable, PID/lifetime, exact window title and saved input policy,
not the launcher's identity or an OS receipt. Wait for the first exact eligible
window under the original deadline. Read-only Foundation/libproc checks retain
the selected lifetime while windowless; the public SDK independently binds its
window. Process loss/replacement refuses the attempt, not a new target selection.
Readiness must precede ordinary workflow input.

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

The host policy is 30 s including preflight, launch, process/window waiting,
native initialization, Readiness and Workflow. Stage changes never renew it.
The supervisor transfers an absolute shared-OS-monotonic deadline to its child;
spawn, payload transfer and validation consume that same budget. Conservative
conversion never grants a fresh duration, and an expired bound refuses before
Host/native work.
Other bounds remain 300 acquired frames, 1 s waits,
100 ms pacing, and 64 cumulative expanded input events. A click consumes three
events, or four with a hold; each key press/release consumes one. Uncertain
submission does not refund authority. Cleanup releases use the separate restricted
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

Launch admission and cancellation share a linearization fence. If Stop wins,
no request is submitted. After admission the OS can finish opening the app after
Stop or timeout; the pending callback remains owned until physical completion,
but its late result cannot start capture/input or package execution. Stop never
terminates the game/launcher or claims to undo launch. Retain typed preparation
phase and launch disposition independently of primary outcome, input receipts,
visible effect and cleanup, including accepted-launch-then-timeout.

## Acceptance boundary

Portable admission/snapshot/CLI/supervisor regressions, launch-library checks and
public-facade controlled replay establish consumer behavior, not live game effect.
Actual WebView review/refusal and controlled Stop/rerun are distinct from the
separately authorized authored native workflow, newer-frame visible effect,
normal Stop and clean rerun. Useful acceptance covers both already-running and
initially absent games, plus an authorized installed separate-launcher recipe.
OS acceptance alone is insufficient. Missing native authority/resources remain
explicitly incomplete. Windows Native Desktop, requested activation, automatic
recovery, the broader M0/R6 matrix and release qualification remain out of scope.
See the [Desktop procedure](../desktop.md#reviewed-macos-native-start)
and [engine prerequisites](../runtime-native.md).
