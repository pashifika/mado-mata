# ADR 0011: Edit-owned Script execution

- Status: Accepted for implementation; native acceptance remains separate
- Scope: Whole package and existing-function execution without ending Edit

## Decision

Reuse the authoring lease, operation reservation, captured inventory, trusted
compiler and supervised runner. Do not create a second runner or persistent VM.
Ordinary Start remains unavailable during Edit and refuses a function invocation.
The owner-bound trial command validates the current saved revision and profile,
then supplies the immutable inventory to the existing preparation path.

Dirty content requires explicit **Save all and run** for non-native execution,
or **Save all and review** for Native, through the shared draft controller.
Partial publication, failed refresh, composition or a later edit blocks admission.
After admission, editing affects only the next invocation. Rerun retains setup,
not executable bytes, approvals, VM state or live handles.

Setup discovery runs only for explicit Run… opening, Load/Reload or Run preparation. An early
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

## Execution entry and defaults

Keep **Run…** beside Validate in Package contents, retaining the saved-package
validation icon. The icon never asserts execution readiness; diagnostics remain
in shared Status details.

The initial inline strip consumed editor space. Its compact Setup replacement
still forced a profile choice and a Run-tab round trip. The operator could not
complete that workflow. A reference editor instead selects a ready destination
beside Run/Stop and prepares the current package on explicit Run.

Use one whole-package/function selector and an explicit application/test
destination. Initialize from the actual packaged `default` preset, otherwise
schema defaults; never replace an invalid or disappeared explicit selection.
A declared target selects Native without consent, otherwise non-native Test.
Unsupported Native remains refused. Zero-argument functions need no JSON edit.

Configured target and OCR settings were valid during actual Edit inspection,
but `nativeText` still blocked Run on handwritten operation/postcondition prose.
The operator chose one explicit captured-Script confirmation instead of that
extra description and separate capture/input checkboxes.

Keep Native review in the execution dialog. Run captures the saved source, entry,
arguments, values, target, environment and finite limits without granting consent.
Show that captured Script/target review in the same dialog; **Allow** grants live
capture and keyboard/pointer input for that execution only. Preserve visible
target, route and focus. Keep limits and optional whole-package launch/recovery
in disclosure; launch/recovery remain separately approved. Do not invent
game-effect text or treat approval as evidence of an application outcome.
Missing prerequisites link directly to Target or OCR settings and return here.
Advanced profiles/options remain on Run, sharing the Edit state without ending
the lease or enabling ordinary Start.

Dirty Native offers **Save all and review**, not execution or confirmation.
Saving and rereading remain in the dialog; the next explicit Run opens review
of the final revision. Back, cancellation, closing and Escape grant no authority.
Intervening changes invalidate review; a stale target discovered on reread
refuses submission instead of replacing the captured target. Each Allow is
consumed once, including on refusal. Close before execution/navigation so shared
Stop stays reachable. Setup discovery retains Stop/Release in the dialog footer.

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
profile, target, environment and finite limits. Edit submits
`NativeIntent.review = {"kind":"script"}` only with one-shot confirmation.
Ordinary Run retains `review.kind: "operation"` with `operation` and
`visible_postcondition` text and separate capture/input consent. The internal
engine projection uses `input.package_approved`; external CLI plans cannot claim
that authority or Script review.

Existing Recognition capture is not an execution session; release and settle it
first. One operation retains ownership through physical settlement. Incomplete
cleanup fences both the Edit lease and subsequent Native work. Entry return and
input submission do not prove application effect or completed cleanup.

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

The simplified flow ran a zero-argument function with package defaults and
returned `42` without visiting Run. A required-argument function returned `111`.
Earlier English/Japanese WKWebView checks at 760 CSS px verified prerequisite
links, return focus, preserved draft/Undo and disabled unapproved Native execution.
Dirty Native saved and reread in place without submitting Native execution.
The captured-Script confirmation supersedes that disabled-Run review placement:
configured Run opens review without prose, and only Allow grants capture/input.
UI smoke observed confirmation, Back/Escape focus and stale-target refusal;
full integrated verification remains pending.

These are controlled, non-native results. Separately authorized native
attachment, input, game effect and physical settlement remain independent
acceptance obligations. The implementation grants no Windows Native support
and no execution capability to the OMP adapter.
