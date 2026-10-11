# Changelog

## Unreleased

- Add a separate [Plugins dialog](docs/desktop.md#install-select-and-remove-the-adapter)
  for the application-supplied OMP adapter: reviewed user-scope Install, Update,
  Uninstall and checkout migration with verified readback and fresh-session guidance.
- Join Undo/Redo as two independent buttons; move editable Metadata history and
  Discard into its heading while keeping File actions in the source toolbar.
- Add [OMP collaboration](docs/desktop.md#collaborate-through-omp) for shared
  desktop drafts, explicit publication and lifecycle resolution, SDK discovery
  and clipboard-independent Recognition snippets. Requires OMP 18.8.7 or newer;
  no upper bound, package migration or native execution authority is added.
- Retain the source editor's viewport anchor outside an externally changed span
  instead of replacing the entire CodeMirror document.
