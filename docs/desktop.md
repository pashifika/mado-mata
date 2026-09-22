# macOS desktop: controlled runs and recorded replay

MadoMata's trusted Tauri/React WebView provides package inspection, profile
editing, run control, App-local OCR configuration, and structured logs. Package
code runs in the supervised QuickJS runner, never in the WebView. Controlled
runs need no OCR installation. Optional recorded replay uses real engine
OCR/template recognition over explicitly selected, previously authorized frames.
Both desktop lanes retain the **controlled, non-native input sink**. Neither
grants live capture, game launch, focus changes, permission prompts, or OS input.

Only macOS desktop development is supported. Linux and Windows CI check the
frontend and shell-independent Rust core, not additional desktop platforms.
Live native qualification, R6, per-game background compatibility, and release
distribution remain separate work. See the [engine prerequisites](runtime-native.md)
and [replay boundary decision](adr/0003-desktop-recorded-replay.md).

## Build and run from the checkout

Run these commands from the product repository root on macOS, with Xcode Command
Line Tools, Rustup, and Node.js **24.18.0** with its bundled npm. Use the
[pinned tool installation](ci.md#install-the-pinned-tools) for integrity and CI
setup. The application dependencies are fixed by the committed manifests and
lockfiles:

| Component | Version |
| --- | --- |
| Rust | 1.98.1 |
| Tauri / tauri-build | 2.11.6 / 2.6.3 |
| Tauri npm API / CLI | 2.11.1 / 2.11.5 |
| React / React DOM | 19.3.0 |
| TypeScript | 5.9.3 |
| Vite | 8.3.0 |
| tracing / tracing-subscriber | 0.1.41 / 0.3.20 |

The [frontend manifest](../apps/desktop/package.json),
[npm lockfile](../apps/desktop/package-lock.json),
[Rust manifest](../apps/desktop/src-tauri/Cargo.toml), and
[Cargo lockfile](../apps/desktop/src-tauri/Cargo.lock) own the exact dependency
set. The runtime's trusted compiler has its own
[manifest](../tools/runtime-comparison/compiler/package.json) and
[lockfile](../tools/runtime-comparison/compiler/package-lock.json).
No package selected by the operator can install dependencies or run lifecycle
scripts.

```sh
rustup toolchain install 1.98.1 --profile minimal
npm ci --ignore-scripts --no-audit --no-fund --prefix tools/runtime-comparison/compiler
npm ci --ignore-scripts --no-audit --no-fund --prefix apps/desktop
cargo +1.98.1 build --locked --manifest-path tools/runtime-comparison/Cargo.toml --target-dir tools/runtime-comparison/target
npm run build --prefix apps/desktop
cargo +1.98.1 build --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --features custom-protocol
apps/desktop/src-tauri/target/debug/mado-mata-desktop
```

`custom-protocol` serves the built application assets; no Vite server is needed.
These commands do not enable the test-only `webdriver` feature. Normal builds
have no automation listener, and release builds do not register that listener.
The [shell decision](adr/0002-desktop-runner-boundary.md) describes the real
WKWebView boundary; the [replay decision](adr/0003-desktop-recorded-replay.md)
records its separate engine child and bounded consuming evidence.

This is a checkout build, **not a relocatable app bundle or release installer**.
The application owns these fixed paths; package data and IPC cannot select them:

| Artifact | Checkout path |
| --- | --- |
| Controlled runner | `tools/runtime-comparison/target/debug/mado-runtime-comparison` |
| Optional engine runner | `tools/runtime-comparison/target/desktop-engine/debug/mado-runtime-comparison` |
| Trusted compiler | `tools/runtime-comparison/compiler` |

The GUI and controlled runner are built without `engine`. Never replace the
controlled artifact with an engine-enabled build. After installing the pinned
[native prerequisites](runtime-native.md#install-the-engine-prerequisites), use
the absolute location of that revision's public setup tool:

```sh
MACOSX_DEPLOYMENT_TARGET=26.5.2 python3 /absolute/pinned-mado-pilot/tools/setup-native.py -- cargo +1.98.1 build --locked --manifest-path tools/runtime-comparison/Cargo.toml --features engine --target-dir tools/runtime-comparison/target/desktop-engine
```

For engine-enabled regression checks, replace `build` with `test` in that command.
The setup tool configures only its child command and installs nothing. It is not
a product path dependency. Building the engine does not supply models, runtime
libraries, or a corpus, and does not authorize native operations.

Rebuild both relevant runner artifacts after runtime changes. Rebuilding only
the GUI is insufficient. Follow these explicit target directories; do not use a
cross-compilation target for a local app run. CI may use other directories for
compilation checks. Moving a binary alone does not supply the fixed paths.
An absent engine artifact or failure before Rust startup remains a typed
diagnostic in the non-native GUI; controlled execution remains independent.

By default, profiles, settings, and logs live in Tauri's application-local data
directory. For isolated acceptance work, use a dedicated private directory:

```sh
apps/desktop/src-tauri/target/debug/mado-mata-desktop --data-dir "$HOME/Library/Application Support/MadoMata-acceptance"
```

`--data-dir PATH` selects the data root, not the runner, compiler, package, or an
input route. Keep it outside package source and public tracked files. Reuse the
same root when checking restart persistence; use a different private root to
isolate another session without deleting existing data.

New data directories use private Unix permissions. An existing data root or
profile directory with group/other access is refused without changing its mode;
choose or prepare a private application directory deliberately.
The current Tauri setup path reports this refusal on stderr and aborts startup;
it does not open a recovery UI. The existing directory and its files are unchanged.

## Inspect, edit, and save profiles

1. Enter the selected package directory and choose **Inspect**. For the shipped
   controlled example, use the absolute path to
   `tools/runtime-comparison/fixtures/typescript` in this checkout. Inspection
   validates inventory, schema, assets, and static dependencies without executing
   package automation. JavaScript syntax and module bindings are checked without
   evaluating module bodies, including requested executable `.d.ts` dependencies.
   A remembered package path is only a revalidated location hint, not an authority
   grant. Failure to save that hint produces a warning but does not invalidate a
   successfully inspected package; existing settings and pending files survive.
2. Select a package preset or a saved profile. Presets such as `template-first`
   and `ocr-first` become editable drafts; they are not automatically persisted
   application profiles. The form covers supported scalar, enum, nested-object,
   and ordered-array options. Unsupported schema features are errors, not hidden
   controls.
3. Edit the draft and choose **Validate** to view backend validation. Missing
   top-level options may use schema defaults. An explicitly present object or
   array replaces its top-level default; missing nested fields are not filled by
   recursive merging. Validation does not save or replace the raw draft.
4. Use **Save new profile** or **Update profile** to persist valid values. Reopen
   a saved profile to restore its values and order. **Rename** preserves its
   stable ID; **Delete profile** removes only that selected app-local profile.
   **New draft** does not overwrite a saved profile.

The integer editor is limited to JavaScript's safe integer range. For `number`
fields, plain integer text must survive conversion exactly; decimal/exponent
input retains finite floating-point semantics. Source JSON integers outside the
safe integer range and signed zero (`-0.0`) are explicitly refused with field
attribution before they can be silently changed. Unsupported stored values remain
on disk and cannot be updated or renamed through this application.
Number-schema values received from the WebView are restored as floating-point
values before validation, saving, and saved-profile comparison, including values
such as `1.0`, `1e18`, and `1e100` whose JSON spelling may change in transit.

Saved profiles are versioned and bind a stable ID/name, package ID, schema
identity, and values. They are separate files under `profiles/`, not edits to the
package's declared preset files. A relocated compatible package can reuse its
profiles after validation. Incompatible schemas are reported separately from
compatible profiles, and their files remain untouched. Malformed data or
unsupported storage versions refuse profile operations without silent reset,
quarantine, or migration. Diagnostics identify the affected profile/file. Preserve
it before deliberate recovery; do not rewrite identity fields to bypass refusal.

Writes validate first and use a same-directory temporary file plus atomic
replacement. A failed save preserves the previous valid file. Storage is bounded
to **64 profiles**, **64 KiB per encoded profile**, and **1 MiB total profile
bytes**. Settings are separately stored in `settings.json`. Portable profile
values exclude executable/model paths, credentials, permission grants, and input
authority. Package location hints and the optional OCR environment belong only
in application settings; neither is execution authority.

Only `.json` and `.pending` entries belong to the profile store. Other entries,
such as filesystem metadata, are ignored without being opened, but still count
toward the directory-entry bound. An invalid owned file is not ignored.

An interrupted save may leave a `.pending` file. The application preserves it
instead of silently discarding evidence or overwriting it. Close the app and move
that file outside the data root before explicitly retrying; keep the prior
valid `.json` file intact.

## Save and check an OCR environment

1. In **OCR environment**, choose an offered supported profile and enter the
   absolute model root, pinned ONNX Runtime library, and reviewed non-system
   library locations, one library per line. Use the pinned
   [engine prerequisites](runtime-native.md#install-the-engine-prerequisites).
   Model bytes must match the selected profile; a filename alone is insufficient.
   Rust resolves selected aliases to canonical paths and derives byte lengths,
   hashes, and SDK identity; operators do not edit identity fields.
2. **Save environment** validates structure and atomically saves settings. It
   does not load libraries, models, or a backend. **Clear draft** followed by
   **Save as unconfigured** removes the optional environment. Existing M1
   settings with no environment remain unconfigured without a read-time rewrite.
   Failed saves and incompatible stored data preserve the previous bytes.
3. Inspect the recorded package and enter a **Recorded corpus descriptor**.
   The descriptor is the `replay` object from the [existing replay format](runtime-native.md#prepare-a-private-replay-package-and-plan),
   not a Plan or complete `native_config`. Its assets must already belong to the
   inspected package inventory. No arbitrary asset paths, executable override,
   native authority, or unknown fields are accepted.
4. Choose **Check saved environment**. Unsaved environment edits must be saved
   first. Check reserves the same operation slot as Start before settings or
   resource I/O; it is cancellable through **Stop**. Without a corpus it reports
   file-validation progress and the missing prerequisite, never readiness.
5. With a valid corpus, Check starts the real owned engine child and initializes
   the replay backend. It does not resolve a workload profile, compile or
   evaluate package modules, or call readiness/workflow. `NotExecuted`, absent
   VM/workflow metrics, initialization milestones, and independent cleanup are
   intentional. A prerequisite refusal, loader failure, and interrupted
   initialization are different outcomes.

The descriptor is a session location hint, not persisted native authority.
It is bounded to **256 KiB**. Package capture remains **1 MiB** across inspection,
Check, and both Start lanes; expanded replay frames are bounded separately to
**2 MiB**. Geometry, strictly increasing timestamps, package declarations,
template maps, and relative paths are validated before replay admission.

**Last check** retains its operation, stages, identities, failure summary, and
cleanup. Draft/settings/package/descriptor changes detach that association.
Nothing watches paths or grants permission from an earlier successful check:
every Start recaptures and revalidates, including same-path replacements.
Detailed check diagnostics require explicit private disclosure.

## Start, Stop, and results

**Start** submits the current explicit draft. Choose **Controlled** for the
shipped fixtures, or **Recorded replay** with a saved environment and selected
descriptor. Replay accepts only the package workflow, never controlled fault
injection scenarios. The native lane remains refused.

Unmodified saved values retain the saved profile ID; edits run as a draft until
saved. The worker reserves the single preparing/running/stopping slot before
input I/O and owns the settings/profile store lock before Start returns its
operation ID. Later saves cannot overtake this capture. It then revalidates the
package, external resources, and corpus and constructs one immutable input set
with fixed backend limits. A stale inspection or check is not execution authority.

Draft edits and later saves never mutate the active values. Replacing a file can
fail in-flight or subsequent validation; it never updates captured identities.
**Stop** addresses the active operation independently of logs. Wait for its
terminal outcome and owned-child cleanup before starting again. A late record
cannot replace a successor's state. No automatic retry replaces an unsettled run.

The UI keeps entry outcome, receipts, cancellation, and cleanup separate. A Stop
request is not proof of physical cleanup. For a started child, clean cleanup
requires acknowledgement, zero exit, and no forced containment; script failure
can still have clean cleanup. A known pre-child refusal is labeled separately.
Missing evidence remains unverified. Replay failures retain original-source
attribution, successful empty recognition remains distinct from corpus
exhaustion, and recognition-bearing diagnostics require explicit private
disclosure. Full records are rendered only on request, bounded to **512 KiB**;
they are not automatically copied into ordinary logs. Scripts can explicitly
emit bounded messages, so operators must still avoid logging private content.

Run build metadata comes from the actual owned runtime child, not the desktop
executable. If startup identity is unavailable, it remains unknown (`null`) rather
than being substituted with supervisor metadata.

Controlled execution has a **10 s** operation deadline; replay and Check use
**30 s**, including input capture. Repeated parent/child resource verification
is not skipped to fit the controlled-only deadline. Cleanup remains **1 s** and
containment **2 s**. Controller shutdown waits at most **14 s** for its owned
worker. Ordinary window closure requests shutdown off the UI thread. Native macOS
Quit can bypass that request callback, so the final exit callback waits for the
same bounded shutdown, without starting a second sequence. This fallback is
source-verified against the pinned dependencies; the native Quit gesture has not
been exercised. Unexpected app loss retains the runner's parent-loss contract.
An expired deadline remains unverified/incomplete, not a clean acknowledgement.
The log bridge polls every **50 ms** and is joined during shutdown; that interval
is not a join timeout. File-log shutdown follows below and never determines the
run result.

## Logs and retention

Rust diagnostics and explicitly imported Script records share structured event
identity, source, severity, run attribution, and sanitized fields before fan-out.
A process-local Rust subscriber does not implicitly collect child logs. The GUI
and file outputs are independent; delivery to one is not proof of delivery to the
other, and neither is the authoritative result channel.

- **GUI retained-item limit:** integer **1–10,000**, initially **1,000**, saved
  through **Save settings** as an application setting, not a profile value.
  Lowering it immediately keeps only the newest items. Invalid input preserves
  the previous valid limit.
- **Delivery queues:** at most **256 records each** for the application GUI and
  file queues. A larger display limit does not enlarge these queues.
- **Files:** sanitized JSONL under the data root's `logs/`, rotating across at
  most **three 1 MiB files**. Changing the GUI limit does not delete file records.
- **Failure accounting:** source/transport loss, GUI display eviction, file queue
  loss, and writer I/O failure remain distinct. A failed file sink stops without
  recursive logging or retry; GUI delivery, Stop, and result retrieval remain
  independent.
  An existing active file with an incomplete final JSONL line is also refused;
  preserve or move that file before retrying rather than appending to a damaged
  record.
- **Shutdown:** the retained writer guard waits at most **500 ms**. Pending,
  failed, or incomplete file delivery is also reported through an independent,
  sanitized stderr diagnostic with its own **50 ms** wait bound. Expired flush
  and abrupt exit do not guarantee persistence.

Keep app data, execution diagnostics, and any private fixture copies out of
public commits and CI artifacts. Redaction is not authorization to publish raw
execution evidence. The [logging decision](adr/0002-desktop-runner-boundary.md#logging)
owns the sink and shutdown design.

## Local GUI acceptance

This is a procedure, not a claim that the complete UI workflow has passed. Use
the actual macOS application and an isolated private data root, with no native
capture/input permission or game target. Record observed results and unexecuted
scope separately from [hosted build/core checks](ci.md#local-check-scope).

1. Inspect the shipped TypeScript package and confirm that selection alone does
   not start a run. Save both package presets as named application profiles:

   | Preset | Ordered priorities | Actions | Expected controlled key |
   | --- | --- | --- | --- |
   | `template-first` | `template`, `ocr` | template `A`, OCR `B` | `A` |
   | `ocr-first` | `ocr`, `template` | template `C`, OCR `D` | `D` |

   The package preset files also carry the remaining declared values. Do not
   replace the distinct expected decisions with a generic successful status.
2. Restart with the same data root. Reopen and run both saved profiles repeatedly;
   check their declared decisions, receipts, distinct run IDs, and cleanup. Rename
   one and check that its ID/values survive restart; delete only a disposable
   profile and confirm unrelated profiles remain.
3. During actual controlled work, edit the next-run draft and attempt a repeated
   Start. Confirm no second child is admitted and the active run retains its
   captured values. After terminal cleanup, Start again and check the new values.
4. Exercise Stop during a real controlled wait or VM workload, then start another
   run after cleanup. Close the window during an active run; observe application
   exit and owned-child termination, then reopen and run again. Do not infer clean
   cleanup from a closed window or Stop acknowledgement alone.
5. Use private controlled fixture copies to inspect an invalid package, submit an
   invalid nested-object replacement, change a schema after inspection, and
   trigger an original-source TypeScript error. Verify explicit refusal or source
   diagnostics, preserved saved data, and an independent cleanup outcome. Do not
   edit tracked fixtures or enable native authority to manufacture these cases.
6. Change GUI retention, lower it below displayed history, and restart. Check the
   newest-item bound, persistence, invalid-value refusal, structured Rust/Script
   attribution, and independent file output. Exercise queue pressure/file failure
   only in the disposable data root; confirm loss/failure counters do not erase
   results or disable Stop.

### Recorded-replay acceptance

Use explicitly authorized saved frames and separately installed accepted models,
runtime, and libraries. Declare independent recognition/decision expectations
and the finite bounds before measurement. Keep raw paths, images, text, and
records private; do not create new game captures to satisfy this procedure.

1. Save the environment, Close/reopen, and Check with and without a corpus.
   Confirm file validation is not labeled initialization. Use a private package
   that would throw during module evaluation to confirm Check never evaluates it.
2. Run saved template-first and OCR-first profiles through the real WebView.
   Verify their distinct declared decisions, real newer-frame postconditions,
   correlated child/environment/corpus identities, and independent cleanup.
   Repeat only after settlement; no command mocks or generated recognition answers.
3. Replace an owned resource at the same path, then Check/Start again. Confirm
   revalidation, truthful failure stages, and preserved settings/profile bytes.
   Edit saved settings/profile values during work and verify current-run isolation.
   Test missing engine prerequisites without disrupting controlled execution.
4. Stop during actual SDK initialization, recognition, and query work. Close
   during an active operation, observe settlement/reaping, then reopen. Exercise
   file-output failure or log pressure without losing Stop or terminal evidence.
   Record forced/incomplete outcomes honestly; do not retry them into a pass.
5. Trigger an original-source exception carrying real recognition detail.
   Confirm the ordinary result shows only safe classification, with full bounded
   diagnostics available only through explicit private disclosure.

Hosted CI does not execute these steps. Live Windows/macOS native qualification,
R6, background compatibility, and release packaging remain separate obligations.
