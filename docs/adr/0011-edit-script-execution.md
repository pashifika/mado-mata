# ADR 0011: Edit-owned Script execution

- Status: Accepted for implementation; native acceptance remains separate
- Scope: Whole package and existing-function execution without ending Edit

## Decision

Reuse the authoring lease, operation reservation, captured inventory, trusted
compiler and supervised runner. Do not create a second runner or persistent VM.
Ordinary Start remains unavailable during Edit and refuses a function invocation.
The owner-bound trial command validates the current saved revision and profile,
then supplies the immutable inventory to the existing preparation path.

Dirty content requires explicit **Save all and run** through the shared draft
controller. Partial publication, failed refresh, composition or a later edit
blocks admission. After capture, editing affects only the next invocation.
Rerun retains setup, not executable bytes, approvals, VM state or live handles.

Setup discovery runs only for explicit Setup opening, Load/Reload or Run preparation. An early
implementation also ran it after Save; real Edit inspection and review showed
that its reservation interrupted subsequent Save/Validate and OMP commands.
Removing that automatic work preserves the ordinary editing contract.

Profile discovery returns an empty catalogue for an unregistered namespace.
Schema-valid explicit values do not require ordinary profile-store ownership;
saved selections retain its strict identity checks. Incompatible profiles reuse
the existing repair/reset/close operations under the Edit owner and saved
revision, without binding a normal Run selection or writing during discovery.
Same-revision setup reads retain the repair token and draft; publication or
owner changes retire that authority. Committed repair receipts survive a failed
catalogue refresh.

Diagnostic owner/revision attribution is independent of a clean execution
ticket. Dirty source can open an obsolete diagnostic's file, but cannot gain
execution authority or an exact-position claim from that diagnostic.

## Setup placement

Replace the Package contents validation-details button with **Setup**, retaining
its saved-package validation icon. The icon never asserts execution readiness;
diagnostics remain in shared Status details.

An initial inline trial strip and expandable configuration consumed editor space.
Use a compact invocation dialog instead, with full configuration on the Run tab.
During Edit, that tab operates on the same owner-bound trial state, not ordinary
Run's independent profile draft. Reuse option/lane fields and existing Target,
Native review and repair controls. Do not end the lease or enable ordinary Start.

Actual WKWebView checks at 760 CSS px showed that placing the Run-settings link
after function arguments hid the route below the initial viewport. Place it
before invocation fields. Close the dialog before execution/navigation, and use
the existing dialog footer for setup-discovery Stop and retained capture Release.

## Function selection and invocation

List concrete eligible file-level declarations from the captured sources without
evaluating modules. Include named declarations and function/arrow bindings,
exported or file-local, including async functions. Overloads identify one
implementation; re-export aliases do not duplicate its selectable declaration.
The UI explains unsupported nested, receiver-dependent and generator functions;
there is no separate catalogue of disabled entries. Explicit unsupported or stale
requests still fail before invocation. This is smaller than maintaining a second
entry model and does not make an unsupported function runnable.

Use the compiler AST to append a collision-free transient export. Preserve
original source maps and leave authored source and the manifest unchanged.
Imports and top-level initialization retain the existing restricted loading
phase. Invoke only the selected function: neither package readiness nor workflow
is an implicit prelude.

Accept a positional JSON array, bounded to 64 KiB UTF-8, depth 32 and 32 arguments.
Diagnose statically known missing positions; do not reconstruct closures,
receivers or native handles. Await promises under the existing finite limits.
After closing ordinary admission, summarize primitive returns only. Nonprimitive
values stay opaque; display must not call getters or conversion hooks.
String projection uses validated UTF-8 conversion. Isolated surrogates remain
unavailable; a bounded prefix split inside a valid pair is recut safely.

## Native boundary

Whole package preserves ordinary Script-led startup and readiness. Function uses
the same internal correspondence and initialization machinery to attach to an
already-running target before invoking the selected function. It does not call
package readiness or claim that capture availability means the game is ready.
Launch, recovery, activation and scene preparation are unavailable in this mode.

Every Native submission needs fresh review of the saved source, entry, arguments,
profile, target, environment and finite limits. Existing Recognition capture is
not an execution session; release and settle it first. One operation retains
ownership through physical settlement. Incomplete cleanup fences both the Edit
lease and subsequent Native work. Entry return and input submission do not prove
application effect or completed cleanup.

## Evidence and limits

Real compiler/QuickJS regressions exercise non-evaluating discovery, file-local
async execution, bounded arguments, original-source errors, fresh state, loader
refusal, finite promises and unchanged whole-package dispatch. A separate
saved-fixture smoke ran `decisions.ts::decide(null, null)` through the owned
runner: package readiness was not invoked, the return was `null`, and cleanup
was clean. Ordinary function Start was refused.

Actual macOS WKWebView observation also exercised Whole package, a file-local
async function, corrected-source rerun, Undo, mapped source navigation and
active-child Stop. The function returned 11, then 111 after an explicit saved
edit; each invocation logged fresh module state. Readiness and workflow were
deliberately throwing but were not invoked by Function. Stop retained a
Cancelled primary outcome separately from clean controlled cleanup.

These are controlled, non-native results. Separately authorized native
attachment, input, game effect and physical settlement remain independent
acceptance obligations. The implementation grants no Windows Native support
and no execution capability to the OMP adapter.
