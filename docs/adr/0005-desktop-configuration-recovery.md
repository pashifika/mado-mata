# ADR 0005: Desktop configuration recovery and named workspaces

Status: Accepted for the controlled macOS development application.

## Decision

Keep the shell independent of successful `Application` construction.
[`Bootstrap::new`](../../apps/desktop/src-tauri/src/bootstrap.rs) performs no I/O;
[`bootstrap_status`](../../apps/desktop/src-tauri/src/main.rs) dispatches initial
loading to a blocking worker. `Application::new` still reads settings and
revalidates each saved-open Tab's selected directory source. Explicit Loading
remains visible until that work resolves; no default settings or synthetic
inventory stands in for an unknown result. Missing settings require Setup;
invalid present data
requires Recovery. Later faults retain the existing Application for Stop and
polling rather than hiding an owned operation.

[`Store::new`](../../apps/desktop/src-tauri/src/storage.rs) validates a present
root without creating an absent one. Initialize validates and encodes preferences
before directory creation, then publishes missing settings without replacement.
Reject automatic default repair: missing and malformed data require different
operator decisions, and a stale Initialize must not overwrite a newly appearing
document.

Persist each named Tab in `tabs/<internal_name>/tab.config`. Its immutable
internal name owns storage; its display name is not an identity. Runtime
workspace IDs and revisions remain session-local. Reopening revalidates the
selected source and retains owner-scoped source failures. Profiles belong to
`tabs/<internal_name>/<package_id>/<profile_id>.config`, not a shared catalog.
Historical-root import and compatible legacy-profile import require explicit
actions and preserve source bytes. Identical imported profile bytes are
idempotent; conflicts retain both the source and any already committed subset.

Inspect changes only the owning `tab.config` among managed configuration files,
before publishing the in-memory selection. It never writes the App
`package_path` hint; old hints remain inert and are preserved by preference
saves. Reject a global recent-package writer or registry: either would couple
independent Tabs and introduce a second authority beside their saved records.
Custom package archives remain unsupported, and Edit remains guidance rather
than a package editor.

Amend [ADR 0004](0004-desktop-localization-resources.md) only for presentation
before settings are available: the temporary language and Setup's saved-language
draft are independent. [`bootstrap.ts`](../../apps/desktop/src/bootstrap.ts)
keeps them separate; [`App.tsx`](../../apps/desktop/src/App.tsx) gives loaded saved
settings precedence. Changing temporary presentation does not save preferences.

## Snapshot and restore boundary

Use standard stored ZIP, not CustomZip or a new package container. CustomZip's
unrelated package-compatibility protocol must not become a recovery dependency.
The exact
[`Cargo.toml`](../../apps/desktop/src-tauri/Cargo.toml) pins are `zip =8.6.0` with
`default-features = false` and `unicode-normalization =0.1.25`; manifests and
lockfiles retain the existing runtime and shell pins. The ZIP crate owns entry
reading and CRC checks; the application owns bounded framing, manifest hashes,
managed paths, and alias checks. Reject compressed, encrypted, linked, duplicate,
traversing, or unsupported entry forms before installation.
The archive is not encrypted; hashes establish byte consistency, not author
trust or execution authority.

[`configuration::capture`](../../apps/desktop/src-tauri/src/configuration.rs)
collects at most 4,096 managed files and 16 MiB of raw bytes, including safe
malformed or orphaned configuration. Two observations detect source changes;
the Application store lock serializes in-process writers, not external ones.
[`backup`](../../apps/desktop/src-tauri/src/backup.rs) limits the manifest to
4 MiB and the archive to 32 MiB. It excludes package payloads, logs, temporary
files, and backups. Preservation is not restore eligibility: Restore separately
requires supported schemas and consistent Tab/package/profile ownership.

