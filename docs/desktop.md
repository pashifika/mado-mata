# Desktop checkout: authoring, controlled/replay and macOS Native runs

MadoMata's trusted Tauri/React WebView provides directory-package and Recognition
authoring, inspection, profiles, run control, App-local OCR settings, and logs.
Package code runs in the supervised QuickJS runner, never in the WebView.
Controlled runs need no OCR installation. Recorded replay uses real engine
OCR/template recognition over explicitly selected, previously authorized frames.
Both execution lanes retain the **controlled, non-native input sink**.

Recognition has a separate explicit, read-only native window acquisition path.
It requires an authorized target and the fixed engine child; opening the editor
does not capture, initialize OCR, or request permissions. Separately reviewed
macOS **Native** Start runs the authored `readiness()` before target attachment.
Its explicit startup request can attach to a verified saved application through
the fixed engine child, or submit its saved recipe once when absence is confirmed
and launch is separately approved. Activation, automatic recovery and Windows
Native Start remain refused.

The checkout includes macOS and Windows shells; Linux checks the frontend and
shell-independent core. Windows interactive authoring and both-OS native
qualification are separate acceptance obligations, not claims made by a build.
Release packaging and full runtime adoption remain unresolved. See the
[engine prerequisites](runtime-native.md),
[capture boundary](adr/0007-native-capture-authoring.md), and
[Native Run admission](adr/0008-macos-native-run-admission.md).

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
| CodeMirror | state 6.7.6, view 6.43.13, language 6.12.4, JavaScript 6.2.5, autocomplete 6.20.3, commands 6.11.1 |
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

Vite bundles the editor and a lazy TypeScript language-service worker, including
the ES2020 declaration closure from the pinned frontend TypeScript dependency.
No CDN, runtime type download, package plugin or package-selected compiler is
used. The editor and execution compiler share the application-owned SDK generator.

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

