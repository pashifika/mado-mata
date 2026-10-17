# Changelog

## Unreleased

- Execute a whole package or an existing file-level function directly in Edit.
  Reuse explicit Save all, immutable source capture and the supervised runner;
  rerun current source with fresh VM state while preserving the game's state.
  Native Function is separately reviewed and attach-only, without launch,
  recovery or a hidden readiness call. Keep shared Stop and independent cleanup.
- Put saved-package Validate and an icon-bearing Setup entry in Package contents.
  Keep function/arguments and Run/Rerun in a compact dialog; open full execution
  settings on Run without ending Edit or losing source drafts and Undo.
  The icon reports package validation only; Status details retains diagnostics.
  Remove Run's separate profile Validate action while retaining inline errors
  and mandatory Start/Save validation.

- Keep macOS engine-check temporary paths short for Unix socket fixtures,
  independently of its descriptive build and dependency-cache directory.

- Support native Windows `.exe` selection for separate game and launcher target
  fields. Preserve Cancel, stale-owner refusal, metadata-only Check, explicit
  Save and restart readback; reject shortcuts and reparse-point files. Keep
  saved foreign-platform bindings unchanged and Windows Native Start unavailable.

- Bound the native preflight regression's settlement wait by its admitted
  operation and containment budgets, without changing production deadlines.

- Assign fresh Windows OMP test fixtures to the exact test user when hosted
  runners default ownership to a group; retain strict production owner checks.

- Cover PNG encoding failure before capture acceptance: no frame commit or
  published transfer, with the acquired session still closed exactly once.

- Run the full repository check and model-free engine consumer tests on hosted
  macOS 26 arm64, including checksum-pinned actionlint and offline local-link
  validation. Resolve native setup from the pinned SDK checkout; preserve its
  macOS deployment floor and record the installed OpenCV 4 version. Keep Windows
  runtime-only, event-specific gate isolation and physical acceptance separate.

- Support native Windows OMP Agent integration with reviewed installation
  management, bounded Job-owned command trees and private authenticated named
  pipes. Release adapter `0.1.3`; preserve explicit owner/disclosure checks,
  unknown outcomes, existing OMP sessions and macOS transport behavior.
  Pass identity-checked ordinary Windows source paths to OMP's link command,
  avoiding malformed junctions from duplicated native namespace prefixes.
  Base actual integration acceptance on the locally installed OMP at test time,
  recording its version rather than requiring historical-version reproduction.
- Load application, preview, Edit, Recognition and source-editor code at their
  actual view boundaries instead of one oversized startup bundle. Preserve
  session ownership, operation controls and focus while local chunks load.
- Contain the package library's hidden live status within its list scrollport,
  preventing large Windows catalogs from scrolling the outer dialog.
- Support long Windows paths during atomic no-replace configuration publication,
  including restore displacement and recovery. Keep existing-destination refusal
  and verify continuation/rollback without shortening the configured data root.
- Scope intentional Unix-only process limits/outcomes and directory mutability
  diagnostics to their consumers; remove unused test/import bindings and gate
  engine-only correspondence methods precisely, retaining global warnings.
- Preserve the trusted Node executable for Windows JavaScript and TypeScript
  runs before restricting the native DLL search path. Keep initialization-only
  environment checks independent of Node and retain parser isolation.
- Do not start CI when a PR is marked ready for review. Retain opening, head
  updates, reopening, and explicit base-retarget revalidation.
- Add a read-only Configuration doctor available before Application construction,
  with bounded owner-scoped findings, explicit unexamined scope and existing
  recovery entry points. It neither rewrites configuration nor grants execution
  authority.
  Keep failures visible beside unexamined scope, avoid ownerless workspace routes,
  and expose guarded Retry even when the retained Application is still Ready.
- Replace the inline installed-package list with a searchable, viewport-bounded
  library popup. Group releases by signer/package, preserve browsing through
  Import and diagnosis, and require exact-artifact selection for the captured
  workspace. Keep Stop independent of catalog verification.
  Show declared authors and current versions instead of key fingerprints, with
  an explicit Apply action. Put Packages… at Guidance's upper right and group
  Reinspect and Change… in Run's source header, apart from editing and details.
  Keep a pending directory replacement visible when source details are collapsed.
- Isolate controlled-plan test filenames across concurrent calls instead of relying
  on clock resolution alone.
- Add [signed package transfer](docs/desktop.md#transfer-signed-packages):
  distribution metadata, Google author enrollment, offline Ed25519 export and
  verified `.mmpkg` import. Installed releases require explicit selection;
  recipient profiles and target bindings remain local. Source stays readable
  externally, and signatures do not certify an author's identity.
  Enrollment uses a build-supplied native registration key; public CI needs none.
  Upgrade the bundled OMP adapter to `0.1.2` for shared metadata editing.
- Put Export PKG beside Duplicate in the authoring rail; simplify export and
  unsaved-exit actions. Use contextual release/license help and clarify multiline
  plain-text descriptions.
- Keep installed packages as original archives under signing-key/package/artifact
  paths, with verified in-memory execution and no extracted payload or receipts.
  Remove installed-source viewing, editing and duplication through the app and OMP.
  Reuse same-signer version settings; refuse signer changes before local mutations.
  Old expanded installations require reimporting their original `.mmpkg`.
- Keep signed-transfer bounds consistent before publication and decompression.
  Allow import alongside an active Run/Check without changing its captured inputs;
  keep Stop reachable during publication without moving lost focus to it.
  Reject occupied export names before preparation, restore focus only to usable
  controls, and distinguish command contention from unsettled package cleanup on Close.
- Read complete HTTP response frames in OIDC loopback tests without requiring
  a graceful TCP close after oversized-header refusal; retain rejection,
  secret-redaction and callback completion checks. Await the listener-owning
  thread rather than probing a TCP address that can already have a new owner.
- Add bounded Script mouse drags with ordered capture-pixel paths and optional
  post-arrival waits. Preserve partial event receipts and owned-button cleanup
  on Stop without treating cleanup as game cancellation. Share types, EN/JA SDK
  discovery and three usage examples across Edit and OMP; disclose release-on-Stop
  risks in Native review. Controlled/replay checks do not qualify live game input.
- Reuse intact managed OCR models and runtimes after an engine-pin-only update.
  Keep acquisition receipts unchanged and continue rejecting corrupt resources.

- Keep the retirement-close regression valid when bounded log shutdown times out;
  still verify retirement, close, and refused new commands without changing
  production shutdown behavior.
- Add a separate [Plugins dialog](docs/desktop.md#install-select-and-remove-the-adapter)
  for the application-supplied OMP adapter: reviewed user-scope Install, Update,
  Uninstall and checkout migration with verified readback and fresh-session guidance.
  Separate System/External tabs and compact entries from per-plugin Details pages;
  navigation retains in-flight actions, target selection and historical receipts.
  Refuse dotenv-directed targets, serialize shared-installation commands across
  desktop instances, and settle owned command groups before releasing ownership.
  Keep uncertain work fenced across restarts.
- Join Undo/Redo as two independent buttons; move editable Metadata history and
  Discard into its heading while keeping File actions in the source toolbar.
- Add [OMP collaboration](docs/desktop.md#collaborate-through-omp) for shared
  desktop drafts, explicit publication and lifecycle resolution, SDK discovery
  and clipboard-independent Recognition snippets. Requires OMP 18.8.7 or newer;
  no upper bound, package migration or native execution authority is added.
- Retain the source editor's viewport anchor outside an externally changed span
  instead of replacing the entire CodeMirror document.
