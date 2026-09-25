# macOS desktop: controlled runs and recorded replay

MadoMata's trusted Tauri/React WebView provides directory-package authoring,
inspection, profile editing, run control, App-local OCR configuration, and
structured logs. Package code runs in the supervised QuickJS runner, never in
the WebView. Controlled runs need no OCR installation. Optional recorded replay uses real engine
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

The default configuration root is `$HOME/.config/mado-mata`, not Tauri's
application-local data directory. For isolated acceptance, select a private root:

```sh
apps/desktop/src-tauri/target/debug/mado-mata-desktop --data-dir "$HOME/.config/mado-mata-acceptance"
```

`--data-dir PATH` selects the root explicitly and skips historical-root discovery.
It does not select the runner, compiler, package, or an input route. Keep the root
outside package source and public tracked files. Reuse it for restart checks;
choose another private root to isolate work without deleting existing data.
An absent root is not created merely by launching the application.

New application-owned directories use private Unix permissions. Existing managed
directories or files with group/other access, unsafe types, or links are refused
without changing their modes. Root and configuration failures open **Recovery**;
the desktop performs no automatic repair, elevation, or fallback to another root.
See [ADR 0005](adr/0005-desktop-configuration-recovery.md) for the configuration
ownership and reconstruction boundary.

## Setup and Recovery

**Loading** waits for the selected root and saved configuration to be read; it
does not expose a default settings draft or start normal application polling.
A missing root or missing `settings.json` leads to **Setup**. Choose **Saved
language** and an optional backup directory, then click **Initialize**. The host
validates before publishing missing settings without replacement. Existing data
is not cleared, and a settings file that appears meanwhile is read, not replaced.
Initialization creates no Tab, inspects no package, and starts no run or OCR work.

A present but unreadable, malformed, unsupported, or invalid settings file leads
to **Recovery**, not Setup. The screen retains the selected root, failing stage,
and original cause until an authoritative action resolves them. Editing a form
or dismissing an action error does not clear the bootstrap failure. App settings
cannot save replacement defaults over a failed read.

**Display language for this screen** is temporary during Setup or Recovery.
It neither reads language from a failed document nor writes settings. Setup's
**Saved language** is a separate explicit choice written only by Initialize;
successfully loaded saved settings determine normal application presentation.

After preserving the affected data and repairing files or permissions externally,
choose **Retry** to reread and reconstruct without restarting. A later settings
or catalog failure retains the existing Application, polling, and owner-bound
**Stop**. Reconstruction waits for active operations and commands to settle.
Whenever an Application is retained, Retry requires explicit session-disposal
confirmation even if no profile is dirty; Retry, Restore and interrupted-restore
recovery each have their own confirmation. Successful reconstruction drops drafts,
transient results, Last check, cards, and the in-memory log buffer; file logs and
saved configuration remain. It assigns fresh session identities and ignores
responses from the retired generation. From the moment a reconstructing action is
dispatched until its resulting status is adopted, the desktop dispatches no new
poll and first finishes ingesting the poll already in flight, so the rebuilt
Application's first events are never drained into the retired session. A refused
action resumes polling on the retained session unchanged.

If shutdown reports `LoggingShutdown`, its incomplete writer state is sticky:
use **Exit** or **Close window**, then relaunch. Retry cannot start a second writer
or turn a previous timeout into a successful flush. This is distinct from a
repairable settings read failure.

### Import the historical default root

Only when the new default root is absent, launch may offer the historical
`$HOME/Library/Application Support/dev.madomata.desktop` root. Choose **Import
historical root**, or explicitly confirm **Start fresh without importing the
historical root** before Initialize. Explicit `--data-dir` roots do not discover
or import this location.

Import validates and copies only recognized `settings.json` and legacy
`profiles/*.json` into private staging, then publishes the complete root without
replacement. It refuses an existing destination, source changes, invalid owned
files, and interrupted `.pending` writes; it neither merges nor deletes the
source. Limits are **32 KiB** for settings, **64 profiles**, **64 KiB per profile**,
**1 MiB total profile bytes**, and **128 entries** in the legacy profile directory.
Logs, backups, package payloads, and unrecognized files stay in the old root.
Imported legacy profiles still require the separate per-workspace import below;
the historical package-location hint does not create or bind a Tab.

Legacy-root import does not support symlinked destination ancestors and refuses
them even where Initialize can use the same location. This environment is outside
the supported import contract; the application does not rewrite links, repair
the machine's directory layout, or silently choose a different destination.

## Source layout

TSX files under `apps/desktop/src/` are grouped by responsibility:

| Path | Responsibility |
| --- | --- |
| `main.tsx`, `App.tsx` | Bootstrap and application composition |
| `components/` | Shared selection, schema forms, results, notifications, and named-workspace dialogs/navigation |
| `pages/` | Setup/Recovery, unbound-package guidance, Edit, Run, and Logs views |
| `settings/` | App settings dialog and OCR environment view |
| `locales/` | Bundled English/Japanese JSON text resources |
| `i18n.ts`, `ui-messages.ts`, `locale.tsx` | Typed formatting and saved-locale presentation |
| `authoring.ts` | Per-file drafts/history, revision-bound save responses, search, and diagnostic navigation |

Imports point directly to the owning file. Non-visual TypeScript modules and
their tests remain at the source root.

### Rust ownership

The shell-independent core retains its public root namespaces:

| Root | Private children |
| --- | --- |
| `application.rs` | `workspaces` owns Tab/session transitions; `authoring` owns the global Edit lease and validation/close lifecycle; `profiles` owns ordinary profile commands and desktop value checks; `recovery` owns schema reconciliation, repair/reset authority, and explicit binding retry; `targets` owns target commands; `operations` owns execution, collection, and shutdown |
| `storage.rs` | `settings`, `tabs`, `profiles`, and `targets` own their records and persistence; `fs` owns bounded reads, safe paths, and atomic file publication |
| `authoring.rs` | `catalog` owns prospective declarations; `publication` owns source revisions, changed-file staging, and interrupted-publication recovery |

`Application` retains command admission and authoritative workspace/Store locks.
`Store` and `ProfileStore` retain owner and shared-budget coordination.
Bootstrap, snapshots, restore, configuration primitives, logging, and target
metadata validation keep their existing separate modules. Tests follow their
behavior owner; cross-owner tests stay at the root. Module extraction does not
change persisted formats, filesystem protections, or runtime authority.

## Edit directory packages

Create or open a package from an unbound workspace, or choose **Edit package**
on Run control. Supported sources are ordinary **TypeScript or JavaScript
directories**, with at most **128 declared files and 1 MiB total content**.
Create writes a runnable TypeScript starter. Duplicate copies the saved source
under a new package ID and updates package ownership in each packaged preset. It does not copy App
settings, named-workspace profiles, target bindings, or execution results.
Neither action inspects, binds, or runs the package.

