# Controlled macOS desktop

MadoMata's development application uses a trusted Tauri/React WebView for package
inspection, profile editing, run control, and structured logs. Package code runs
in the existing supervised QuickJS runner, never in the WebView. JavaScript and
TypeScript packages use the **controlled, non-native input sink**: no screen
capture, OCR installation, game launch, focus changes, permission prompts, or OS
input are required or authorized.

Only macOS desktop development is supported in this Change. Linux and Windows
CI check the frontend and shell-independent Rust core, not additional desktop
platform support. Native adoption, R6, per-game background compatibility, replay
prerequisites, and release distribution remain separate unresolved work. See the
[native prerequisites](runtime-native.md) before any native operation.

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
cargo +1.98.1 build --locked --manifest-path tools/runtime-comparison/Cargo.toml
npm run build --prefix apps/desktop
cargo +1.98.1 build --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --features custom-protocol
apps/desktop/src-tauri/target/debug/mado-mata-desktop
```

`custom-protocol` serves the built application assets; no Vite server is needed.
These commands do not enable the test-only `webdriver` feature. Normal builds
have no automation listener, and release builds do not register that listener.
The [shell decision](adr/0002-desktop-runner-boundary.md) records the limited
WKWebView/real-child/Stop/window-close evidence, not completed workflow acceptance.

This is a checkout build, **not a relocatable app bundle or release installer**.
The application owns a fixed runner path at
`tools/runtime-comparison/target/debug/mado-runtime-comparison` and uses the
trusted compiler installed under `tools/runtime-comparison/compiler`. Build both
in this checkout before launching; rebuilding only the app is insufficient after
runtime changes. Do not redirect the runtime build with `CARGO_TARGET_DIR` or a
cross-compilation target when following these run instructions. CI may use other
build directories for compilation checks, but a real app run still needs its
fixed runner/compiler paths. Moving a binary alone does not supply them.

By default, profiles, settings, and logs live in Tauri's application-local data
directory. For isolated acceptance work, use a dedicated private directory:

```sh
apps/desktop/src-tauri/target/debug/mado-mata-desktop --data-dir "$HOME/Library/Application Support/MadoMata-acceptance"
```

`--data-dir PATH` selects the data root, not the runner, compiler, package, or an
input route. Keep it outside package source and public tracked files. Reuse the
same root when checking restart persistence; use a different private root to
isolate another session without deleting existing data.

## Inspect, edit, and save profiles

1. Enter the selected package directory and choose **Inspect**. For the shipped
   controlled example, use the absolute path to
   `tools/runtime-comparison/fixtures/typescript` in this checkout. Inspection
   validates inventory, schema, assets, and static dependencies without executing
   package automation. A remembered package path is only a location hint and is
   revalidated, not an authority grant.
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

Saved profiles are versioned and bind a stable ID/name, package ID, schema
identity, and values. They are separate files under `profiles/`, not edits to the
package's declared preset files. A relocated compatible package can reuse its
profiles after validation. Incompatible schemas are reported separately from
compatible profiles, and their files remain untouched. Malformed data or
unsupported storage versions are refused without silent reset or migration.
Create a new compatible profile deliberately; do not rewrite stored identity
fields merely to bypass refusal.

Writes validate first and use a same-directory temporary file plus atomic
replacement. A failed save preserves the previous valid file. Storage is bounded
to **64 profiles**, **64 KiB per encoded profile**, and **1 MiB total profile
bytes**. Settings are separately stored in `settings.json`. Portable profile
values exclude executable/model paths, credentials, permission grants, and input
authority; remembered local package paths belong only in application settings.

An interrupted save may leave a `.pending` file. The application preserves it
instead of silently discarding evidence or overwriting it. Close the app and move
that file outside the data root before explicitly retrying; keep the prior
valid `.json` file intact.

## Start, Stop, and results

**Start** submits the current explicit draft. Unmodified saved values retain the
saved profile ID; edited values run as a draft until saved. Rust revalidates the
package/profile and captures an immutable run, including dependency content and
finite execution limits. A stale inspection is not a permission to execute changed
source. Only one preparing/running/stopping reservation is admitted.

Edits to a draft, a later profile save, or changes to package source after capture
affect only the next run. **Stop** addresses the active run independently of
ordinary logs. Wait for its terminal outcome and owned-child cleanup before
starting again. A late record from an earlier run must not replace a newer run's
state.

The UI keeps entry outcome, receipts, cancellation progress, and cleanup outcome
separate. A Stop request is not proof of physical cleanup; a returned entry with
forced/incomplete cleanup is not success. TypeScript failures retain original
source attribution and host causes rather than becoming generic UI errors.

The controlled plan bounds an invocation to **10 s**, cleanup to **1 s**, and
containment to **2 s**. Controller shutdown waits at most **14 s** for its owned
worker. Closing the window requests shutdown off the UI thread; unexpected app
loss uses the runner's existing parent-loss contract. An expired deadline remains
an unverified/incomplete-cleanup outcome, not a clean acknowledgement. File-log
shutdown has its own bound below and never determines the run result.

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
- **Shutdown:** the retained writer guard waits at most **500 ms**. Expired flush
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

Saved-frame replay needs explicitly configured engine/model prerequisites; this
controlled application does not synthesize replay or OCR success when they are
missing. Full native Windows/macOS qualification, R6, background compatibility,
and release packaging are not covered by this procedure or a green hosted build.
