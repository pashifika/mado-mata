# ADR 0013: Edit-owned Script execution

- Status: Accepted for implementation; native acceptance remains separate
- Scope: Whole package and existing-function execution without ending Edit

## Decision

Reuse the authoring lease, operation reservation, captured inventory, trusted
compiler and supervised runner. Do not create a second runner or persistent VM.
Ordinary Start remains unavailable during Edit and refuses a function invocation.
The owner-bound trial command validates the current saved revision and profile,
then supplies the immutable inventory to the existing preparation path.

Dirty content requires explicit **Save all and run** through the shared draft
controller in every destination. Partial publication, failed refresh,
composition or a later edit blocks admission. After admission, editing affects
only the next invocation. Rerun retains setup, not executable bytes, VM state or
live handles.

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

## Execution entry and defaults

Keep a **Run** split button beside Validate in Package contents. The main action
executes; the adjoining arrow opens **Setup…**, including when Run is unavailable.
Remove Setup's validation icon; saved-validation state and diagnostics remain in
shared Status details. Each action retains its own accessible name and focus.

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
but `nativeText` blocked Run on handwritten operation/postcondition prose.
Replacing that with a captured-Script Allow screen still forced repeated review
of source that intentionally changes between runs. The operator rejected that
extra step and duplicate application checks.

Setup selects the unit, arguments, destination and settings only. Done, close
and Escape grant no execution authority. The separate explicit Run captures
current source and grants capture/input for that invocation. Dirty source uses
Save all and run directly; no source-bound consent cache or validity session is
introduced. Target and optional startup choices shown at Run must survive its
preparation reread unchanged, or that attempt is refused.

Keep finite limits and optional whole-package launch/recovery in disclosure.
These separate choices survive Script edits and reruns; relevant setup changes
withdraw them. Each explicit Run remains independently bounded. Missing
prerequisites link to Target or OCR settings and return to Setup. Advanced
profiles/options stay on Run under the same Edit lease; ordinary Start stays
disabled. Preparation errors remain on Edit independently of setup errors.
Setup discovery retains Stop/Release in the dialog footer.

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

Whole package preserves Script-led startup and readiness. Function attaches
only to an already-running target; it does not call readiness, launch, recover,
activate or prepare a scene.

Edit resolves the saved APP to its running executable before public SDK selection.
Same-bundle candidates at the saved path need bounded metadata inspection and
exactly one process; filtering to one visible window does not prove APP uniqueness.
If a runtime path differs, reuse existing signed-application correspondence;
unverifiable or ambiguous mapping refuses execution. Pass that executable and
the exact window title, without a preselected PID/lifetime. Do not cache this
mapping as authority or change the saved target.

The SDK owns process/window binding and terminal-aware capture/input. Do not add
per-host-call lifetime checks or invalidate committed historical results with
a later OS lookup. APP absence is distinct from a missing window and permits
launch only when separately approved. Optional recovery retains positive exit
evidence after SDK TargetLost; window loss alone does not prove process exit.
Earlier SDK faults retain precedence; failures use existing logs and cleanup.

This corrects the assumption that an installed executable path always identifies
the running APP. A read-only App Translocation reproduction failed with the saved
path and selected the real SDK window with the resolved runtime path. No capture
or input was needed. Resource integrity was a separate checkout-build cost:
optimizing only SHA-256 reduced measured capture/recheck stages from about 4.6 s
each to 0.32 s without weakening checks. These are stage timings, not end-to-end
Native execution acceptance.

Explicit Edit Run submits `NativeIntent.review = {"kind":"script"}` and one
request's capture/input consent. Ordinary Run retains operation/postcondition
review and separate consent. Its preselected process correspondence is unchanged.
External CLI plans cannot claim Script review, `input.package_approved` or
Desktop-only `confirm_process_exit` authority.

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

The separated Setup/Run flow was exercised in an isolated macOS WKWebView.
Setup/Done submitted nothing. Controlled Run returned `42`; editing then
Save all and run returned `43`, and a standalone Save then Run returned `44`.
Argument execution returned `111` with fresh module state. Compile diagnostics
survived a successful settings reread; runtime errors retained clean cleanup.
An active function Stop settled as Cancelled with Clean cleanup.

Native UI requests were intercepted before host dispatch, independently guarded
by a nonexistent target executable. Explicit Run and dirty Save all and run
each formed one current-source request without an Allow screen. No actual
Native execution or whole-run latency improvement is claimed by this evidence.

These are controlled, non-native results. Separately authorized native
attachment, input, game effect and physical settlement remain independent
acceptance obligations. The implementation grants no Windows Native support
and no execution capability to the OMP adapter.