Publish exactly `app.config.<seconds>` from synced private staging and verify
the published archive. A name collision is a failure, not permission to overwrite
or choose a suffix. Explicit destinations resolve existing ancestors and cannot
enter managed `tabs/`, `profiles/`, or `.restore*` storage. Missing-only
publication uses `renamex_np(RENAME_EXCL)` on macOS,
`renameat2(RENAME_NOREPLACE)` on Linux, and `MoveFileExW` with flags `0` on
Windows. `std::fs::rename` is not a no-replace substitute; unsupported publication
fails rather than falling back to an overwriting operation.

Before replacing nonempty configuration, require a separately requested snapshot
receipt from this session, its still-readable matching archive, and a generation
matching the current managed bytes. Restore requires explicit replacement
confirmation. Retry, Restore, and interrupted-restore recovery require explicit
session disposal whenever an Application exists, even with clean profile drafts.
Reconstruction admits only settled commands and an idle controller, closes future
admission, and retires owned resources before replacement. An incomplete logger
shutdown requires Exit and relaunch; duplicate Retry writers are not a recovery
strategy. Failed attempts retain the previous valid receipt; successful install
or recovery consumes it.

[`restore`](../../apps/desktop/src-tauri/src/restore.rs) stages and syncs the
journal and preimages before the first managed replacement. It verifies the
complete installed generation, rolls back failures when possible, and otherwise
retains discoverable recovery state. Reject whole-root swaps: logs and package
payloads must remain outside the write set. Before deleting preimages, publish
the bounded `.restore-completion` marker. `pending` recognizes both that marker
and `.restore-journal`; cleanup revalidates the committed generation and retains
its direction through partial deletion and restart. Unknown files are preserved,
not recursively erased to force cleanup success. Installed bytes, incomplete
cleanup, and failed Application reconstruction remain distinct outcomes.

Snapshot, import, and restore publication sync directories on Unix. Windows
directory sync is not implemented, so Windows crash durability remains
unqualified. Ordinary Store replacement retains the existing file-sync and
atomic-rename publication without parent-directory sync. Neither that path nor
the fault-injection tests establish universal power-loss durability.

## Evidence and limits

The source regressions exercise these contracts:

- [`storage.rs`](../../apps/desktop/src-tauri/src/storage.rs):
  `invalid_initialization_leaves_absent_destination_and_legacy_source_untouched`,
  `missing_only_publication_refuses_destination_appearance`, and
  `explicit_legacy_import_is_exact_idempotent_and_not_shared`.
- [`application.rs`](../../apps/desktop/src-tauri/src/application.rs):
  `inspection_preserves_unrelated_app_preferences_and_legacy_hint` and
  `reconstruction_requires_settled_commands_and_closes_all_future_admission`.
- [`backup.rs`](../../apps/desktop/src-tauri/src/backup.rs):
  `raw_roundtrip_collision_and_explicit_destination` and
  `refuses_malicious_features_and_manifest_disagreement_at_reader`.
- [`bootstrap.rs`](../../apps/desktop/src-tauri/src/bootstrap.rs):
  `restore_requires_a_clicked_current_preservation_receipt`.
- [`restore.rs`](../../apps/desktop/src-tauri/src/restore.rs):
  `each_publication_and_file_failure_restores_original_generation`,
  `interrupted_committed_cleanup_remains_pending_until_restart_recovery_finishes`,
  and `committed_cleanup_revalidates_live_generation_before_removing_evidence`.

The recorded local core run passed 104 tests with:

```sh
cargo +1.98.1 test --manifest-path apps/desktop/src-tauri/Cargo.toml --locked --no-default-features --lib
```

That result does not establish full CI, WebView acceptance, replay, or native
qualification. The [CI guide](../ci.md) owns public checks; the
[desktop guide](../desktop.md#local-gui-acceptance) owns separate GUI acceptance.
The supervised runtime, fixed runner paths, non-native input sink, and native
authority boundaries of [ADR 0002](0002-desktop-runner-boundary.md) and
[ADR 0003](0003-desktop-recorded-replay.md) remain unchanged. Normal builds must
not enable the development-only `webdriver` feature.
