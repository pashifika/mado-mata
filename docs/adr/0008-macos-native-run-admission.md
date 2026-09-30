# ADR 0008: Reviewed macOS Native Run admission

- Status: Accepted for the development checkout; native acceptance remains separate
- Date: 2026-09-30
- Scope: One already-running macOS application bundle, not launch or recovery

## Decision

Extend the existing Desktop Start/preparation worker and fixed engine child.
Controlled and recorded replay retain their non-native input sink. Authoring
Capture remains a separate input-free owner; neither its historical frames nor
Target Check authorize a Script run.

The trusted Run page submits current package/profile/target expectations, separate
capture/input consent, reviewed operation/postcondition text, and finite limits.
Approval is transient and spent on every Start. Relevant edits, including a target
draft changed back to its saved value, invalidate it. The text describes the
operator's review, not a hostile-script sandbox. IPC cannot supply a Plan, process
identity, executable, environment override, or native configuration.

Reserve Preparing before slow work. Capture saved profile, target record and App
environment under existing serialization; release command/Store locks before OS
correspondence and resource hashing. Reuse the typed bundle-correspondence proof,
require exactly one verified process, and give the child its actual runtime
executable, PID/lifetime, exact window title and saved input policy. The child
independently binds that lifetime/window through the public SDK. Do not rewrite
the saved installation, choose the first match, activate, prompt, or fall back.
Package recognition metadata/template maps use the existing captured inventory
and shared image budgets. Readiness must precede ordinary workflow input.

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

The host policy is 30 s including preparation, 300 acquired frames, 1 s waits,
100 ms pacing, and 64 cumulative expanded input events. A click consumes three
events, or four with a hold; each key press/release consumes one. Uncertain
submission does not refund authority. Cleanup releases use the separate restricted
path, with 1 s cleanup and 2 s containment bounds.

Preserve explicit cancellation versus whole-operation timeout through a typed
supervisor control reason and the child's first-cause latch. Do not rewrite a
returned record to hide its receipts, entry outcome or cleanup. Retain worker and
child ownership until physical settlement/reaping. Forced or unverified Native
cleanup blocks another Native run and in-process configuration reconstruction;
the operator must reconcile the target and restart the application.

## Acceptance boundary

Portable admission/snapshot/CLI/supervisor regressions and public-facade controlled
replay tests establish consumer behavior, not live capture or game effect.
Actual WebView review/refusal and controlled Stop/rerun are distinct from the
separately authorized authored native workflow, newer-frame visible effect,
normal Stop and clean rerun. Windows Native Desktop, game launch/activation,
automatic recovery, the broader M0/R6 matrix and release qualification remain out
of scope. See the [Desktop procedure](../desktop.md#reviewed-macos-native-start)
and [engine prerequisites](../runtime-native.md).