On Windows, use an x64 MSVC developer terminal with the Windows SDK, Node.js
24.18.0, Rust 1.98.1, and an installed WebView2 runtime for interactive use.
Preserve the [tracked symlink](development-guidance.md#claude-symlink).
The same dependency installation, frontend build, and Cargo build commands above
apply. Launch the checkout artifact with:

```powershell
.\apps\desktop\src-tauri\target\debug\mado-mata-desktop.exe --data-dir "$env:USERPROFILE\.config\mado-mata-acceptance"
```

The owned controlled and engine runners use the same directories with `.exe`
suffixes. Build the optional engine from that terminal using the
[Windows native setup](runtime-native.md#install-the-engine-prerequisites),
adding `--target-dir tools/runtime-comparison/target/desktop-engine`.
Windows CI compiles the shell and runs portable/Windows contract checks; it does
not launch a GUI or qualify native capture, clipboard interaction, or OCR.

An absent engine artifact or failure before Rust startup remains a typed
diagnostic in the non-native GUI; controlled execution remains independent.

The default configuration root is `$HOME/.config/mado-mata`, not Tauri's
application-local data directory. For isolated acceptance, select a private root:

```sh
apps/desktop/src-tauri/target/debug/mado-mata-desktop --data-dir "$HOME/.config/mado-mata-acceptance"
```

`--data-dir PATH` selects the configuration root explicitly and skips historical-root
discovery. It supplies the default editable collection at `PATH/sources` and
the private image cache at `PATH/caches`; it does not select a runner, compiler,
package to inspect, or input route. Keep the
root outside an existing package source and public tracked files. Reuse it for
restart checks; choose another private root to isolate work without deleting data.
An absent root is not created merely by launching the application.

New application-owned directories use private Unix permissions. Existing managed
directories or files with group/other access, unsafe types, or links are refused
without changing their modes. Root and configuration failures open **Recovery**.
Only supported, owner-scoped legacy identifiers are converted automatically;
schema/value repair, elevation, and fallback to another root remain prohibited.
See [ADR 0005](adr/0005-desktop-configuration-recovery.md) for the configuration
ownership and reconstruction boundary.

## Setup and Recovery

**Loading** waits for the selected root, saved configuration, and any supported
legacy-ID conversion. Conversion finishes before Application publication or
normal polling; it never supplies default settings or repairs profile values.
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

Import captures the bounded managed configuration into private staging, validates
its ownership, and converts eligible owned profile/target IDs before publishing
the complete root without replacement. It refuses an existing destination,
source changes, unsafe input, and pending transactions; it neither merges nor
deletes the source. The [managed configuration limits](#configuration-files-and-limits)
apply. Logs, backups, package payloads, and unrecognized files stay in the old
root. Unassigned `profiles/*.json` remain byte-identical and require the separate
per-workspace import below; a historical package-location hint never creates or
binds a Tab.

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
| `application.rs` | `workspaces` owns Tab/session transitions; `authoring` owns the global Edit lease and validation/close lifecycle; `profiles` owns ordinary profile commands and desktop value checks; `recovery` owns schema reconciliation, repair/reset authority, and explicit binding retry; `targets` owns target commands; `native_run` captures and resolves reviewed bundle correspondence; `operations` owns execution, collection, and shutdown |
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
directories**, with at most **128 declared files** under the
[shared image and non-image limits](adr/0006-saved-image-recognition-observations.md#image-policy).
Create writes a runnable TypeScript starter. Duplicate copies the saved source
under a new package ID and updates package ownership in each packaged preset. It does not copy App
settings, named-workspace profiles, target bindings, or execution results.
Neither action inspects, binds, or runs the package.

- **Application → App settings → Packages** sets the sources folder. Blank uses
  `<data-dir>/sources` beside `settings.json` (normally
  `$HOME/.config/mado-mata/sources`). **Create or open a package** and **Duplicate**
  ask only for Package ID and show the derived destination. Settings Save creates
  nothing; valid Create/Duplicate creates missing parents. Changing the root
  neither moves existing packages nor retargets an active Edit session.
- `authoring` remains private publication-journal storage. `pkgs` is reserved for
  downloaded/packaged content; this does not add a download or archive feature.
  Existing explicit roots and package references under `pkgs` remain usable.
- Create and Duplicate require a missing destination. Existing package roots,
  links, traversal and aliases are refused. Under the App data root, source is
  allowed only within `sources` or `pkgs`, never configuration or journals.
  **Open for Edit** still accepts an existing external package directory.
- The collapsible left tree contains **Files**, **Metadata**, **Recognition**
  and **Duplicate**. Selecting a file or metadata item displays its editor,
  form or facts on the right. Collapsing the navigation leaves the current
  detail and drafts intact.
- **Files** contains scripts and assets in expandable folders. Folder nodes come
  from declared paths: adding or renaming `src/lib/helper.ts` creates its parents.
  There is no independent empty-folder operation. Scripts have independent
  drafts, selection, undo/redo, highlighted TypeScript/JavaScript, literal
  search/replacement and line numbers. Opening or revealing source preserves
  its line endings and UTF-16 positions, including non-BMP characters. File-tree assets
  are inventory facts, not decoded or text-edited; image authoring belongs to
  **Recognition**.
- **Metadata** opens structured manifest, option-schema and packaged preset
  controls; generated source maps are read-only facts. Metadata never
  opens in the source editor. Manifest controls preserve package identity and
  declarations while editing supported entries and portable target intent.
  Schema **Fields** pair the type selector and field-name input in one joined
  control, like Logs filters, including the new-field row. Names commit on Enter
  or focus loss; empty or duplicate names retain adjacent validation feedback.
  Malformed schema/preset bytes remain unchanged until deliberate repair and
  Save; rebuilding an invalid document requires confirmation. Saved local
  workspace profiles are not part of these forms.
- **+** beside the tree's collapse button adds a source, preset, JSON asset or
  source map. Image crops are saved separately through **Recognition**.
  Group headings, folders and blank tree space have no context menus.
  Right-click an individual file, use its menu button, or press **Shift+F10** /
  the **Context Menu** key for Rename/Remove where available; the manifest has no
  file-action menu and the required schema offers Rename only. A pointer-opened
  menu initially highlights no action; keyboard opening focuses the first action,
  with arrow/Home/End navigation.
  Actions target that row, not another selected file. Rename updates declarations,
  not source imports. Required entries/schema/presets cannot be removed; unsafe
  paths, links, collisions and undeclared files are refused, except known ordinary
  OS metadata files (`.DS_Store`, AppleDouble `._*`, `Thumbs.db`, `ehthumbs.db`,
  `ehthumbs_vista.db`, and `desktop.ini`). These files are left untouched and do
  not affect package revisions or application-owned storage; links, special
  files, directories with those names, and explicit package declarations of
  reserved metadata names are still refused. This is not a general hidden-file
  exception.
  Both the main and Recognition preview windows disable the ordinary Web
  Inspector, including debug builds; normal text-editing clipboard menus remain
  available outside these owned menus.
- **Save file** and **Save all** publish drafts without running or validating
  them. Incomplete script or invalid metadata values can be saved for later
  repair; they are not an executable inventory. A later edit stays dirty if an
  earlier Save response arrives afterward. Composition, paste, completion and
  replacement use the same file-local history as typing; package-supplied
  WebView code is never loaded.
- **Validate** checks one saved revision through the existing inventory and
  trusted compiler without evaluating package code. Unsaved text is excluded.
  Diagnostics identify their revision and link to declared source locations.
  The finite validation child occupies the existing operation slot; **Stop**
  requests cancellation and retains ownership until the worker settles.
- One Edit session owns the application. Ordinary **Start**, independent OCR
  **Check**, a second editor, and configuration reconstruction are refused.
  One shared application strip explains this exclusion, including when another
  workspace is selected. Its expandable authority details explain Save/Inspect
  as static content outside the live status announcement, not another warning.
  Idle Edit has no timed runner. Navigation remains available; the owner strip
  and **Return to Edit** preserve the session across workspaces and dialogs.
- Guidance **Open/Create**, like Run-page Edit, asks before leaving unsaved
  profile/recovery drafts, including an incomplete Reset's default-based draft.
  **Keep draft** cancels entry. Closing the choice restores focus to its entry
  button, or the page tab while that button is locked; a refused Open/Create
  does not strand keyboard focus. **Discard and edit** explicitly proceeds.
  Saved profiles remain untouched.
- Exit, Duplicate, closing the workspace, and closing the application resolve
  dirty files and Recognition metadata/crop selections with
  **Save / Discard / Cancel**. Cancel keeps the lease and drafts. Save processes
  dirty files before Recognition and stops at the first failure. Confirmed
  application close uses bounded shutdown and preserves incomplete containment
  outcomes; closing is not proof of successful cleanup.
- **Exit Edit**, then explicitly **Inspect/Reinspect** before Start. Selections
  for every workspace sharing an edited source are invalidated. A normal exit
  returns to unbound guidance, not an inspection-failure error. Affected recovery
  contexts also expire; other Tabs disclose that any unsaved repair draft was
  cleared. Saved references and local profiles remain untouched; inspection
  uses the existing [profile reconciliation and repair flow](#recover-profiles-after-a-schema-change).

### Script editing and completion

Use **Cmd/Ctrl-S** to Save, **Cmd/Ctrl-F** to focus Find, **Cmd/Ctrl-G** and
**Shift-Cmd/Ctrl-G** for next/previous matches, and **Cmd/Ctrl-Z** /
**Shift-Cmd/Ctrl-Z** for file-local Undo/Redo. Tab accepts the selected completion,
or inserts two spaces when no candidate is selected; Shift-Tab moves focus out.
Escape closes suggestions or returns from the search controls to source.
Mouse drag, double-click and Shift-arrow selections remain highlighted on the
current line. Selection stays visible in a subdued color while a toolbar control
has focus; typing over a focused selection and Undo use the same file-local history.

Find is case-insensitive and literal. **Replace** changes the selected full
match, or selects the next match without editing. **Replace all** changes every
original non-overlapping match in one Undo action, even beyond the displayed
10,000-match count cap. Replacement text is literal (`$&` and backslashes have
no special meaning); empty text deletes. Empty queries, composition and
ineligible files disable replacement. Oversized output is refused as a whole.
Draft preflight checks the 1 MiB source and non-image draft budgets; host
Save/Validate also count trusted dependency content and remain authoritative.

**Complete** or **Ctrl-Space** requests suggestions immediately at the caret; the
button remains available when macOS reserves that shortcut. New automatic
sessions follow eligible direct typing after the saved opening delay (100 ms by
default). Configure automatic opening and its 0–1000 ms delay in
**Application → App settings → Editor**. Provider processing takes additional
time; the delay does not apply to explicit requests or an active session's updates.

Typing and Backspace within an active token refresh candidates from the current
source, even when automatic opening is off. Backspace from `rel` to `re` restores
both `recognize` and `release`, including candidates outside an earlier capped
subset. Candidates retain provider order and match the decoded prefix
case-insensitively before the 200-candidate and response-byte limits. An unmatched
prefix closes either session kind; an empty prefix retains contextual choices.

Escape, blur, unrelated caret/selection movement, context departure and composition
cancel pending work and remove old candidates/documentation. After an identifier or
keyword, `{`, `}` or a backslash that starts no `\uXXXX`/`\u{…}` escape is a
departure. An incomplete escape keeps the session without candidates or requests.
Completing a valid identifier escape refreshes the decoded prefix; any other
completed escape cancels the session. Paste, acceptance, Undo/Redo, focus
restoration, idle and composition commit alone do not open a session; Backspace
alone does not reopen a closed session. Arrow keys select,
Enter or Tab accepts, and one Undo restores the prior source. Completion never accepts
during IME composition. It covers the current source's local bindings, `host.call`
methods and arguments, inferred SDK results, and nested fields/enum alternatives
from the current structured options-schema draft. Local declarations that shadow
`host` retain their own types.

Candidates use original 16px VS Code Codicons (`0.0.46-40`), its TypeScript-kind
mapping and light-theme symbol colors, and stay on one line. The SVG assets and
[CC BY 4.0 / MIT notices](../apps/desktop/public/third-party/vscode-icons/NOTICE.txt)
are bundled locally; there is no runtime icon download. Selected icons inherit
the selected row's foreground. A separate panel follows the selected candidate's
signature and available documentation, beside the list when there is room and
below or above it otherwise. The shared SDK provides method-specific call
signatures and descriptions; local functions retain their own inferred types and
documentation. Long candidate names are ellipsized in the list, while details
wrap and scroll in the panel. All documentation is rendered as plain text.
Opening suggestions brings the editor into view. The list stays inside its visible
area rather than jumping above the first source line; constrained lists scroll.

This is single-document assistance, not project-wide type resolution.
Imported helper exports, auto-imports, cross-file edits, DOM/Node APIs and package
type configuration are unavailable. Invalid schema immediately withdraws its
option fields and visibly reports unknown options; independent SDK completion
remains available. Repair or Discard uses only the resulting current schema.

Analysis has one worker, one active request and one coalesced latest request.
Startup is limited to 5 seconds and a dispatched request to 2 seconds.
Declarations are bounded to 2 MiB, responses to 200 candidates / 256 KiB and
each candidate's signature and documentation together to 4 KiB. Capped results
and omitted details are disclosed. A worker failure or deadline retires analysis
without changing drafts or blocking Save, navigation or Stop. Only a fresh
**Complete** or **Ctrl-Space** request may retry; continuing a manually opened
session does not grant restart authority. These are payload and deadline bounds,
not a total WebView memory guarantee.

Suggestions only edit drafts. Continue through **Save → Validate → Exit Edit →
Inspect/Reinspect → Start**; neither highlighting nor completion validates or
authorizes execution.

Session transitions are an independent behavioral adaptation of the supplied MIT
VS Code 1.140.0 snapshot's `suggestModel.ts`, `suggestController.ts` and
`completionModel.ts`; no VS Code source functions are copied. CodeMirror and the
existing restricted TypeScript worker remain authoritative. Builds and checks
do not read the reference checkout or use Monaco/workbench services.


### Source conflicts and interrupted saves

Every publication checks the owner, source revision, and current disk bytes.
An external edit is refused rather than overwritten. **Refresh** deliberately
reads the new revision while keeping dirty text; compare it before saving, or
discard that file's draft to adopt the disk version. A committed Save whose
follow-up refresh failed remains committed; refresh before writing again.

Publication stages only changed files beside the package directory. A private,
bounded journal under `data_root/authoring/` records old/new bytes outside package
inventory. The journal is written and synced in one bounded unpublished slot,
then atomically published as `pending.json` before any package file changes.
An interrupted unpublished write does not block admission; the next Save replaces
only that unpublished slot. A published pending journal blocks package admission
after restart. Use the displayed **Recover interrupted save** action for its
recorded package: recovery rolls forward matching old/new source bytes and
rebuilds partial private stages from the durable journal. Unexpected source or
staging bytes are preserved for repair. Do not delete the published journal to
bypass refusal. Configuration snapshots exclude package source and this journal.
Snapshot destinations inside `sources`, `pkgs`, the configured collection, or an
existing package are refused before creating directories or archives.

Interruption regressions cover process-level failures, not physical power loss.
Windows directory sync retains the
[existing platform limitation](adr/0005-desktop-configuration-recovery.md).
Custom archives, remote download, game input, and native qualification remain
separate work.

## Acquire a native historical frame

Open **Recognition → Open preview** inside an owned Edit session. No prior
Run-page target configuration is required. **Select window** verifies the chosen
application and window, then saves a locator only for this workspace/package.
Existing launch/input settings are preserved; a new capture-only binding has no
input policy. Access-denied, replaced/reparse paths, incompatible declarations and
unverifiable correspondence remain refusals. Do not elevate or substitute a
launcher to bypass them.

The fused capture control sits directly left of **Done**. Its left SVG target
icon selects a window; after selection it becomes the stacked-frame **New capture**
icon. The compact main segment shows **Capture** for both a saved locator and a
selected window, and **Stop** during native work. There is no separate capture panel.
Progress uses the fixed-height bottom status bar. **Help** opens the interaction
guide; help and scrollable errors/warnings overlay the image without resizing its
viewport or changing Fit scale. Help closes with its button or Escape. Toolbar
rows change only with window width, not operation state.
Feedback has no enclosing panel: individual red error, amber warning and blue
help borders distinguish message types.
The native-failure notice has a top-right close button. Dismissal hides that
notice through ordinary updates without changing native refusal or cleanup state;
a later failure can appear again.

1. With no saved target, choose the target icon (**Select window**). Discovery is metadata-only,
   bounded to 5 seconds and 64 matching processes / 64 eligible windows. The
   visual picker outlines the intended capture area; click selects and Escape
   cancels without forwarding input to the game. Its overlays leave before
   capture. Opening Preview alone acquires no pixels and initializes no OCR.
   On Windows, picker overlays belong to Preview rather than the main editor,
   so selection does not bring the main window above Preview. Preview remains
   an independent window, not an always-on-top window.
   This also works before **Inspect/Reinspect**. The verified target configuration
   belongs to the open workspace and package ID; saving it does not register a
   Run source, create profiles, or inspect the package.
   Configuration snapshots can restore this target before Run inspection;
   restoring ordinary profiles still requires their saved package reference.
2. With a saved target, **Capture** freshly verifies the locator and selects
   exactly one matching live window. Missing or ambiguous matches require
   explicit reselection; saved PIDs, window numbers and authority are never reused.
3. **Capture** refreshes the current Capture ID, Regions and unsaved draft.
   **New capture** creates a separate capture. Each explicit request acquires at
   most one frame within 10 seconds using the original retained Engine/TargetId.
   Changed lifetime or geometry requires reselection; there is no display
   fallback, target focus/resize, automatic retry or continuous sampling.
4. Each frame requires terminal-aware commitment and clean capture-session close.
   The same owned worker remains idle between captures, with no capture session.
   An idle Engine permits metadata edits and historical-image trials; active
   capture/picker work excludes another authoring operation.
5. **Done**, Preview close/destruction, cancellation and Edit exit release the
   owned Engine. Cleanup and containment retain separate 1-second / 2-second
   bounds; a Stop receipt is not cleanup proof. Incomplete cleanup stays visible.
6. The accepted image is historical, not a live connection or readiness result.
   Later target exit does not invalidate it. Every Script Start and independent
   OCR Check remain excluded throughout Edit; leaving Edit grants no Native authority.

The retained Engine has no idle expiry. Replay protection is bounded to 4096
capture identities per Engine; exhaustion refuses with `NativeCaptureLimit`
until an explicit restart. Save may advance the package revision without replacing
the Engine only after unchanged target constraints, binding and source proof are
revalidated. **Reset target** in Edit first settles the Engine, then removes only
the local binding; Recognition work and the package's target declaration remain.

The responsible capture executable is the fixed engine runner, not the WebView.
Missing permission is a refusal; the application does not request a permission
prompt to make an attempt succeed. Authorize each real target, environment and
operation separately. Both-OS GUI, permission, target-loss, overlapping-window,
mixed-DPI and negative-origin acceptance remain distinct from CI.

### Game content candidates (experimental)

In Preview, select **Game content**, choose **Auto**, **16:9**, **16:10** or
**4:3**, then **Detect**. Auto compares the three ratios. A selected ratio
restricts detection; it never creates a centered crop just to match that ratio.
Detection only proposes the cyan dashed rectangle. **Apply** uses the existing
content edit, Undo, draft/confirmation and crop-staleness paths; review and confirm
the geometry in the ordinary workflow. **Full** is also a proposal, not an
implicit confirmation. Cancel, beginning a manual content drag, a frame/basis or
owner change, a native action, and Done invalidate pending proposals. No
successful detection automatically overwrites a previously confirmed basis.

The detector checks sampled, nearly flat exterior scanlines and continuous
adjacent boundaries, including light/dark title bars, thin frames and letterboxes.
It searches at most the outer quarter on each side and bounds the number of
candidates. It needs visible interior variation; uniform/loading images and
similarly ranked alternatives produce no applied edit. These heuristics cannot
prove that a flat game UI panel is OS chrome. Custom ratios, arbitrary non-flat
borders and original-pixel refinement of downsampled previews remain manual.

The already displayed, owner-scoped raster is borrowed locally; no additional
capture, full-original decode, OCR, new permission or native input is requested.
A raster is bounded to 4,194,304 pixels. The host reserves the display raster,
one temporary canvas and one ImageData readback before publishing its preview.
The canvas is released on success or failure. This is payload accounting, not a
bound on browser/driver allocations or process RSS. Coordinates use the raster's
natural size and half-open edges, never CSS Fit or devicePixelRatio. When the
host has downsampled a large original, the candidate is explicitly labeled
approximate: mapped coordinates do not claim single-original-pixel accuracy.
The original PNG and package assets are unchanged. In Game content mode, the
compact ratio/Detect/Full/Apply/Cancel group replaces the toolbar's Undo/Delete
buttons; Regions mode retains those buttons. Candidate status stays in the
fixed-height bottom rail. There is no Detection limits panel. Controls remain
outside the image viewport; candidate changes do not remount the image stage.
In automatic mode, the compact zoom selector shows the actual percentage without
a **Fit** prefix. The separate Fit button still indicates the active mode.

The bounded regression command from the repository root is:

```sh
node --experimental-strip-types --test apps/desktop/src/contentDetection.test.mjs
```

These tests are also registered in the desktop `npm test` command used by CI.
They cover synthetic pixel geometry and readback cleanup, not interactive native
acceptance. Before acceptance, test both real WebViews: Apply/Undo/Confirm,
Cancel during pending detection, manual drag, same-size Capture, A -> B -> A,
owner changes, Done, Fit/scroll stability, and EN/JA at narrow viewport widths.

### Captures, migration and private originals

**New capture** and **Add capture from PNG** allocate a checked XID.
Recognition JSON version 2 contains `captures: [{capture_id, document}]`; each
inner document retains the existing basis, rounding and local `r1`/`r2` IDs.
The package-wide definition key is `(capture_id, region_id)`, not a filename.
Legacy single-image metadata is read without writing; explicit Save wraps it
without changing existing Region IDs, asset IDs, paths, aliases or pasted code.
Unknown versions and invalid data are refused rather than repaired.

Only one original is decoded at a time. Refresh preserves metadata and stages
checked unsaved crop pixels before releasing their original; Save never silently
substitutes a later frame. Crop identity depends on kind, geometry basis and
region, not names, reference text or metadata revisions. Same-size refresh reuses
confirmed Game content. Resized pixels require explicit geometry adjustment for
new-frame operations, while historical metadata and retained original crops remain
editable and saveable.

Save publishes the selected capture's crops and retains other captures' pending
originals; save captures separately. If only another capture has pending crops,
the Save explanation directs you to select it and save or explicitly discard.
Save all and Exit-save do not finish while other crop choices remain. Discard
clears crop choices across captures.
Delete/Undo restores selected originals only while their frame or staged pixels
remain available, never after Save or explicit discard released them. Switching
captures or loading an image resolves the current crop choices and releases
abandoned staged pixels without clearing other captures. Failed replacement leaves
no active image, not old pixels relabeled as a new capture. Saved PNGs change only
through explicit Save; trials, previews, Copy and Save retain revision fences.

Limits remain aggregate: 256 definitions, 256 KiB metadata, 64 Undo actions /
1 MiB, and 512 MiB accounted image payload. Originals are at most 16,384 pixels
per axis, 16,777,216 pixels / 64 MiB decoded; encoded input, transfer and cache
PNGs are bounded to 32 MiB. Native accounted image storage is limited to
256 MiB, including retained mapping copies and observable padding. These are
payload limits, not total RSS or opaque GPU/driver allocation guarantees.

**Cache new native captures on this machine** in **App settings** is persisted
and OFF by default. Accepted originals are written to
`~/.config/mado-mata/caches/<package_id>/<capture_id>.png` when enabled.
An explicit `--data-dir PATH` uses `PATH/caches/` for writes, reloads, size
measurement and folder opening. The former platform application cache is no
longer used; existing files there are not automatically moved or deleted.
Refreshing replaces only that capture's cached original, never saved package crops.
The transfer PNG is encoded before capture acceptance and reused for caching.
Encoding failure refuses acquisition; a later cache write failure is reported
separately and does not discard a usable accepted frame.
**Load cached original** is explicit, retains the capture ID, and creates
fresh runtime revisions; it restores no native authority. Reloaded originals
are marked historical even when no native acquisition timestamp is available.
Missing/corrupt files leave saved metadata, crops and templates intact.
Nothing auto-loads on reopen.

Cached image reads use verified file handles. Publication and cleanup retain the
managed package-directory identity; a link entry may be replaced, never followed.
**Manage image cache** measures regular-file bytes when opened. Unsafe or missing
entries and observed directory changes produce an error, not a partial total.
Measurement is not an atomic snapshot against deliberate same-user interference.
**Open folder** creates missing managed directories, validates their pathname
immediately before Finder/Explorer dispatch, and reports dispatch failures.
Links/reparse points present at validation are refused. The file manager resolves
the pathname independently; a same-user replacement afterward is not atomically
contained. This action reads or writes no file contents.
There is no polling, quota, eviction, cleanup daemon, custom location or cache
database. The preference may participate in configuration backup; original images
and cache paths stay outside packages, profiles, backup payloads and routine logs.

#### Windows filesystem refusal checks

These shell-independent checks use owned disposable PNGs and byte files, not
game processes, real cache data, capture, OCR or input. Run from the repository
root in the x64 MSVC developer environment described above. The two ordinary
Windows sharing tests run in the portable cache subset:

```powershell
cargo +1.98.1 test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --no-default-features --lib capture_cache::tests:: -- --nocapture
```

Only with existing, authorized symbolic-link capability, explicitly include the
ignored reparse fixture and run the actual cached-loader and target-file checks:

```powershell
cargo +1.98.1 test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --no-default-features --lib capture_cache::tests:: -- --include-ignored --nocapture
cargo +1.98.1 test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --no-default-features --lib application::authoring::recognition::tests::windows_cached_loader_refuses_reparse_substitution_and_preserves_verified_pixels -- --exact --ignored --nocapture
cargo +1.98.1 test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --no-default-features --lib application::authoring::native_capture::windows_file::tests::symbolic_link_leaf_and_ancestor_are_refused_without_following -- --exact --ignored --nocapture
```

Do not elevate or enable Developer Mode just to run these checks. An ignored or
zero-test result is not acceptance. The fixtures cover cache-root/package
junctions, symbolic-link PNGs, actual cached loading, pinned-directory
rename/delete refusal and release, and sharing-conflict refusal without an I/O
fallback. They do not qualify the Windows GUI/native workflow or strengthen the
external file-manager/measurement boundary described above.

## Author saved-image Recognition

Open the viewfinder-icon **Recognition** action in the Edit session's left
**Package contents** tree, above **Duplicate package…**. Use **Load PNG…** to
select an existing, authorized local image; this is not a capture command or
permission grant.
Loading, editing, saving, and Copy do not initialize OCR. Trials require the
fixed engine runner and [saved OCR environment](#save-and-check-an-ocr-environment),
but no replay descriptor, workload profile, or executable package source.
Opening Recognition probes the engine's grouped OCR capability without loading
models; **Check engine** retries a missing capability. No guessed limit or
substitute backend enables a trial.

**Open preview** opens one independent, normal-level native window for the image,
**Fit**/zoom, and the **Regions / Game content / Inspect** tool switch. It is not
attached above the main window; either window can come to the front. Definitions,
results, **Save recognition**, and **Copy snippet** remain in the main Edit window.
Selection, metadata, and bounded Undo are shared; closing and reopening the
preview keeps the draft and Edit lease. The preview receives a bounded display
raster, not a second editable original. Its scale does not change stored geometry.
The percentage picker uses the same keyboard-accessible dropdown as the main
window. Tool and zoom controls retain separate focus outlines; **Fit** and the
minus/plus buttons do not change the stored geometry.
The toolbar's rightmost **Done** button uses the primary accent color and closes
only the preview, including when no image is loaded. It does not save, discard,
exit Edit, or stop a running trial.
Reopen with **Open preview** to continue the same draft.

With **Inspect**, click a pixel anywhere in the original image, including outside
Game content or before geometry confirmation. X/Y are zero-based full-frame
coordinates from the top-left. With the image focused, arrows move one original
pixel, including with Shift; an edge move is a no-op and no selection is invented.
Escape clears the selection. Inspect does not change geometry, drafts, crops,
trial evidence or Undo; Delete/Undo controls and geometry shortcuts do not edit.

The host's decoded **RGBA8** channel numbers are authoritative, not browser
compositing, the reduced display raster, monitor color or encoded sample depth.
**RGB only** hex excludes alpha; A is shown separately, and the checkerboard
swatch illustrates transparency. Even A 0 retains the stored RGB values.

The readout is transient, not saved or written to ordinary logs. Pointer leave
and same-source zoom/scroll preserve it. Repeated background status snapshots do
not reset a just-selected tool or zoom. Source/owner changes, a missing or replaced
raster, leaving Inspect, acquisition start and Preview close clear it.
One read runs at a time; newer selections replace the queued point and immediately
clear the old color. Pending/errors stay in the fixed readout without resizing
the image viewport or Fit scale. After a failure, explicitly select a pixel again;
there is no automatic retry or previous-color fallback.

Use this workflow:

1. Set up the content rectangle once with **Game content**, excluding
   black bars or other non-content borders. The preview returns to **Regions**
   after committing the adjustment. Regions is the default tool: repeatedly drag
   empty content to add OCR definitions for each scene; select, move, resize,
   delete, or Undo through the same shared state.
   Region coordinates are normalized to the content rectangle, then mapped to
   original capture pixels with floor for left/top and ceil for right/bottom.
   Empty, non-finite, or out-of-bounds rectangles are refused. Zoom, scrolling,
   and Retina display scale never resize the recognition input or saved crop.
2. Review **Geometry** in the main window. The first new document starts with
   the whole image confirmed. Confirm an explicit content adjustment. Confirmed
   content is reused for same-size scene images and, once saved, after reopening
   the package. Different image dimensions require **Confirm geometry**;
   repeatedly loading that size cannot bypass confirmation. Equal dimensions
   cannot detect changed placement: use **Game content** when it changes.
   Every replacement retains definitions and saved assets but clears current
   crop selections and invalidates earlier frame observations. Copy source
   freshness follows its geometry or definitions, separately from trial evidence.
3. Name definitions and choose **OCR** or **Template** explicitly; there is no
   automatic fallback. Up to **256 definitions** fit within **256 KiB** of
   metadata. Nine or more definitions are valid even though the pinned engine
   accepts only **8 OCR zones per grouped request**. Select any subset within
   the reported capability and choose **Try OCR**. The UI sends list order;
   the host preserves any distinct caller-supplied selection order in one
   grouped request. Excess selection is refused, never split or truncated.
4. Read all returned OCR regions under their originating definition: public
   text, confidence, and capture-pixel bounds in engine order. The upstream text
   is already NFC-normalized and Unicode-trimmed; the editor adds no
   normalization, concatenation, exact-match judge, or correctness verdict.
   **Recognized** means observations exist, not that the text is correct.
   Results exceeding **256 regions or 256 KiB** fail rather than become a
   truncated success. [ADR 0006](adr/0006-saved-image-recognition-observations.md)
   owns this observation contract.
5. For a template, move its pattern crop or resize its separate search area from
   an edge or corner. The search area's interior remains available for selecting
   other regions and drawing new ones. The search must fit the pattern.
   Enter and review **Template rights** before
   trialing or saving template pixels. **Try template** tests one template and
   reports the effective threshold, actual scores, and boxes; no-match invents
   no score. Threshold and maximum-result defaults are displayed, not editable
   tuning controls. Fine matching criteria belong to Script authors.
6. Check **Save crop** only for definitions whose current pixels should be
   persisted, then choose **Save recognition**. Metadata-only Save needs no
   loaded image when retaining confirmed content setup, including after a failed
   image replacement. A changed, unconfirmed basis cannot be saved and reopened
   to bypass confirmation.
   New or replacement crops require confirmed geometry. The transaction writes
   metadata JSON, selected original-resolution PNG crops, and required template
   manifest/maps. It never persists the loaded original, trial text, or trial
   results. Template pixels require a license, creator, optional purpose, and
   explicit rights review; the application invents none.
7. **Copy Game content setup** copies a `recognitionBasis` declaration from the
   confirmed frame dimensions and content rectangle. It needs no selected or
   checked definition. Paste it once before the grouped OCR snippets, inside
   the existing workflow or another scope where they can access it.
   This is Script data, not Engine initialization: models, runtime libraries,
   environment settings, and target/input authority remain application-owned.
8. **Copy checked OCR (one request)** uses every checked OCR **Trial** row in
   list order, independently of **Selected definition**. The adjacent names and
   count show the selection. Capability discovery must have completed; an
   unknown bound or more than the engine's **8 zones** refuses the whole Copy.
   There is no hidden batching. **Copy selected template recognize** retains
   its selected saved-template behavior.
   Optional **Script reference text** is bounded to **4 KiB UTF-8** and preserved
   across kind changes. It appears only as escaped author-context comments in
   OCR Copy, never as a recognition filter, wait condition, or trial verdict.

Copy publishes `mado-host-v1` source through the native macOS or Windows clipboard
only after the explicit action. It needs no browser clipboard permission or general
clipboard plugin and never edits package source. Preserve the package's existing
Readiness/workflow exports when pasting. Relevant edits mark the corresponding
Copy obsolete; pasted code is never updated automatically.

Copy requires a loaded, confirmed frame. Setup is geometry only, never a verified
recognition result. Grouped OCR emits one `scan_ocr_zones` call over one retained
observation, using normalized regions relative to `recognitionBasis`. The host
requires matching frame dimensions and valid content/zone bounds before engine
work. Results contain every public region per zone, including explicit
`no_match` zones, in caller/engine order. They are plain snapshots without result
handles; `finally` releases the original observation. No waits, text matching,
input, or automatic text logging are generated.

Copy verification describes trial observations **at Copy time**, not later
images or text correctness. Setup freshness follows the actual geometry values;
grouped source follows definitions and checked selection, so it can be reused
with another frame and separately updated setup. Template source uses concrete
ROIs and additionally requires current saved metadata, pixels, rights, and maps.
Saving an OCR diagnostic crop alone does not obsolete grouped OCR source.
Equal dimensions cannot detect changed content placement: adjust Game content,
copy the setup again, and replace the pasted `recognitionBasis` when needed.

**Recheck saved crop** recognizes the complete saved OCR sample whether or not
a scene image is currently loaded. It reads the saved PNG, not the loaded scene
or its current region pixels. Sample bounds are crop-local; the result proves
neither original-frame placement nor a current frame trial and cannot authorize
Copy. After reopening, definitions remain editable without the original image.
Load a frame for Copy, new crops, or frame trials; the saved basis is reused only
at matching dimensions.

Recognition Save shares the existing source-revision transaction. Save or discard
a dirty manifest first; unrelated script drafts remain independent.
**Save all** also saves dirty Recognition state after files. A committed Save
whose refresh fails stays committed; refresh to adopt authoritative saved crop
references before writing again. **Discard recognition draft** restores saved
metadata and clears crop selections. If its saved basis has different dimensions
from the loaded replacement, the host releases that incompatible frame and preview
instead of remapping saved coordinates. A retained compatible frame still needs
geometry confirmation.
Manually restoring saved metadata clears its dirty state without rewinding edit
revisions. Save refusals identify the actual geometry or template-rights
requirement; the dirty-choice dialog disables Save and leaves Cancel and Discard
available.

New crop assets use independent checked XIDs and flat paths
`recognition/crops/0001.png`, `0002.png`, and onward. Each Save derives the
greatest numeric PNG basename from the current package inventory, ignores
custom names and nested directories, and assigns consecutive numbers in the
submitted crop order. Padding is at least four digits; `9999.png` is followed
by `10000.png`. Numeric overflow, stale inventory, or a collision refuses Save
without fallback names or partial publication. There is no persisted counter.

Updating an unshared crop keeps its asset ID and current path. Package editor
**Rename** preserves that ID and its declared/template references; replacing a
shared crop instead creates a new independent asset. Renaming or deleting the
highest numeric filename permits later reuse of that filename, not its asset
identity. Existing assets, local Region IDs, saved aliases and pasted Script
are not migrated or automatically refactored.

Deleting a definition does not immediately delete saved pixels. Save reconciles
owned crop references and generated template assets, preserving shared crops and
unrelated JSON consumers. Pasted Script references are never refactored. Saved
template maps merge into desktop replay only when explicit mappings agree;
conflicting aliases or engine-manifest mappings are refused. Saving a crop does
not create a replay corpus or native authority.

Trials reserve the shared operation slot and run only captured pixels through
the supervised engine child, never package modules. **Stop** remains available
in the main and preview windows. The **30 s** deadline, **1 s** cleanup, and
**2 s** containment bounds are separate: Stop or deadline expiry is not proof
that the backend returned, its session closed, or its child was reaped.
No subsequent operation is admitted while the owned worker is unsettled.
Read primary outcome and cleanup independently; forced or incomplete cleanup
does not become a successful trial.
An unexpected child exit without a verified terminal outcome reports `Transport`
unless an earlier failure or cancellation already explains it. The collapsed
cleanup summary flags incomplete or unconfirmed cleanup independently of child
reaping; expand it for the retained details.
Confirmed child reaping releases Edit admission even after failed cleanup, unless
the supervisor reports containment. Unconfirmed ownership continues to refuse
Save, Exit and new operations. Terminal collection retains the outcome before
releasing admission; shutdown also reports incomplete cleanup that its command
had not yet returned. Reaping alone does not prove successful session cleanup.

The [image policy](adr/0006-saved-image-recognition-observations.md#image-policy)
bounds encoded input, original pixels, crop size, package content, decoded
frames, and accounted application-owned payloads. It is not a total-RSS limit.
Keep images, recognized text, clipboard contents, and local resource paths private.

## Package workspaces and App settings

The two-row navigation header stays at the top while the document and the
package tree continue scrolling. Revealed diagnostics must clear the measured
header height; scrolling or collapsing the tree does not discard drafts.
The icon-only **Menu** button opens the existing Application actions, with a
localized accessible name and tooltip; its icon is decorative. Enter/Space
opens the menu, Up/Down moves between actions, Escape closes it and returns
focus to the button, and Tab or an outside click dismisses it.

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

Persistent notices stay with their scope: the application strip identifies the
Edit or operation owner, while action-specific refusals and faults remain beside
the relevant controls or panel. Dialogs provide their own **Stop** and
**Return to Edit** controls because the background is inert. Dismissing a
notification or closable notice changes presentation only: it does not release
Edit ownership, enable a refused command, erase a retained failure, or prove
cleanup.

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

Use **Application → App settings** for Display, Notifications, Editor, Packages,
OCR environment, Captures, Logs, and Backups.
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

### Editor completion preferences

**Editor → Show completions automatically** defaults to on; **Automatic opening
delay (ms)** accepts integers from 0 through 1000 and defaults to 100. These are
App-owned `settings.json` values under `editor_completion.automatic` and
`editor_completion.delay_ms`, not package or Profile preferences.

One authoritative **Save changes** applies the pair. Draft changes, Cancel and
Escape do not preview preferences; a failed Save preserves correctable edits.
Edits made after submission remain unsaved when the earlier Save returns.
Settings Save remains available during an otherwise admitted Edit session:
source, selection, history and the Edit owner stay intact. A changed completion
value cancels pending/visible suggestions in place; unchanged values do not reset
completion. Saving or returning focus does not itself open suggestions. The
existing busy-command, closing and restore refusals still apply, and settings Save
does not enable Script Start, OCR Check or another Edit while the lease is held.

Older settings without the whole `editor_completion` object read as on/100 without
rewriting the file. Present malformed, partial, unknown-member, wrong-type or
out-of-range values are refused without repair or clamping. Saved values survive
restart and use the existing configuration snapshot/restore path. Preserve a
compatible configuration backup before rolling back to an older strict binary.


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

The **Target** section on a bound workspace's Run page stores local configuration
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
   A direct-executable launcher uses an explicit absolute working directory or
   its executable's parent directory. Bundle launch uses the OS-defined working
   directory; an explicit directory for a bundle recipient is refused before
   launch, not ignored. Supply an exact window title when the package leaves it
   local; a package-required title is read-only.
3. Leave all input fields blank for capture-only use. To configure future input,
   explicitly select a complete policy. Process-directed input requires a pointer
   mode: Core Graphics permits either focus policy; AppKit background requires
   preserved focus. System input requires an already focused target and no
   process-pointer mode. Click hold is **0–1000 ms**. None grants execution authority.
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
text. Runtime relocation never rewrites the saved installation. This observation
grants no Native Start authority; Windows target work is deferred, and initial
both-OS qualification and R6 remain unresolved.


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
| `identity-migrations.config` | Versioned legacy-to-XID reservations; **4,096 entries / 4 MiB**, included in the shared managed-set limits |
| `logs/` | File diagnostics; not configuration |
| `backups/app.config.<unix_time>` | Default manual snapshot destination |

The managed set is bounded to **4,096 files / 16 MiB**, across open and closed
Tabs, settings, the migration ledger, and recognized legacy files. Bounded store
directories permit at most **128 entries**; snapshot enumeration permits **16,384 entries** overall.
Source paths and explicit backup destinations are absolute UTF-8 paths of at most
**4,096 bytes**. Filesystem case/normalization aliases and containing-identity
mismatches are refused rather than selecting another owner's data.

Known ordinary OS metadata files are left untouched and excluded from the
managed count, byte budget, and configuration snapshots. Raw enumeration remains
bounded, and genuine pending writes and unsafe managed entries still refuse.

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

### Persisted identifiers and automatic conversion

New profile and target-binding identities use canonical **20-character XIDs**
from the immutable public Fork revision pinned in Cargo. Save, rename, schema
repair, and restart retain the current ID. Allocation failures or exhausted
collision attempts refuse the operation; they never overwrite an existing
profile or fall back to an application-specific generator.

During Loading, supported owned `p-<32hex>-<8hex>-<16hex>` identities are converted
before ordinary commands can run. Only typed IDs, profile `.config` basenames,
and their typed references change. Names, values, schema identity, target
configuration and revisions remain unchanged. Schema-rejected profiles remain
rejected until explicit repair. Package IDs and payloads, Region IDs, assets,
aliases, pasted source, and unassigned `profiles/*.json` are not rewritten.

`identity-migrations.config` reserves each assignment by entity kind, Tab internal
name, package ID, and old ID. Identical old text in another owner or entity kind
does not share an identity. Reservations survive rollback, deletion, and owner
retirement; they do not recreate missing entities. Limits, invalid metadata,
conflicting mappings, or unsafe records refuse conversion without eviction or
best-effort replacement. Do not edit or delete the ledger to retry a failed import.

Conversion uses the same bounded journal and explicit pending-Recovery controls
as Restore, without creating an automatic backup or bypassing Restore consent.
Rollback preserves assignments while restoring original user configuration.
After interruption, use the displayed transaction controls; do not remove
`.restore-journal` or `.restore-completion` to force Ready. Old session/profile/
target expectations are not translated into fresh command authority.

XIDs are not secrets, capabilities, or anonymous identifiers. Their time,
machine-derived, process-derived, and counter components are observable.
A converted XID records **allocation at migration time**, not the entity's
original creation time or creation order. It grants no native authority.

### Development transition notes

This converter is an **unreleased development change**. The pre-converter
`dev/m3` baseline is `64e1dc8f5f37cfbd4eaacef7de0bce67763e93ef`.
No product release or tag existed when this transition was introduced; the
manifest's `0.1.0` is not a published release boundary. The first published
converter release must identify itself and the last released legacy writer, if
one exists, rather than infer either from the unchanged development version.

Keep automatic legacy conversion enabled until a separate approved Change names
the first rejecting product version, retains an available converter-release path
for old roots/backups, and decides ledger retirement. There is no date-based
expiry or timer cleanup. Converted roots and version-2 archives are not supported
by legacy-only binaries; do not downgrade in place or delete retained backups.

### Import legacy profiles

After real inspection, **Import legacy profiles** imports compatible
`profiles/*.json` into the explicitly selected Tab/package and keeps source bytes
unchanged. Legacy IDs use that owner's reserved XIDs; current XIDs stay unchanged.
Each committed profile and its mapping form one recoverable unit. A repeated
import skips identical normalized content, refuses edited-destination conflicts
without overwrite, and recreates an explicitly reimported deleted profile with
its reserved ID. Another Tab receives a separate mapping. A later-file failure
retains the actual committed subset, reported separately from already-present
and failed entries.
Journal admission captures the bounded managed set: an unresolved pending write
or unsafe entry in another owner can refuse Import until repaired. This does not
broaden the existing owner-scoped admission for ordinary profile edits.

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
`tab.config` files, package `.config` files, recognized legacy profiles, and
`identity-migrations.config` when present. Ledger-bearing snapshots use archive
version 2; the delivered version-1 archives remain readable during the transition.
Back up now neither converts IDs nor creates a missing ledger.
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

Restore replaces the managed user configuration, not package payloads, file logs,
backups, or unrelated data. Eligible legacy identities are normalized only in the
validated proposed generation; archive bytes remain unchanged. Compatible live
and archived reservations coalesce, and ledger-free archives retain live
reservations. Normalization, conflicting mappings, filesystem aliases, and rollback
budgets are checked before retiring a Ready session; these refusals preserve its
workspaces and unsaved drafts. Publication rechecks the live generation and pending
recovery before writing. Repeated restoration preserves mapped IDs within one
maintained root; independent empty roots restoring a ledger-free archive need not
assign equal IDs.

Before the first identity replacement, the host stages incoming bytes, durable
reservations, and rollback preimages and persists `.restore-journal`. Completion
requires checking the entire installed generation. Failure restores original user
configuration while retaining reservations, or keeps Recovery and its evidence
if publication, reservation preservation, or cleanup remains unresolved.
Installed configuration and successful Application reconstruction are separate.

Before deleting preimages, cleanup publishes `.restore-completion`. This marker
keeps restart admission blocked even after partial journal deletion. Restart with
either artifact enters Recovery, not a mixed configuration. Explicitly confirm
the recovery scope and choose **Complete operation** or **Roll back operation**.
Once the completion marker commits a direction, only that same direction can
finish cleanup; the opposite action is refused. Do not delete these artifacts
to bypass Recovery. Retry, Restore, and transaction recovery share idle and
session-disposal requirements; incomplete log-writer shutdown requires Exit and
relaunch before any reconstruction.
Pending transaction evidence blocks ordinary commands, not **Exit** or window
close. Closing still contains owned resources and preserves that evidence; it
does not complete or roll back the transaction or bypass unresolved editor guards.

Recovery distinguishes incomplete restoration, cleanup failure and failed
Application reconstruction. If installation and automatic rollback both fail,
the current configuration may be partly replaced; neither generation is
claimed to be intact. Resolve the displayed cause before continuing.
Interrupted-write and alias refusals identify the relative managed path. Private
file-read and directory-enumeration failures also retain their path attribution.

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
It is bounded to **256 KiB**. Inspection, Check, and both Start lanes use the
[shared image policy](adr/0006-saved-image-recognition-observations.md#image-policy),
including separate package-content and decoded replay-frame bounds. Geometry,
strictly increasing timestamps, package declarations, template maps, and relative
paths are validated before replay admission.

**Last check** retains its operation, stages, identities, failure summary, and
cleanup. Draft/settings/package/descriptor changes detach that association.
Nothing watches paths or grants permission from an earlier successful check:
every Start recaptures and revalidates, including same-path replacements.
Detailed check diagnostics require explicit private disclosure.

## Start, Stop, and results

**Start** submits the current explicit draft. Choose **Controlled** for the
shipped fixtures, or **Recorded replay** with a saved environment and selected
descriptor. **Native** requires the separate macOS review below. Replay and Native
accept only the package workflow, never controlled fault-injection scenarios.

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

Controlled execution has a **10 s** operation deadline; replay, Check and
saved-image trials use **30 s**, including input preparation. Desktop Native
instead reviews separate **60 s Startup**, **30 s Readiness** and **30 s Workflow**
budgets, under an outer deadline fixed at reservation. Timeout remains distinct
from explicit Stop. Repeated parent/child resource verification is not skipped
to fit an execution budget.
Cleanup remains **1 s** and containment **2 s**. Controller shutdown waits at most
**14 s** for its owned worker. Ordinary window closure requests shutdown off the
UI thread. Native macOS Quit can bypass that request callback, so the final exit
callback waits for the
same bounded shutdown, without starting a second sequence. This fallback is
source-verified against the pinned dependencies; the native Quit gesture has not
been exercised. Unexpected app loss retains the runner's parent-loss contract.
An expired deadline remains unverified/incomplete, not a clean acknowledgement.
The log bridge polls every **50 ms** and is joined during shutdown; that interval
is not a join timeout. File-log shutdown follows below and never determines the
run result.

## Reviewed macOS Native Start

Native requires a current, explicitly authorized target, environment and operation.
Without separate launch approval it is attach-only. An installed engine or
successful Check is not permission, native qualification, or proof of game effect.

1. Build the fixed engine artifact above and save the reviewed App OCR environment.
   Save/Validate the authored package, exit Edit, then Inspect it. Recognition
   definitions and template aliases come from declared package assets; do not
   paste local engine paths or a native Plan into the package.
2. Save a compatible macOS application-bundle Target with an exact window title
   and explicit route/focus/pointer policy. The Script's startup request freshly
   verifies bundle/runtime correspondence and reuses a unique running game without
   changing its arguments. If the game is confirmed absent, only separate launch
   approval permits one saved-recipe submission after a final discovery recheck.
   Missing windows, ambiguity, overflow or unverifiable candidates never authorize
   launch. Script status probes drive bounded preparation for the selected
   lifetime's exact eligible window; loss or replacement fails rather than
   attaching a successor. Check running application remains historical information.
3. Select **Native**. Review package/profile, target binding/revision and policy;
   enter the intended operation and what a newer frame must show. This text records
   the human review; the package must implement its recognition and postcondition.
   It is not a script sandbox or an automatic assertion generated from prose.
4. Review the host phase budgets: **60 s Startup** from reservation,
   **30 s Readiness** from capture availability, and **30 s Workflow** after
   `"Ready"`. These are defaults and ceilings; invalid tuples refuse rather than
   clamp. Each phase starts once, cannot borrow unused time, and cannot extend
   the absolute outer deadline fixed from the reviewed sum (**120 s** by default).
   Other limits remain **300** acquired frames, **1 s** waits, **100 ms** pacing,
   **64** input events across the run, **1 s** cleanup and **2 s** containment.
   A click consumes three events, or four with a hold; key press/release each
   consume one. Producer frames and restricted cleanup releases are not ordinary
   acquisition/input budget entries. Any reviewed-tuple change withdraws consent.
5. Review the saved launch recipient and literal ordered arguments. The recipient
   is the separate launcher when configured, otherwise the outer game bundle.
   Bundle launch uses macOS `NSWorkspace`, not a shell or its inner executable;
   direct-executable launchers use their reviewed working-directory policy.
   The launcher owns onward game arguments/cwd; its PID or exit is not game
   identity or readiness. Approve **launch if absent** only when permitted.
6. Approve capture and input separately, then Start. Every submission consumes
   all approvals, even a refusal. Relevant edits, target edit/discard, environment
   changes and leaving the workspace withdraw them. Unrelated settings changes
   do not change the reviewed environment. Native does not request activation,
   focus, permissions or elevation, substitute a route, or retry uncertain input
   or launch. The launched app can independently present windows or change focus;
   disabling API prompts cannot suppress macOS Gatekeeper UI.
7. Preflight validates captured package/profile, static compilation/imports, assets
   and required resources without evaluating package code or acquiring capture/input.
   The existing runner then enters `readiness()` before attaching a target.
   That Script must explicitly call `host.call("target_start", {})`; without it,
   no target is acquired or launched. The call returns `pending` without waiting
   for a window. Poll `host.call("target_status", {})` with Script-owned waits,
   then use actual image/template/OCR criteria before returning `"Ready"`.
   Target status (`not_requested`, `pending`, `capture_ready`), preparation phase
   and launch disposition are shown separately. Capture availability is not game
   readiness. Early capture/`"Ready"`, duplicate requests and argument overrides
   refuse. Input remains unavailable until Workflow. Typed startup faults are not
   converted to pending or automatically retried.
8. Keep launch disposition, input receipts, entry, postcondition and cleanup
   separate. An accepted launch is not a ready game or a successful workflow.
   Postconditions require a strictly newer compatible frame; input submission or
   a newer frame alone does not establish the expected effect. Independently
   confirm the authorized visible effect. First-frame placement is authoritative;
   later geometry changes are refused, not silently rescaled.

Insert explicit startup/polling before the package's existing Native Readiness
recognition criteria. For example, this finite polling fragment does not itself
declare the game ready:

```ts
let target = host.call("target_start", {});
for (let probe = 0; probe < 120 && target.status !== "capture_ready"; probe++) {
  host.call("wait", { duration_ms: 500 });
  target = host.call("target_status", {});
}
if (target.status !== "capture_ready") throw new Error("CaptureNotReady");
```

The reviewed host deadlines can expire before the local probe bound. Only
Desktop Native Readiness permits these empty-argument calls; module evaluation,
Workflow, Controlled, Replay and the independent explicit-plan CLI refuse them.
No path, recipe or process identity belongs in Script arguments.

Stop remains available through navigation and App settings. Cancellation before
launch admission submits nothing. After admission an OS request can still open
the app, even after Stop or timeout; no late completion can resume automation.
Stop does not terminate the game/launcher or undo an accepted launch. Wait for
owned request workers/callbacks and automation-child settlement before a fresh
review. An external launcher remaining alive is not an active automation worker.
Forced/incomplete/unverified Native cleanup leaves `NativeCleanupRequired`:
reconcile the target manually and restart the application. Controlled/replay
work and configuration reconstruction cannot clear that refusal.

For local acceptance, run the same authored recognition/input/postcondition
workflow with the game already running and initially absent. Review any startup
interaction explicitly; the launch library does not click through startup screens.
Record phase timing, a single absent-game launch, normal Stop and a separately
approved clean rerun. Exercise both a direct bundle and an authorized installed
separate-launcher recipe, including its argument/cwd semantics. Record exact
build/resources, independent visible effect and cleanup privately. Missing
authority, a launcher recipe, prerequisites or completion within the duration
leaves the corresponding acceptance open. Do not kill, move or resize the game
to manufacture target loss; disruptive cases need separate approval. Controlled
target-loss regressions do not replace native evidence or the independent
Windows/M0/R6 obligations.

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

The main-window configuration currently requests **1180 × 840** initially and
**760 × 600** as its minimum, in logical window dimensions. This is not a
verified supported minimum; native clamp and layout qualification remain open.
Preview sizing is unchanged. For both English and Japanese:

- Record display/work-area geometry and scale, native logical window dimensions,
  and measured WebView viewport dimensions separately. Resize interactively to
  the native minimum on each axis independently, then at both limits together.
  A programmatic window rectangle that bypasses the native clamp is not proof
  of normal interactive enforcement. Check source, metadata, Run, Logs,
  Recognition, Menu and settings for overflow and reachable primary controls.
- Measure the current header's bottom edge and content clearance after locale
  changes and resizing; keep both navigation rows visible during long document
  scrolling. Scroll a long package tree independently, collapse/expand it and
  resize without losing its position or drafts. Exercise diagnostic reveal as
  described below rather than assuming a fixed header offset.
- Check one shared Edit exclusion with another workspace selected, expandable
  static Save/Inspect details, adjacent faults, and modal-local Stop/Return.
  Verify the localized Menu name, decorative icon, keyboard behavior and focus
  return. Dismiss a notification, navigate away and back, and confirm retained
  failure and cleanup outcomes remain available. Report synthetic WebView
  actions separately from physical keyboard or native resize observations.

### Directory-package authoring acceptance

Use an isolated App data root and disposable package collections. Keep screenshots,
local paths, and compiler/run records outside public commits.

1. Create a TypeScript starter by ID under the default `sources` root. Change the
   sources folder in settings; saving must not create or move source. Restart and
   create another package using the saved root. Open an existing external source
   and a previously referenced package under `pkgs`; neither should be relocated.
2. Add `src/lib/helper.ts`, edit two scripts and check independent undo/redo,
   selection, highlighting, search/replacement, line numbers and composition.
   Request and accept SDK method/argument/result and nested options/enum
   completions; rename a field in the schema draft, make it invalid, then repair
   or discard it and verify that old suggestions do not survive. Check individual
   replacement, deletion, literal `$&`, replacement containing its query and
   more than 10,000 matches; one Undo must restore the original file.
   Expand/collapse folders and
   the entire left navigation; selection and drafts must survive. Use right-click,
   keyboard and menu-button actions to rename the helper into another folder,
   Save and reopen. Cancel removal, then confirm it; verify only the targeted
   helper changes and required-reference removal is refused. Files must contain
   only scripts/assets; Metadata opens forms or facts on the right, not code.
   Edit schema and preset fields through those forms, including a numeric draft
   across file/page navigation. Refuse occupied or nested package destinations.
   Duplicate by ID; verify original bytes and absence of App-local configuration
   in the copy. Refuse Snapshot inside either package collection before any write.
3. Save a syntax error and Validate. With the document and package tree scrolled,
   follow its diagnostic: the editor target must clear the current measured
   header and the selected tree row must be visible. Reveal must not change
   draft/saved bytes or add Undo history. Repair it and validate the new saved
   revision. Test Stop while validation owns the operation slot; no new work may
   start before it settles. Record the primary outcome and cleanup separately;
   preserve forced/incomplete cleanup rather than claiming a clean Stop.
4. Keep another workspace bound to the same source. While Edit owns the first,
   verify disabled Start/Check controls and host-side refusal of a stale client
   request. Navigate through Logs/settings and return to the unchanged drafts.
5. Exercise Save, Discard, and Cancel for editor/workspace/window closure.
   Repeat close/exit with a recoverable configuration fault; drafts must remain
   resolvable without admitting ordinary execution.
6. Open malformed schema/preset content in a disposable copy. Exercise deliberate
   structured repair/rebuild and dirty Save/Discard/Cancel, preserving original
   disk bytes until Save. Exit and reinspect; a saved local profile must not reset.
7. Run the changed valid package through the real controlled runner. Choose the
   expected state/log result before the run and compare it with the actual record.
   Saving or compiler success alone is not execution acceptance.
8. Check English/Japanese presentation and a narrow supported window. Exercise
   physical Japanese IME, native clipboard and native Edit-menu Undo/Redo;
   distinguish each from synthetic WebView events. Navigate through Recognition,
   Logs and settings, return to the same drafts, paste an existing Recognition
   snippet, and repeat the saved-source validation/run loop. Retire a failed
   analysis worker and verify subsequent editing/Save and explicit completion
   recovery. Record bundle size, worker startup/warm response observations and
   the scope of memory measurements. Missing physical IME or GUI observations
   remain incomplete; hosted checks cannot qualify them. This procedure grants
   no game input or live-capture authority.
9. During unsaved Edit, use **App settings → Editor** to save automatic opening
   off/on and delays **0 / 100 / 1000 ms**. Check Cancel, a refused save, edits
   made while Save is pending, unchanged view/selection/Undo history, and saved
   values after restart. With automatic opening off, explicitly open at `r`, type
   to `rel`, then Backspace to `re`; broader candidates must return immediately.
   With it on, rapidly type `host.` and wait for member candidates. Check escaped
   quotes at an unterminated literal's end. Dismiss before the delay or worker
   response, type an unmatched prefix, and verify that stale rows cannot return
   or insert text. Separate opening-delay measurements from provider latency.

### Saved-image Recognition acceptance

Use the actual WKWebView, an isolated root, disposable package source, and
explicitly authorized saved PNGs. Real trials additionally require the fixed
engine artifact and accepted local OCR resources. This procedure does not claim
completed acceptance or authorize new captures. Keep image/text/path evidence
private; record source checks, controlled fixtures, and actual GUI observations
separately.

1. Load a large saved PNG within the image policy. Open the separate preview,
   compare Fit and zoomed/scrolling geometry, exclude black bars, and create,
   move, resize, select, delete, and Undo regions. Check exact original-pixel
   coordinates, shared main-window selection, narrow-window overflow, and both
   languages. Close/reopen the preview and verify the draft survives. Bring the
   main window, preview, then main window to the front and observe native window
   order; the preview must not remain above the main window. Record whether
   pointer actions were WebView events or physical OS input; one does not
   qualify the other.
   For **Inspect**, use a deterministic supported PNG and an independent source
   byte oracle, including more than 4,194,304 pixels so the display is reduced.
   Include a source pixel omitted by reduction, first/last and outside-Game-content
   pixels, and alpha 0/partial/255 with stored RGB. Compare exact X/Y, RGBA and
   RGB-only hex at Fit, 150% zoom and scrolling; never use composited display
   pixels as the original oracle. Check the pixel-center marker, focused arrows
   (also with Shift), boundary no-op, Escape, pointer leave and foreign-control
   focus. Inspect must also work on a different-size unconfirmed raw frame without
   rebasing. Compare draft/Undo and saved package bytes before/after inspection,
   then return to Regions/Game content and verify ordinary editing.
   Switch from Inspect to Regions while the main window asks about unsaved changes;
   cancel that choice and confirm the next Region edit and Undo still apply.
   Exercise equal/different-size source and capture changes, A → B → A, held late
   success/failure, raster loss, tool departure and Preview close/reopen. Old
   values/queued points must not return; failed reads require explicit reselection.
   Check acquisition-start invalidation only with separate native authorization.
   At the minimum **480 × 320 content viewport**, check English/Japanese
   pending/value/cleared/error states, stable image viewport/Fit scale, scrollable
   feedback/help and reachable Done/Help plus Stop during owned work. Record
   actual WKWebView observations separately from physical input and native/Windows
   qualification; this saved-image procedure does not supply those missing checks.
2. Keep at least nine definitions. Confirm the actual child reports its grouped
   limit; trial a non-contiguous selection within it and verify attribution/order.
   Over-limit selection must refuse without hidden batching or omitted zones.
   Observe real OCR text/confidence/bounds, including no-match, without adding an
   expected-text pass/fail check. Use disposable source whose module body throws
   if evaluated; trials must not execute it.
3. Trial one template with explicit rights, a distinct pattern/search region, and
   a declared comparison image. Observe scores, threshold, and no-match separately
   from OCR. Save selected OCR/template crops and reopen without the original;
   check original-resolution crop bytes, manifest/maps, unchanged Script source,
   and saved-sample rechecking. Copy must remain unavailable without a confirmed
   loaded frame.
4. Replace with a same-size scene image and verify confirmed content is reused,
   Regions remains the default, and prior frame-dependent results and crop
   selections are invalidated. Check Copy source freshness per purpose:
   unchanged Game content setup and grouped OCR definitions/selection remain
   current; template Copy stays bound to its frame, content basis, and saved
   package revision. Save/reopen and repeat without setting up content again.
   Adjust content explicitly and verify the tool returns to Regions. Different-size
   images require confirmation, including repeated loads; changed, unconfirmed
   geometry must not be saved and reopened to bypass this gate. Discard an
   incompatible replacement and verify saved coordinates survive without its
   pixels. Exercise invalid PNG refusal and late responses without reviving them.
5. Exercise native Copy for all three purposes. Check two OCR Trial rows while
   selecting a third unchecked row: the single `scan_ocr_zones` request must
   contain exactly the checked rows in list order. Optional reference text with
   quotes and Unicode appears only in escaped comments. Nine checked rows or an
   unknown capability must refuse the whole Copy without partial publication.
   Change the checked selection and verify prior Copy becomes obsolete even if
   the original selection is restored. Inspect clipboard source privately; paste
   setup and the grouped block into a disposable Script and validate against the
   actual SDK. Check geometry guards and observation release in `finally`, with
   no waits or input. Verify unchanged source before paste, obsolete Copy after
   relevant edits, and visible clipboard failure rather than false success.
6. Resolve dirty Script, manifest, and Recognition state through Save/Discard/
   Cancel, including application close and source conflicts. Where a real
   post-commit refresh failure can be observed, confirm the completed Save is not
   repeated and the next refresh adopts saved crop references. Record unavailable
   fault timing as unexecuted.
7. Stop during actual initialization/recognition and close during owned work.
   Observe primary outcome, session cleanup, and child reaping separately before
   another operation. Preserve forced/incomplete outcomes. Missing engine/models,
   native picker interaction, total RSS, or other unexercised scenarios remain
   explicit gaps; CI and source inspection cannot supply them.

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
   import compatible legacy profiles: check unchanged source bytes, stable
   owner-scoped XIDs, idempotent retry, conflict refusal, partial-success reporting,
   and independence from another Tab inspecting the same source.

### Identifier migration acceptance

1. Copy a legacy root into a private isolated location with multiple Tabs/packages,
   profiles, target bindings, and a structurally safe stale-schema profile. Launch
   the actual app with that root. Observe Loading followed by Ready or an
   attributed Recovery; never admit a mixed generation.
2. Compare saved user content, package hashes, ownership, target revisions, and
   unassigned sources. Active IDs must be canonical 20-character XIDs; the
   stale-schema profile must still require explicit recovery. Rename, save,
   delete/reimport, close, and restart; surviving/reserved IDs must stay stable.
3. Repeat explicit Import and legacy archive Restore, including after restart.
   Verify no duplicates, edited-import conflict refusal, unchanged archive bytes,
   and the still-required clicked current preimage receipt and disposal consent.
4. Where safe interruption can be observed in a disposable generation, restart
   with the retained journal. Exercise validated recovery and stable assignments,
   stale-command refusal, visible cleanup failure, and reachable Exit. Record
   unexecuted crash or platform scenarios separately from core regression checks.

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
   inspection must not gate either. Unreviewed Native Start must remain refused. Keep
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
   selectors, and combined level/text log-search field. Check Native review/refusal
   on macOS (disabled elsewhere), Replay's scenario lock, preset/profile round trips, empty enum
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
