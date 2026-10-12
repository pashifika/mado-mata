# Changelog

## Unreleased

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