- Create and Duplicate require a missing destination beneath an existing parent.
  Existing packages and the App data root cannot contain the destination.
  Source and App data roots must not overlap in either direction.
- The tree lists manifest-owned files. Text files have independent drafts,
  selection, undo/redo, literal search, and line numbers. Assets are listed,
  not decoded or edited as text. The native textarea handles composition events;
  no editor dependency or package-supplied WebView code is loaded.
- **Manage files** adds sources, presets, JSON assets, or source maps and
  coordinates their manifest declarations. Rename changes declarations, not
  source imports. Required entries/schema/presets cannot be removed. Manifest
  JSON editing must preserve a safe, coherent declared-file catalog and package
  identity; unsafe paths, links, collisions, and undeclared files are refused.
- **Save file** and **Save all** write source without running or validating it.
  Syntax-invalid source, schema, or preset text can be saved for later repair;
  it is not an executable inventory. A later edit stays dirty if an earlier Save
  response arrives afterward.
- **Validate** checks one saved revision through the existing inventory and
  trusted compiler without evaluating package code. Unsaved text is excluded.
  Diagnostics identify their revision and link to declared source locations.
  The finite validation child occupies the existing operation slot; **Stop**
  requests cancellation and retains ownership until the worker settles.
- One Edit session owns the application. Ordinary **Start**, independent OCR
  **Check**, a second editor, and configuration reconstruction are refused.
  Idle Edit has no timed runner. Navigation remains available; the owner strip
  and **Return to Edit** preserve the session across workspaces and dialogs.
- Exit, Duplicate, closing the workspace, and closing the application resolve
  dirty files with **Save / Discard / Cancel**. Cancel keeps the lease and drafts.
  Confirmed application close uses bounded shutdown and preserves incomplete
  containment outcomes; closing is not proof of successful cleanup.
