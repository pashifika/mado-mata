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
actions and preserve source bytes. Identical normalized profile payloads are
idempotent; conflicts retain the source and any already committed subset.

Inspect changes only the owning `tab.config` among managed configuration files,
before publishing the in-memory selection. It never writes the App
`package_path` hint; old hints remain inert and are preserved by preference
saves. Reject a global recent-package writer or registry: either would couple
independent Tabs and introduce a second authority beside their saved records.
Custom package archives remain unsupported. Edit was guidance when this
configuration change shipped; subsequent
[directory-package authoring](../desktop.md#edit-directory-packages) keeps source
publication separate from configuration snapshots and profile reconciliation.

Manual authoring acceptance clarified that source/configuration separation is
an ownership boundary, not a requirement for unrelated parent directories.
The default editable collection is `<data-dir>/sources`; a saved packages-root
override changes only future ID-derived Create/Duplicate destinations. `pkgs`
is reserved for downloaded/packaged content, while existing explicit references
remain usable. Managed capture, restore and reset never traverse or replace
either subtree. `authoring` remains private publication-journal storage. Other
configuration/transaction paths remain forbidden source locations; existing
packages, explicit roots and active leases are not moved by preference changes.

Amend [ADR 0004](0004-desktop-localization-resources.md) only for presentation
before settings are available: the temporary language and Setup's saved-language
draft are independent. [`bootstrap.ts`](../../apps/desktop/src/bootstrap.ts)
keeps them separate; [`App.tsx`](../../apps/desktop/src/App.tsx) gives loaded saved
settings precedence. Changing temporary presentation does not save preferences.

## Persisted identity transition

Use the public `pashifika/xid-rs` Fork at the immutable revision recorded in Cargo
and its lockfile. The shared boundary returns allocation errors and accepts only
canonical 20-character XIDs, including canonical trailing bits. Ordinary writers
never generate the former 60-character `p-` format. No sibling checkout, local
replacement generator, or floating dependency is a public build prerequisite.

Before Application construction, convert eligible typed profile/target identities
and profile filenames without changing values, schema identities, ownership, or
target revisions. A rejected schema stays rejected. Ordinary reads remain
non-mutating. Explicit historical-root/profile Import and Restore normalize only
their installed copy; unassigned profiles and original archives remain unchanged.
Package content, local Region IDs, and existing assets are outside this transition.

One versioned `identity-migrations.config` maps `(kind, Tab, package, legacy ID)`
to its XID. Bound it to 4,096 entries and 4 MiB inside the managed-set budgets.
Reject duplicate/conflicting mappings, unknown metadata, and overflow rather than
evict history. Retain reservations for deleted entities and retired owners without
reactivating them. Repeat Import compares normalized content and refuses edits;
Restore combines compatible archive/live reservations and preserves live entries
even when the archive has no ledger. Conflicting lineages refuse before mutation.

Reuse only the narrow recoverable publisher, not manual Restore admission.
Automatic conversion has a typed plan bound to the captured generation; Restore
still requires the clicked current receipt and explicit disposal/replacement
consent. Version-2 journals tag Restore, automatic migration and owner-bound profile
Import separately; version-1 Restore journals remain recoverable. Journal the
assignments and complete preimages before replacing identities. Rollback restores
user content while retaining exact staged ledger bytes before cleanup; its
reservation-bearing generation must fit the managed budget before publication.
Pending evidence blocks ordinary admission and uses explicit validated recovery;
a completion marker fixes the permitted direction. Reconstruction retires old
runtime expectations rather than translating stale commands. Admission reads the
immutable root without taking the preparation worker's Store lock, preserving
polling and Stop independence.

Snapshots preserve present ledger bytes even when malformed. Ledger-bearing
archives use version 2; supported version-1 archives remain readable. Preservation
does not establish restore eligibility. Never down-convert storage for an older
binary or rewrite old backups.

XID components are observable, not cryptographic secrets or authority. A converted
ID records migration-time allocation, not historical creation time. Conversion
has no date expiry or timer-driven ledger cleanup. Removing legacy input requires
a separate approved release decision naming the first rejecting version, an
available converter-release path for old roots/backups, and ledger retirement.
Capture/asset migration, Windows durability, native qualification, and R6 remain
independent gates.

The release-boundary plan originally assumed published product releases.
GitHub's release list and remote tags were empty at implementation; both desktop
manifests still identify development version `0.1.0`. Record the exact
pre-converter checkout baseline in the [transition notes](../desktop.md#development-transition-notes)
instead of inventing a release. The first actual converter release must record
the published boundary; this does not authorize release packaging or a cutoff.

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
collects at most 4,096 managed files and 16 MiB of raw bytes, including the
migration ledger and safe malformed or orphaned configuration. Two observations
detect source changes;
the Application store lock serializes in-process writers, not external ones.
[`backup`](../../apps/desktop/src-tauri/src/backup.rs) limits the manifest to
4 MiB and the archive to 32 MiB. It excludes package payloads, logs, temporary
files, and backups. Preservation is not restore eligibility: Restore separately
requires supported schemas and consistent Tab/package/profile ownership.

Publish exactly `app.config.<seconds>` from synced private staging and verify
the published archive. A name collision is a failure, not permission to overwrite
or choose a suffix. Destinations resolve existing ancestors and cannot enter
managed `tabs/`, `profiles/`, `.restore*`, either `sources` or `pkgs`, the configured
package collection, or an existing package. Source exclusion precedes directory and
archive creation, including when malformed settings need a raw recovery
snapshot. Missing-only publication uses `renamex_np(RENAME_EXCL)` on macOS,
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
or recovery consumes it. Window close and native Quit contain a published
Application before waiting for an in-flight snapshot or recovery action, and
retire an Application published by a load that raced ahead of closing once that
action settles; the wait does not make a hung filesystem call interruptible.

[`restore`](../../apps/desktop/src-tauri/src/restore.rs) stages and syncs the
journal and preimages before the first managed replacement. It verifies the
complete installed generation, rolls back failures when possible, and otherwise
retains discoverable recovery state. Reject whole-root swaps: logs and package
payloads must remain outside the write set. Before deleting preimages, publish
the bounded `.restore-completion` marker. `pending` recognizes both that marker
and `.restore-journal`; cleanup revalidates the committed generation and retains
its direction through partial deletion and restart. Unknown files are preserved,
not recursively erased to force cleanup success. After full-generation
verification, install and rollback remove without recursion the Tab and package
containers the generation no longer owns. Retiring Tabs also enumerate their
immediate children without following links and remove only empty directories:
deleting the last profile can leave a package container absent from both file
generations. Discovery must not report an invented orphan; a container still
holding unmanaged data is kept and remains attributable. Installed bytes,
incomplete cleanup, and failed Application reconstruction remain distinct outcomes.

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
  `restore_requires_a_clicked_current_preservation_receipt` and
  `shutdown_contains_the_published_application_before_an_in_flight_action_settles`.
- [`restore.rs`](../../apps/desktop/src-tauri/src/restore.rs):
  `each_publication_and_file_failure_restores_original_generation`,
  `interrupted_committed_cleanup_remains_pending_until_restart_recovery_finishes`,
  `committed_cleanup_revalidates_live_generation_before_removing_evidence`,
  `restore_removing_a_tab_with_package_configuration_frees_its_name_and_slots`,
  `restore_after_deleting_the_last_profile_does_not_invent_an_orphan`,
  and `rolled_back_restore_that_adds_a_tab_with_a_profile_leaves_no_orphan`.

The original pre-review local core run passed 104 tests with:

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