- **Exit Edit**, then explicitly **Inspect/Reinspect** before Start. Selections
  for every workspace sharing an edited source are invalidated. Saved local
  profiles remain untouched; inspection uses the existing
  [profile reconciliation and repair flow](#recover-profiles-after-a-schema-change).

### Source conflicts and interrupted saves

Every publication checks the owner, source revision, and current disk bytes.
An external edit is refused rather than overwritten. **Refresh** deliberately
reads the new revision while keeping dirty text; compare it before saving, or
discard that file's draft to adopt the disk version. A committed Save whose
follow-up refresh failed remains committed; refresh before writing again.

Publication stages only changed files beside their destinations. A private,
bounded journal under `data_root/authoring/` records old/new bytes outside package
inventory. A pending journal blocks package admission after restart. Use the
displayed **Recover interrupted save** action for its recorded package: recovery
rolls forward only matching old/new bytes and preserves conflicting external
content. Do not delete the journal to bypass refusal. Configuration snapshots
exclude both package source and this source-publication journal.

Interruption regressions cover process-level failures, not physical power loss.
Windows core checks do not qualify crash durability or an additional desktop OS;
directory sync retains the [existing platform limitation](adr/0005-desktop-configuration-recovery.md).
Custom archives, remote download, image/OCR authoring, native capture/input, and
the broader practical Edit readiness gate remain separate work.

## Package workspaces and App settings

A Workspace is an open session of a saved **Tab**, not a game attachment.
**+** opens **New workspace**, asking for an internal name and an optional display name:

- **Internal name:** **1–64 ASCII letters, digits `0-9`, `_`, or `-`**, with no
  spaces, trimming, or automatic suffix. Digits may appear anywhere. It is unique
  across open and closed saved Tabs without regard to ASCII case; entered case
  is preserved.
- **Display name (optional):** leave empty to use the internal name. An explicit
  name must contain **1–80 Unicode scalar values**, be nonblank and have no control
  characters; entered text is preserved. The effective name is saved, so the
  fallback survives close/reopen and restart. Duplicate display names are allowed
  and disambiguated with internal names. Both names are immutable here.

Creation immediately saves an open, unbound Tab. It requires no package path,
OCR environment, or target. At most **eight Tabs are open** and **64 are saved**;
reaching a limit never evicts an existing Tab. Names are not profile names, run
IDs, or host-issued session identities.

Saved-open Tabs return at launch with fresh sessions. Their selected directory
references are reinspected against real package inventory before package commands
become available. Saved-closed Tabs stay closed; choose **Saved workspaces** in
the Workspace dropdown and **Reopen** to create a fresh session. Names, package
references, and saved profiles persist; unsaved drafts, execution choices,
navigation, results, and runs do not.

An unbound Tab provides real directory-package **Create/Open for Edit** actions
and a separate **Inspect a local package directory** action. Custom-package
archive loading/execution, remote download, and a package-catalog UI remain
unsupported. A missing or changed saved source shows **Saved source unavailable** with the saved
directory reference displayed and prefilled in the Inspect field; a custom archive
reference shows **Unsupported saved source** with its path displayed but never
prefilled. Both preserve the reference rather than inventing inventory,
substituting defaults, or claiming that no package exists, and the displayed
reference grants no package command. A source failure belongs to its Tab;
healthy Tabs remain usable.

Different Tabs may inspect the same canonical source and remain independent.
Inspecting that source does not activate or merge another Tab. Profile drafts,
saved profiles, execution choices, and Run/Logs navigation belong to their owner.

The summary shows **Running n/ALL**, where ALL is the number of open workspaces.
**Errors n** appears only while one or more workspaces need attention; it counts
workspaces, not log entries. Individual states appear inside the styled dropdown,
not as an always-visible row. Small indicators beside secondary status text
distinguish ready, unsaved, attention, and active work using the existing palette.
Text remains available and CSS animation respects reduced-motion preferences.

Use arrow keys, Home/End, or type-ahead to move through the dropdown without
switching workspaces. Enter/Space selects; Escape cancels and returns focus to the
trigger. Tab reaches enabled footer actions, including **Saved workspaces** and
the selected workspace's close action; Shift+Tab returns through those actions.
Leaving the popup dismisses it.

Profile, preset, execution, schema-enum, App settings, and log-filter controls use
the same styled single-select as workspace navigation. Arrow keys, Home/End, and
type-ahead move the active option; Enter/Space commits and Escape cancels. Form
selectors also commit on Tab. Workspace Tab never switches workspaces by itself.
Keyboard focus stays on the trigger while the active option is indicated in the
popup; disabled options cannot be selected. Empty and invalid current values are
not silently replaced.

Popups scroll within the viewport and open above the trigger when needed. Inside
App settings they remain within the native modal, outside its content scroller.
Escape dismisses an open selector without cancelling the settings draft.

**Run control** (bound) or **Guidance** (unbound) and **Logs** beside the dropdown
belong to the selected workspace.
Switching workspaces does not transfer an operation. There is **one application-wide
operation slot** for Start and OCR Check; the owner and Stop remain available
across navigation and inside App settings. The host retains the latest terminal
outcome for each open workspace independently of log retention.

Use **Close selected workspace** in the dropdown footer to close the current
workspace. A touched unsaved draft requires confirmation. An active owner or a
workspace command still in progress must settle before closure.
Closing persists the Tab's closed state before removing its session. It keeps
the saved Tab, profiles, package references, and package source files.
**Reinspect** validates again and resets the draft to schema defaults with a new
selection revision. Prior results remain labeled with their original revision;
they are not validation of the new selection. Reinspecting a touched draft asks
for inline confirmation; **Keep draft** returns focus to **Reinspect**. The host
runs one workspace command at a time; while one is in flight, profile and execution
controls and workspace command buttons are disabled. Schema options remain editable;
Stop and navigation stay available. The reason is shown in the issuing workspace,
the workspace dropdown, and the **+** dialog.

Use **Application → App settings** for Display, Notifications, OCR environment,
Logs, and Backups.
Categories share one draft and one **Save changes** action. **Cancel**, the
dialog close button, or **Escape** discards unsaved edits; merely opening the
dialog initializes no recognition backend. A category whose fields are invalid
is marked in the category list and named in the footer, including while another
category is shown. **Save changes** and **Check saved environment** wait while a
package or workspace command is in flight; the Check panel names the reason
beside its button, the footer names it once the draft has changes, and
**Cancel** stays available. Save atomically updates editable App preferences.
Inspection writes the owning Tab's package reference, not an App package hint;
the legacy `package_path` field is preserved but is not used to reopen packages.
Active operations keep the settings they already captured.
**Application → Application logs** opens application-wide
diagnostics; **Close window** follows the bounded shutdown path.

### Display language

Choose **Display → Language → English / 日本語**, then **Save changes**.
The saved language applies immediately without restarting workspaces or runs.
Cancel, Escape, and failed Save keep the previous language; edits made during a
pending Save remain unsaved. Language does not change OCR recognition settings,
profile values, or input authority.

Setup initially offers English; Initialize writes the explicitly chosen saved
language. Older valid settings without `locale` use English without a read-time
rewrite. An invalid present locale is refused without replacing the file.
A failed initial settings read leads to Recovery with App settings unavailable;
repair and Retry, or deliberately restore a supported snapshot. A failed read is
not a new installation. Older strict binaries may reject newly saved fields;
preserve the configuration before rollback and use a compatible backup rather
than resetting profiles or removing unrelated settings.

UI labels, help, accessible names, and frontend validation are localized.
All log bodies, log-derived notification bodies, backend/SDK errors, and raw
diagnostics stay original after existing privacy and size limits. Package text,
names, IDs, paths, and values are not translated.

Translations live in JSON; formatting and typed parameters stay in TypeScript.
The existing `npm test --prefix apps/desktop` checks cross-language keys and
placeholders. See [ADR 0004](adr/0004-desktop-localization-resources.md).

## Local target configuration

The **Target** section on a bound workspace's Run page stores macOS configuration
for that **Tab and package only**. The package must declare portable target intent
as described in [the manifest contract](runtime-comparison.md#optional-portable-target-declaration).
A targetless package still runs existing controlled/replay workflows; a missing,
invalid, or incompatible local target is not an execution prerequisite.

1. For the **actual game**, choose **Application bundle (.app)** and
   **Choose application…** to select the outer application, including a supported
   iOS-on-Mac wrapper. Do not navigate to its internal executable. Cancel keeps
   the draft. Manual absolute paths and direct executable configuration remain
   available. A separate bundle launcher uses the same picker; it does not
   identify or replace the game.
2. Add arguments as ordered individual fields. Empty fields remain empty
   arguments; spaces and shell syntax are literal, never split or expanded.
   Optionally enter an absolute working directory. Supply an exact window title
   when the package leaves it local; a package-required title is read-only.
3. Explicitly select the future input policy. Process-directed input requires a
   pointer mode: Core Graphics permits either focus policy; AppKit background
   requires preserved focus. System input requires an already focused target
   and has no process-pointer mode. Click hold is **0–1000 ms**.
4. **Check configuration** examines the current unsaved draft without writing.
   **Save binding** repeats validation and metadata resolution before an atomic
   write. Both inspect only filesystem metadata: executable accessibility,
   bounded XML/binary bundle metadata, public Foundation resolution, contained
   bundle executable, working directory, and canonical paths. No executable or
   helper is launched. A declared `target.macos.bundle_id` must match the actual
   game's derived identifier; it never constrains a separate launcher.
5. Review changed canonical locations explicitly. A direct Save also opens this
   review without writing; its warning is not a metadata-check failure.
   **Save reviewed resolution** resolves them again and refuses a different
   result. Merely checking cannot replace saved locations. In-place executable
   updates at the same canonical location remain compatible; this is not a
   signed-binary or content check.

The native picker is an asynchronous sheet, available only while Run/OCR is
idle. While it is open, new Run/OCR and another picker are refused; state polling
continues. A late selection cannot overwrite a changed draft, another Tab, or a
closed/reinspected owner. Bundle metadata is reread on every Check/Save:
same-process changes to executable names and identifiers do not reuse cached
values. Participating plists remain bounded to **64 KiB**, **8,192 events**, and
**32 nesting levels**. Unsupported alternate metadata locations, including
`Info-macos.plist`, are refused before Foundation reads them. Bundle resolution
is macOS-only; portable record/snapshot validation remains cross-platform.

Required values are listed at the start of the form. Empty fields and unselected
options are not marked as errors. Check and Save stay disabled until the required
values form a valid configuration; populated invalid values and host validation
failures are still shown.

Neither action finds processes/windows, connects, captures, performs OCR, sends
input, changes focus, or requests permissions. A passed check is **configuration
metadata only**, not runtime identity, authority, or native acceptance. Native
Start remains refused; R6 and initial Windows/macOS qualification remain open.

Paths are absolute UTF-8 strings of at most **4,096 bytes**. Titles are nonempty
exact strings of at most **512 bytes**, not patterns. Arguments permit at most
**32 entries**, **1,024 bytes each**, **8,192 bytes total**; control characters
are refused, while an empty argument is valid. Store no credentials or secrets:
there is no automatic secret detector. Full values appear only in the deliberate
local form and private details, not routine logs or notifications. Configuration
and backups contain these private values and must not be published.

Each Tab has its own binding and draft, even for the same package. Navigation
preserves drafts; edits invalidate a check's applicability. Late results remain
attributed to their originating configuration. Reinspect/restart/reopen never
restore a checked or connected status. Closing or reinspecting a touched target
draft uses the existing discard confirmation alongside profile edits.

**Reload saved view · keep draft** reconciles only the issuing owner.
**Discard edits** loads the latest compatible saved values, or a blank form.
A stale record revision/binding ID is refused: Reload before another mutation,
then deliberately keep the draft or Discard. If persistence succeeds but its
follow-up read fails, the write remains completed; repair and retry the read,
not the completed mutation. Draft edits retain the completed-write notice while
that read is outstanding; an edit after recovery retires it. Malformed target
storage is reported independently of profiles. Preserve the file before external
repair.

Changing/removing the portable declaration retains an incompatible record
without applying it to the form. With a current declaration, Save deliberately
replaces it after validation. Without one, declare a target in the manifest and
Reinspect before replacing it. Confirmed **Remove binding** clears only this
owner's binding and increments its record revision. When a declaration makes the
form available, its current draft remains until Discard. Removal never deletes
installed programs, package source, profiles, or another Tab's configuration.
Check, Save, and Remove refuse active runs/OCR checks; read, navigation, polling,
and Stop retain their existing roles.

Use one application instance per data root. Revision checks protect in-process
stale requests; they are not cross-process locking or protection against
concurrent external filesystem edits.

### Check running application

After saving a compatible application-bundle binding and settling its owner
refresh, choose **Check running application**. This separate read-only action
uses the selected installation's derived bundle ID to query AppKit, not a window
title, basename, launcher, or temporary-directory search. It does not launch,
activate, attach, capture, perform OCR/input, or request permissions. Direct
executable selections and non-macOS hosts are unsupported.

The timestamped result is an observation, never connected, ready, or authorized:

- **Exact installed executable** requires stable process lifetime and canonical
  executable equality. Signed code must also pass the static/dynamic signature,
  designated-requirement, and executing-architecture code-identity checks.
- **Signed application correspondence** permits a different runtime path only
  with those checks and a matching nonempty Team ID. **Original-copy attribution
  is unavailable**: an indistinguishable signed copy cannot be assigned an
  original installation by these checks.
- No candidate, ambiguity, unverifiable evidence, cancellation, and timeout are
  distinct outcomes. Multiple verified candidates, or an additional candidate
  that cannot be safely excluded, prevent unique success. Old running signed
  code does not match a replaced installed build, even at the same path.

The current public-API provider cannot independently prove that a live image is
unsigned when its on-disk file may have been replaced. Such evidence is
**unverifiable**, not an unsigned path-only success. Signature errors are never
downgraded to unsigned.

The operation considers at most **64 candidates**, has a **5-second monotonic
visible deadline**, and bounds deliberate private diagnostics to **64 KiB**.
**Cancel** invalidates publication; it does not physically interrupt synchronous
OS calls. One worker remains occupied until the OS read returns, including
across application reconstruction. Navigation, polling, run Stop, and shutdown
do not wait for that worker under shared locks.

Edits, Save/Remove, owner changes, reinspection, and root/restore transitions
invalidate applicability. Reopening never restores an observation. Failure does
not roll back a completed binding write. Process paths, lifetime and signature
details remain transient private diagnostics: they do not enter `target.config`,
profiles, snapshots, or routine logs. Routine check outcomes retain only workspace
attribution, action, bounded status, host stage (`admission`, `observation`, or
`publication`), and fault category; they do not copy private diagnostics or fault
text. Runtime relocation never rewrites the saved installation. Native Start
remains refused; Windows target work is deferred, and initial both-OS qualification
and R6 remain unresolved.


## Inspect, edit, and save profiles

1. Create or reopen a named workspace, enter a local package directory in its
   guidance page, and choose **Inspect**. For the shipped controlled example, use
   the absolute path to `tools/runtime-comparison/fixtures/typescript` in this
   checkout. Inspection validates inventory, schema, assets, and static
   dependencies without executing package automation. JavaScript syntax and
   module bindings are checked without evaluating module bodies, including
   requested executable `.d.ts` dependencies. Successful binding requires saving
   the owning Tab's reference before publishing the new selection. A failed
   binding write retains the durable reference, any prior inspected selection,
   and its unsaved draft; it never publishes the attempted replacement as runnable.
   No remembered path grants authority.
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
identity, and values. Each belongs to one Tab/package scope under
`tabs/<internal_name>/<package_id>/<profile_id>.config`, not a global catalog or
the package's preset files. Saving, renaming, or deleting refreshes only that
owner's catalog; another Tab using the same source retains its own profiles and
draft. A relocated source with the same package ID can reuse that Tab's profiles
only after compatibility validation and a durable Tab-reference update. Changing
the selected package does not delete other saved package settings.

### Recover profiles after a schema change

Explicit **Inspect** or **Reinspect** reconciles safe owned profiles with the
captured schema. Supplied values are retained, and only absent top-level options
with declared defaults are added. Optional options without defaults stay absent.
Present nested objects and arrays are complete replacements: recovery does not
recursively fill them, coerce types, guess renamed fields, remove unknown fields,
or execute package-provided migration code. Already-compatible profiles are not
rewritten. Startup, catalog reads, Validate, and Start do not perform recovery
writes.

Each successful replacement preserves the profile's ID, name, package ID, and
version while updating its schema identity. Profiles commit independently: a
later profile, binding, or catalog-refresh failure does not undo an earlier save.
The result distinguishes saved profiles, profiles needing repair, and storage
failures. A valid profile can remain usable on an in-place source while another
profile needs repair.

The recovery editor is available on the Run or workspace guidance page. It
shows the original values and structured field issue against the captured schema.
Stored strings under numeric fields remain type mismatches until deliberately
replaced or edited; an untouched numeric-looking string is not converted on Save.
Repair changes only the selected profile. Reset requires a confirmation scoped
to that Tab and recovery context, ignores the discarded draft's parse errors, and
uses the schema's top-level defaults; cancellation changes nothing. If those
defaults cannot form a valid profile, reset preserves the original file and
returns an unsaved draft with the invalid value or individual-profile limit
identified. Aggregate storage exhaustion is a publication failure, not an
incomplete-default draft. Complete a returned draft and save deliberately.

When the old source has moved, inspecting a same-ID replacement does not require
the old directory to exist. Rejected profiles produce a **non-runnable recovery
candidate**, not a new binding. Repair or reset never binds it implicitly.
Explicitly retry binding after recovery; the host rechecks the captured source,
strict profile catalog, and durable Tab-reference write before publishing a
normal selection. Discarding or superseding a candidate invalidates its recovery
context but does not undo completed saves. Recovery never consumes the global
operation slot or changes an existing operation's Stop authority.

Failed source binding has an explicit **Retry binding** action even for a first
inspection or an in-place update. Its failure is separate from an earlier
saved-source inspection error. In-place retry does not require unrelated rejected
profiles to be repaired; their recovery access and unsaved drafts remain available.
If a failed binding retained a previous selection, repairing the attempted
package cannot replace that selection's draft or another package/schema's catalog.
Discard leaves the retained selection authoritative; it does not recreate one
that a recovery-only candidate already invalidated.

Before each recovery save the host re-reads the original file under storage
serialization. A changed or missing original is refused rather than overwritten
or recreated; Reinspect to obtain a current context. Malformed, foreign,
unsupported-version, unsafe-value, pending-write, and unsafe-path records remain
storage faults, not editable synthetic profiles. Preserve affected files before
external repair; do not rewrite identity fields to bypass refusal.

Navigation and late replies retain their originating workspace/profile identity.
Newer editor drafts are not replaced by older responses. If a write succeeds but
its follow-up read fails, repair and retry the read rather than repeating the
completed mutation. Ordinary listing failures preserve the owner's known catalog
and draft. Inspect/Reinspect asks before discarding edited recovery values,
including Enter in the guidance path field; a confirmed new inspection replaces
its own editor context.

### Configuration files and limits

Paths below are relative to the selected root:

| Path | Owner and bound |
| --- | --- |
| `settings.json` | App preferences; **32 KiB** |
| `tabs/<internal_name>/tab.config` | Version, names, open state, package references, and selected package ID; **128 KiB**, **16 references per Tab** |
| `tabs/<internal_name>/<package_id>/<profile_id>.config` | One saved profile; **64 KiB**, **64 profiles / 1 MiB per Tab/package** |
| `tabs/<internal_name>/<package_id>/target.config` | Versioned local target record, revision, and optional binding; **64 KiB** |
| `profiles/<profile_id>.json` | Recognized legacy profiles; explicit import only |
| `logs/` | File diagnostics; not configuration |
| `backups/app.config.<unix_time>` | Default manual snapshot destination |

The managed set is bounded to **4,096 files / 16 MiB**, across open and closed
Tabs, settings, and recognized legacy files. Bounded store directories permit
at most **128 entries**; snapshot enumeration permits **16,384 entries** overall.
Source paths and explicit backup destinations are absolute UTF-8 paths of at most
**4,096 bytes**. Filesystem case/normalization aliases and containing-identity
mismatches are refused rather than selecting another owner's data.

Writes validate first and use a same-directory temporary file plus atomic
publication. A failed save preserves the previous valid file. Portable profile
values exclude executable/model paths, credentials, permission grants, and input
authority. OCR configuration belongs to App settings; package locations belong
to Tab references. Neither is execution authority.

Only `.config` and `.pending` entries belong to the active package profile store.
`target.config` and `target.pending` belong to the separate target store; malformed
or interrupted target data does not block profile loading or controlled/replay
Start. Other regular entries such as filesystem metadata are ignored without
being opened but still count toward directory-entry limits. An invalid owned
profile file is not ignored.

An interrupted save may leave a `.pending` file. The application preserves it
instead of silently discarding evidence or overwriting it. Close the app and move
that file outside the data root before explicitly retrying; keep the prior
valid `.config` or `.json` file intact.

### Import legacy profiles

After real inspection, **Import legacy profiles** copies compatible
`profiles/*.json` from the selected root into that Tab's package scope. It
preserves IDs and exact bytes, validates values against the inspected schema,
and keeps source files. Nothing is adopted automatically or shared with another
Tab. Byte-identical committed entries are skipped on retry; a conflicting ID is
refused without overwriting either file. Partial success remains committed and
the UI reports imported, already-present, and failed entries separately.

## Configuration snapshots and restore

**Application → App settings → Backups** owns the saved backup directory.
Blank means `<root>/backups`; **Save changes** persists a different private
absolute destination. **Back up now** is a separate explicit action using the
saved destination, never an unsaved settings draft. Ready also exposes
**Application → Restore a snapshot**; Setup and Recovery expose snapshot and
restore controls directly. **Destination for this snapshot** overrides only that
action. When saved settings cannot be decoded and validated, blank uses the
default destination without repairing or rewriting settings.

### Snapshot scope and publication

The filename is exactly `app.config.<unix_time>`, using decimal UTC Unix seconds,
with **no `.zip` suffix**. Its format is a versioned standard ZIP with **stored
(uncompressed), unencrypted entries**, not the reserved custom-package archive
format. The manifest records relative managed paths, kinds, lengths, SHA-256
digests, and observed root/settings absence. Checksums detect corruption; they
do not authenticate the archive or grant execution authority.

A snapshot includes present `settings.json`, all saved open and closed Tab
`tab.config` files, package `.config` files, and recognized legacy profiles.
Capture is structural, not dependent on successfully decoding a registry:
safe readable malformed, unsupported, and orphaned configuration is preserved
byte for byte. Missing entries remain absent. An empty managed set reports
`NothingToBackUp` without creating a destination or archive.

Package directories and custom archives, models, logs, other roots, earlier
backups, unrelated files, and temporary/journal data are excluded. An unresolved
owned `.pending` write or restore transaction blocks capture rather than being
silently omitted. Unsafe, unreadable, linked, aliased, oversized, or changing
managed data is refused. Capture serializes with configuration writers; archive
I/O does not hold the operation-control lock, so owner-bound Stop stays available.

Limits are **4,096 payload files**, **16 MiB payload bytes**, **4 MiB manifest**,
and **32 MiB archive**. The host creates a missing private destination and refuses
an unsafe existing one without chmod. Explicit destinations cannot enter the
root's `tabs/`, `profiles/`, or any `.restore*` storage, including aliases resolved
through existing ancestors.

The host writes and syncs private staging, validates the archive, and publishes
with no-replace semantics. A same-second or other existing-name collision is
refused without overwrite, suffix, or undisclosed alternate name. Wait for a
later second and click again, or select another safe destination. Success returns
the archive path, source generation, file count, and payload bytes. Dismissing a
dialog does not cancel an already dispatched snapshot; its outcome is retained.
Failures identify retained attempt-owned artifacts rather than claiming rollback
after publication.

Archives contain raw configuration, including local paths and potentially private
malformed bytes. Keep them private and outside public commits and CI artifacts;
log redaction does not sanitize the archive. Deliberately disclosed receipt paths
are not permission to publish its contents.

### Replace the managed configuration

1. Open **Application → Restore a snapshot** while Ready, or use the restore
   section in Setup/Recovery. Enter a supported snapshot's path. A configuration
   archive is not a package archive and does not install or execute package code.
2. If any managed configuration exists, separately click **Back up now** in this
   application session before replacement. Restore requires its successful
   receipt to match the current source generation and verifies that the
   preservation archive still exists unchanged. A file edited, created, or
   removed since capture requires a new click. Restore never makes a hidden
   substitute snapshot; unsafe or unreadable preimages block replacement.
3. Wait for operations and commands to settle. Confirm replacement of **all saved
   App, Tab, and package configuration**, including closed Tabs and legacy
   profiles, not just the selected workspace. If an Application is retained,
   separately confirm session reconstruction and disposal even when profiles
   are clean. This drops unsaved drafts and transient results, not file logs.
4. Click **Restore**. Container version, manifest/entry agreement, sizes, hashes,
   supported document schemas, and owning identities are validated before live
   mutation. Traversal, links, duplicate/aliased paths, encryption, unsupported
   ZIP features, unknown owners/versions, and unlisted payloads are refused.
   Target records are validated structurally without requiring their referenced
   installation to exist on this machine; restored bindings remain unchecked.
   A raw preservation snapshot with malformed data, orphaned package files, or
   no valid App settings is not an installable restore; preserve it for external
   repair.
5. Read the resulting state and cause. Confirmation resets after each attempt.
   A refused attempt does not erase an earlier valid preimage receipt; installed
   or recovered configuration consumes it. Successful reconstruction reloads
   saved-open Tabs with fresh identities and real source inspection, not old
   drafts, runs, Last check, cards, or in-memory logs. If the configuration was
   installed (or an interrupted restore rolled back) but the Application could
   not be rebuilt from it, Recovery says so explicitly instead of claiming that
   nothing was replaced; repair the cause and **Retry** rather than restoring
   again.

Restore replaces the whole managed file set; it is not a profile merge or a
whole-root swap. Package payloads, file logs, backups, and unrelated data stay
outside its write set. Before the first managed write, it stages incoming bytes
and rollback preimages privately and persists `.restore-journal`. Completion
requires checking the entire installed generation. Failure rolls back or keeps
Recovery with the journal/preimages and explicit incomplete-cleanup diagnostics;
installed configuration and successful reconstruction are separate outcomes.

Before deleting preimages, cleanup publishes `.restore-completion`. This marker
keeps restart admission blocked even after partial journal deletion. Restart with
either artifact enters Recovery, not a mixed configuration. Explicitly confirm
the recovery scope and choose **Complete restore** or **Roll back restore**.
Once the completion marker commits a direction, only that same direction can
finish cleanup; the opposite action is refused. Do not delete these artifacts
to bypass Recovery. Retry, Restore, and transaction recovery share idle and
session-disposal requirements; incomplete log-writer shutdown requires Exit and
relaunch before any reconstruction.

Recovery distinguishes incomplete restoration, cleanup failure and failed
Application reconstruction. If installation and automatic rollback both fail,
the current configuration may be partly replaced; neither generation is
claimed to be intact. Resolve the displayed cause before continuing.

While a restore remains pending, use the transaction controls in its committed
direction; **Retry** is unavailable. An unreadable or unsupported completion
marker does not establish either direction or verify the live configuration.
Repair the cause without deleting the recovery evidence. If the completion
marker was removed but the final directory sync failed, cleanup completion is
unconfirmed although no transaction remains pending. Recovery then offers
**Retry** after repair, not controls for a transaction that no longer exists.

## Save and check an OCR environment

1. Open **Application → App settings → OCR environment**, choose an offered
   supported profile, and enter the absolute model root, pinned ONNX Runtime
   library, and reviewed non-system
   library locations, **1–64 libraries**, one per line. Blank lines are ignored
   and surrounding whitespace is trimmed. Use the pinned
   [engine prerequisites](runtime-native.md#install-the-engine-prerequisites).
   Model bytes must match the selected profile; a filename alone is insufficient.
   Rust resolves selected aliases to canonical paths and derives byte lengths,
   hashes, and SDK identity; operators do not edit identity fields.
2. **Save changes** validates and atomically saves the dialog's editable settings.
   It does not load libraries, models, or a backend. **Clear draft** followed by
   **Save changes** removes the optional environment. Existing settings with no
   environment remain unconfigured without a read-time rewrite. Failed saves and
   incompatible stored data preserve the previous bytes.
3. On the selected workspace's **Run control** page, enter a **Recorded corpus
   descriptor** for its inspected recorded package.
   The descriptor is the `replay` object from the [existing replay format](runtime-native.md#prepare-a-private-replay-package-and-plan),
   not a Plan or complete `native_config`. Its assets must already belong to the
   inspected package inventory. No arbitrary asset paths, executable override,
   native authority, or unknown fields are accepted.
4. Return to **App settings → OCR environment** and choose **Check saved
   environment**. Unsaved environment edits must be saved first. Check uses that
   workspace and selection revision; its target is shown in the dialog. With no
   selected workspace, it can validate environment files but cannot initialize a
   corpus. A stale or closed workspace is refused before an operation starts.
   Check reserves the same operation slot as Start before settings or resource
   I/O; it is cancellable
   through **Stop**. Without a corpus it reports file-validation progress and the
   missing prerequisite, never readiness.
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

The UI displays the recorded result status, or the typed error category when no
result record exists; a timeout or cancellation is not relabeled as a refusal.
It keeps entry outcome, receipts, cancellation, and cleanup separate. A Stop
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
identity, source, severity, workspace/run attribution, and sanitized fields before fan-out.
A process-local Rust subscriber does not implicitly collect child logs. The GUI
and file outputs are independent; delivery to one is not proof of delivery to the
other, and neither is the authoritative result channel.

Workspace **Logs** filters the single shared buffer by the originating workspace,
not the visible workspace at delivery time. Application logs can show application-only
or all retained events. The log-level selector sits on the left inside the search
field, followed by text search; both controls are separately labeled and their
filters combine. Text search covers event code, message, source, and run;
private diagnostic fields are not searched. Severity and text filters do not
increase retention. A notification's **View logs** action opens its original
scope; closed origins and evicted events are labeled explicitly.

- **GUI retained-item limit:** integer **1–10,000**, initially **1,000**, saved
  through **Application → App settings → Logs → Save changes**, not a profile
  value. Lowering it immediately keeps only the newest items across all scopes.
  Invalid input preserves the previous valid limit.
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

### Notification cards

Cards summarize command and terminal outcomes without replacing persistent
errors, cleanup evidence, or logs. **App settings → Notifications** supports
**one or two cards**, **5, 8, or 12 seconds**, and success visibility; defaults are
**two**, **8 seconds**, and **enabled**. Older settings lacking this field use
those defaults without a read-time rewrite. Invalid present values are refused.

New outcomes displace the oldest visible card; there is no hidden unbounded
backlog. Lowering the saved count trims immediately. Each card keeps its original
timeout, paused while hovered or keyboard-focused. Dismissal or expiry does not
erase its diagnostic record. When a keyboard-focused card is dismissed or
displaced, focus moves to the card now in its place, or back to where focus
entered the stack; cards that expire or are dismissed without focus leave focus
untouched. Disabling success cards never suppresses warning or error cards. Cards
outside the modal are inert while App settings is open.

Keep app data, execution diagnostics, and any private fixture copies out of
public commits and CI artifacts. Redaction is not authorization to publish raw
execution evidence. The [logging decision](adr/0002-desktop-runner-boundary.md#logging)
owns the sink and shutdown design.

## Local GUI acceptance

This is a procedure, not a claim that the complete UI workflow has passed. Use
the actual macOS application and an isolated private data root, without granting
native capture/input authority. Running-application observation additionally
requires an explicitly authorized, already-running application; it grants no
launch or input authority. Record observed results and unexecuted scope separately
from [hosted build/core checks](ci.md#local-check-scope).

Preserve original bytes before fault cases. Use only disposable copies and private
roots; do not modify tracked fixtures or the operator's normal configuration.
Run the Setup, Recovery, naming, snapshot, and restore checks in both English and
Japanese. Do not replace actual WebView interaction with mocked command results.

### Directory-package authoring acceptance

Use a disposable package directory outside the isolated App data root. Keep
screenshots, local paths, and compiler/run records outside public commits.

1. Create a TypeScript starter, open it, and add a second source file. Edit both
   files; check independent undo/redo, selection, search, line numbers, and
   composition. Save, exit, and reopen; verify both saved contents.
2. Add, rename, and remove an optional declared file. Refuse an occupied or
   nested package destination. Duplicate under a different ID; verify original
   bytes and the absence of App-local configuration in the copy.
3. Save a syntax error and Validate. Follow its diagnostic to the source, repair
   it, and validate the new saved revision. Test Stop while validation owns the
   operation slot; no new work may start before it settles.
4. Keep another workspace bound to the same source. While Edit owns the first,
   verify disabled Start/Check controls and host-side refusal of a stale client
   request. Navigate through Logs/settings and return to the unchanged drafts.
5. Exercise Save, Discard, and Cancel for editor/workspace/window closure.
   Repeat close/exit with a recoverable configuration fault; drafts must remain
   resolvable without admitting ordinary execution.
6. Preserve a saved local profile while saving and repairing malformed schema
   text. Exit, explicitly reinspect, and confirm the profile is not reset.
7. Run the changed valid package through the real controlled runner. Choose the
   expected state/log result before the run and compare it with the actual record.
   Saving or compiler success alone is not execution acceptance.
8. Check English/Japanese presentation and a narrow supported window. Record
   whether composition was driven by WebView events or physical OS IME input;
   the former does not qualify the latter. This procedure grants no game input
   or live-capture authority.

### Setup, Recovery, and named workspaces

1. Launch with an absent explicit root. Observe Loading followed by Setup and
   confirm that merely opening or closing Setup creates no root. Relaunch, choose
   temporary Japanese presentation while keeping saved English, and Initialize.
   Confirm saved English, valid settings, no package/run/OCR work, and no Tab.
   Confirm that an existing settings file cannot be overwritten by Initialize.
2. Create a Tab using internal name `Alpha` and display name `共有`, without a
   path or target. Check its saved unbound record and Create/Open guidance; there
   must be no invented schema, profile, or runnable package before an explicit
   package action. Create another Tab with the same display name and a different internal
   name; verify disambiguated labels. Create a digit-leading or all-digit internal
   name with an empty display name; its internal name must appear in the workspace
   selector, saved list and after restart. Refuse case-only internal-name
   collisions, spaces/non-ASCII digits in internal names, whitespace-only/control
   display names, and limits beyond 64 internal characters or 80 display-name
   Unicode scalars. Include supplementary Unicode in the scalar-count check.
3. Inspect the real TypeScript directory separately in two Tabs. Save different
   profile values and confirm distinct catalogs, drafts, and durable
   Tab/package files. Close one, restart, and verify only saved-open Tabs return.
   Reopen the closed Tab through Saved workspaces; names/profiles remain but its
   old draft, results, and run do not. Repeat close/reopen, verify the eight-open
   and 64-saved limits without eviction, and confirm no name-renaming operation.
4. With private source copies, move a bound directory or change its package ID,
   then restart. Verify owner-specific unavailable-source guidance and preserved
   references/profiles, with healthy Tabs usable. A supported saved custom-archive
   reference must show unsupported-source guidance, not a fake empty package.
   Repair and Inspect a real directory; a failed durable bind must retain the
   previous saved reference without publishing a runnable replacement. Check that
   changing a selected package keeps other saved package settings.
5. In a disposable root, distinguish missing settings from malformed, unsupported,
   or unreadable present settings. Verify Setup only for absence; verify Recovery
   retains stage/cause and original bytes otherwise. Correct the external fault
   and Retry without a restart. Temporary language or form changes must not clear
   the cause, save defaults, or spawn duplicate polling/writer owners.
6. During a real controlled operation, make settings unusable in the disposable
   root and trigger their read through App settings. Verify retained polling,
   owner-bound Stop, and refusal of Retry/Restore while work is active. After
   settlement, repair and confirm session disposal before Retry, including when
   profiles are clean. Verify old results, cards, Last check, and in-memory logs
   do not enter the new session. If an actual `LoggingShutdown` timeout is
   observed, verify Exit/relaunch is required; do not manufacture a passing flush.
7. Test historical discovery only in an isolated account/configuration environment
   where the new default root is absent. Verify explicit import or fresh-start
   confirmation, source preservation, bounded recognized-file copying, and
   refusal of a conflicting destination or invalid source. Repeat launch with
   `--data-dir` and confirm discovery is skipped. Within a bound Tab, explicitly
   import compatible legacy profiles: check exact bytes/IDs, idempotent retry,
   conflict refusal, partial-success reporting, and independence from another
   Tab inspecting the same source.

### Configuration snapshot and restore acceptance

1. Save settings and profiles in open and closed Tabs. Click Back up now and
   inspect the resulting standard stored ZIP privately. Verify exact filename,
   manifest/entry agreement, exact managed bytes, and exclusion of payloads,
   models, logs, earlier backups, and temporary data. The receipt must identify
   the actual path, generation, file count, and byte count.
2. Edit the backup destination without saving. Click Back up now and verify the
   saved destination is used and the draft remains unsaved. Dismiss the dialog
   while a dispatched snapshot is pending, then reopen it and inspect its real
   outcome. Exercise an existing-name collision and confirm unchanged archive
   bytes with no suffix or replacement. Verify unsafe destinations and paths
   inside managed/restore storage are refused, including ancestor aliases.
3. Preserve a valid snapshot, then place malformed settings or orphaned package
   configuration only in the disposable root. In Recovery, choose a private
   temporary destination and verify raw bytes are captured without repairing
   their source. Verify NothingToBackUp for an empty managed set. Unsafe,
   unreadable, changed, pending, or oversized data must produce refusal rather
   than a claimed complete archive.
4. From Ready, open Application → Restore a snapshot. Attempt replacement without
   a separately clicked current preimage receipt, then after changing saved
   configuration since a receipt. Both must refuse before mutation. Repeat with
   an unchanged valid receipt, explicit whole-scope confirmation, and separate
   session disposal. Verify missing confirmations and active commands/operations
   block reconstruction; every attempt must reset consent.
5. Restore the valid snapshot from both Ready and Recovery. Verify settings,
   saved-open/closed Tabs, and all package-profile scopes match it, while payloads,
   backups, and file logs remain. Reinspect restored directory references and
   verify fresh session identities with no transferred draft, run, result, card,
   Last check, or in-memory log. Failed attempts must not erase a still-valid
   earlier receipt; successful installation/recovery must consume it.
6. Try corrupt, unsupported, aliased, traversal, or unlisted archive entries only
   with disposable copies. Verify pre-mutation refusal and preserved live data.
   A byte-preserving archive of malformed/orphaned/future configuration must not
   be treated as a supported restore.
7. Where an actual interrupted transaction can be observed safely, restart with
   its real journal and verify Recovery blocks mixed configuration. Exercise
   confirmed completion and rollback in separate cases. Check incomplete cleanup
   with `.restore-completion`, including restart after partial journal removal:
   only the committed direction may complete. Do not delete journal evidence or
   invent production fault-injection commands. Record unavailable interruption
   or writer-timeout scenarios as unexecuted, not passed.

### Profile schema recovery acceptance

Use disposable copies of the shipped controlled package and a private data root.
Keep package presets valid when changing the schema; do not edit tracked fixtures.

1. Save two profiles with distinct supplied values and array order. Add a
   top-level default and change a constraint so only one profile remains valid.
   Reinspect: verify one automatic save, unchanged bytes for the rejected profile,
   stable IDs/names, and usable compatible profiles. Restart and verify reuse.
2. Repair the rejected profile through the shared editor, including deliberate
   removal of an unknown field or replacement of a wrong type. Verify an untouched
   stored numeric string is not coerced. Cancel Reset and compare both draft and
   saved bytes; confirm Reset even when the discarded value has a numeric parse
   error. Switch between two Tabs with the same profile ID while a confirmation
   is open; it must not transfer. Guidance Inspect and Enter must confirm before
   discarding an edited recovery draft. Add a required option with no default
   (updating presets separately), then confirm Reset: verify unchanged bytes, an
   unsaved draft and the field issue. Complete and save that draft.
3. Move the source so the old directory no longer exists while a profile remains
   rejected. Inspect the same-ID destination through guidance. Verify a
   non-runnable candidate and unchanged durable source reference. Repair/reset,
   explicitly retry binding, and inspect the persisted new reference.
4. In the disposable root, exercise a binding-write failure after a profile save.
   Verify the saved fact survives separately from the failed binding. A previously
   inspected selection and its unsaved draft must remain authoritative, never the
   attempted candidate. Repairing another package/schema must not replace its
   catalog. Retry an in-place binding with an unrepaired profile and edited repair
   draft; binding may succeed without losing that draft. Check first-binding
   failure has a reachable Retry action and does not invent a saved-source fault.
   Navigate between Tabs while preserving origin attribution. Attempt candidate
   mutation during another controlled operation; verify refusal and unchanged
   owner-bound Stop. A changed/missing original must be refused without
   replacement/recreation. Repeat failure notices in English/Japanese. Record
   unavailable follow-up read-failure timing as unexecuted, not passed.

### Application target acceptance

1. Use a disposable declared package and a private root. Select an ordinary
   outer application with the native picker, Check, Save, and reopen its saved
   workspace. Repeat with an explicitly authorized wrapped application. Verify
   the form keeps the outer path and derives its identifier without requiring
   an internal executable path. Cancel a picker and confirm the draft is intact.
2. Try a real missing/invalid manual location. Check and Save must report failure
   without changing saved record bytes. Opt a disposable package into an exact
   `target.macos.bundle_id`, Reinspect, and verify explicit incompatibility and
   deliberate replacement; a different game must fail while a distinct launcher
   remains independently valid.
3. During a controlled run/OCR operation, verify the picker is refused and Stop
   remains reachable. With a picker open, verify new operation admission is
   refused while state polling responds. Do not activate a game to test this.
4. For an authorized already-running application, use the product's separate
   Check running application action. Record its timestamp and correspondence
   kind privately. A relocated signed match must disclose unknown original-copy
   attribution. Confirm Cancel, stale-result invalidation, unchanged saved bytes,
   and no restored observation after reopening. A read-only nonmatching
   requirement probe must be refused; do not launch, restart, terminate, change
   protections, or use game input to manufacture the result.
5. Repeat an existing controlled workflow and applicable recorded replay; target
   inspection must not gate either. Native Start must remain refused. Keep
   process paths, IDs, signing values, screenshots, and raw observations private.
   Report missing authorization/replay prerequisites as unexecuted. These checks
   do not satisfy native acceptance, R6, or initial both-OS qualification.

### Controlled run and UI acceptance

1. In a named workspace, Inspect the shipped TypeScript package and confirm that
   selection alone does not start a run. Save both package presets as named
   application profiles:

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
6. Inspect TypeScript and JavaScript packages in separate named Tabs and switch
   workspaces during work. Check independent drafts, owner-bound Stop, retained
   outcomes, stale-revision refusal, and close/reopen without deleting profiles.
   In an isolated store, preserve one owner's profile before making it unreadable.
   Reinspect that Tab, restore the original bytes, then save through that Tab.
   Confirm its catalog recovers without changing another Tab's catalog or draft,
   even when both inspect the same source. Validate must not republish stale lists.
7. In App settings, change notification count/duration/success visibility and
   GUI retention. Exercise Save, Cancel, Escape, focus restoration, invalid-value
   refusal, immediate trimming, and restart persistence. Verify card overflow,
   deduplication, hover/focus pause, failure visibility, and origin-linked Logs
   after workspace switches, closure, and log eviction.
8. Verify Run, Logs, the Application menu, and App settings at 1440, 1024, and
   900 CSS-pixel widths, including keyboard navigation and modal Stop.
   Verify the compact aggregate counters, conditional Errors count, shared
   selectors, and combined level/text log-search field. Check disabled Native
   selection, Replay's scenario lock, preset/profile round trips, empty enum
   values, long-list keyboard navigation, and popup placement near viewport edges.
   Leave a selector open across operation completion and verify it stays anchored
   when the operation strip disappears.
   In App settings, verify selector Escape preserves the dialog and Save retains
   numeric notification values after restart.
   Check structured Rust/Script attribution and independent file output. Exercise
   queue pressure/file failure only in the disposable data root; loss/failure
   counters must not erase results or disable Stop.
9. Repeat the UI checks in English and Japanese. Save and restart; cancel an
   uncommitted language edit; provoke a Save failure only in the disposable
   store and verify unchanged saved bytes, retained language, and Recovery.
   Check document language, validation, and retained notices after switching,
   including asynchronous completion and edits made during Save.
   Switch language during a controlled run, then Stop the same operation.
   Verify independent drafts, card pause/duration, log filters and original
   diagnostic bodies. A synthetic fault checks payload preservation, not actual
   SDK execution. Leave unavailable provider-specific evidence explicitly open.

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
