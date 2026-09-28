# ADR 0007: One-shot native capture authoring

- Status: Accepted
- Date: 2026-09-28
- Scope: Desktop Recognition authoring, not native Script execution or release qualification

## Context

Saved-image authoring already owns one decoded original, bounded geometry and
Undo, real input-free trials, and recoverable crop/metadata publication. Native
acquisition must preserve those contracts without reconstructing window authority
from a PID, title, or native window number. The public engine retains that
authority in its discovery instance; it cannot transfer it to a replacement child.

## Decision

Use the public `mado-pilot` revision
`4b4f3296838a9eecdcb00e9d2bb3121a25cdc240`, including retained window metadata,
required geometry and image-payload limits, and `Session::commit_frame`.
One application-owned child keeps the original Engine and TargetId for the Preview
lifetime. Discovery is bounded to 5 seconds and 64 matching processes / 64 eligible
windows. First selection requires no prior Run-page binding. The child must
acknowledge the exact retained candidate before the host verifies and saves its
application/window locator; rejected selection cannot overwrite a saved locator.
Before the first Capture, the worker adopts the committed binding revision
without rediscovery. Later cancellation refuses acquisition but does not undo
an admitted durable save. No input policy is invented. Saved locators require
fresh verification and exactly one matching window on explicit Start; they
never persist native authority.

Retaining the Engine is distinct from retaining a capture session. Each explicit
Capture opens one bounded session, commits at most one frame within 10 seconds,
and requires clean session close before publication. The worker remains idle
between frames; no continuous sampling or silent rediscovery is permitted.
This replaces the earlier consumed-selection/120-second-expiry decision: the
public API can reopen the original TargetId while independently closing each
session. Replay protection retains at most 4096 capture identities per Engine;
exhaustion requires explicit restart.

The SDK orders frame commitment against terminal capture state. The host checks
current package/binding/installation correspondence and owner/revision/cancellation
fences. Unchanged target constraints and fresh source proof permit package-revision
rebasing after metadata/crop Save without changing Engine/TargetId. Done, Preview
close/destruction, cancellation and owner exit stop/reap only the owned worker.
Stop does not wait for image allocation or metadata validation. Cleanup and
containment remain separate 1-second and 2-second obligations, with incomplete
outcomes preserved. A detached frame remains historical after target exit.

Reuse the existing shared image budget. Native producer, detached and mapped
payloads share a 256 MiB ceiling; observable padding is charged before accepted
publication or CPU copying. Controlled allocations reserve first. Opaque GPU/driver
allocation and total RSS are not bounded by this accounting. Parent transfer,
PNG encoding/decoding and display copies remain within the aggregate 512 MiB
payload policy; no downsampling or guessed physical-copy multiplier is used.

Recognition JSON version 2 wraps existing single-image documents in stable XID
capture namespaces. Local Region IDs, asset identities, aliases, paths, basis
and rounding remain intact. Legacy metadata migrates only on explicit Save.
Limits and Undo are aggregate; exactly one original is decoded. Capture refresh
retains its namespace, Regions and draft; New capture creates a separate document.
Pending crop pixels are staged with source-frame and geometry fingerprints under
the shared budget before replacement. Save cannot substitute newer pixels.
Same-size refresh retains confirmed Game content; resized frames require explicit
rebase/confirmation. Saved crops change only on explicit Save. Publication uses
the existing recoverable transaction, not a second persistence framework.

Original PNG caching is persisted and OFF by default in App settings, alongside
size/path/folder management. Accepted images use the selected configuration
root's `caches/` directory: `~/.config/mado-mata/caches` by default, or
`PATH/caches` with `--data-dir PATH`. Every cache operation shares that root;
the former platform cache is neither a fallback nor automatically migrated.
Refresh replaces only its capture's original. Cache failure does not erase
an accepted frame. Explicit reload restores historical pixels with fresh runtime
revisions, never native authority. Historical status is independent of the
optional native acquisition timestamp. Configuration snapshots use their
existing managed-file whitelist, excluding cache bytes despite the shared parent root.
Image bytes/native identities remain excluded from packages, profiles, backups
and routine logs; the preference may be backed up.

A controlled post-validation file-link substitution previously loaded pixels
outside the cache. Reads now retain the verified file handle; publication and
cleanup retain the package-directory identity (directory-relative on Unix/macOS,
ancestor handles without delete sharing on Windows). Publication may replace a
link entry without following it. Measurement rejects unsafe/disappearing entries
and observed directory changes, but is not an atomic filesystem snapshot.

Folder opening creates missing managed directories and validates the pathname
before external shell dispatch; it reads or writes no file contents. Finder and
Explorer resolve that pathname independently. Deliberate same-user replacement
after validation is outside this containment guarantee; dispatch success is not
proof of the file manager's eventual resolution. This supersedes the earlier
absolute folder-open guarantee rather than adding a hostile-user sandbox.

PNG encoding precedes `Session::commit_frame` and host acceptance. The cache
reuses the bounded transfer PNG; it does not encode a second image. Encoding
failure therefore refuses acquisition, while a cache write/publication failure
preserves an accepted frame. A valid 4096 x 4096 incompressible RGBA fixture
reached the real encoder's compressed-byte refusal; a real native capture with
an occupied cache staging path retained its new frame and prior cached file.
These observations replace the earlier post-acceptance encoding-failure
assumption, not the independent cache-write failure guarantee.

Preview feedback must not change the image viewport or its Fit scale. A fixed
status rail holds progress; help and bounded scrollable errors/warnings overlay
the image. Toolbar rows depend on window width, not capture/error state. This
replaces in-flow feedback, which resized the image at capture start and finish.
There is no outer feedback frame. Individual error, warning and help messages use
red, amber and blue borders, respectively.

## Boundaries and evidence

macOS uses fresh bundle/signature/architecture/process correspondence, including
validation of a first-use candidate against the installed application. The SDK's
retained process provenance must match the complete verified tuple. Mounted
runtime paths need not equal the installed bundle path; that extra equality
incorrectly rejected signed correspondence in the actual WebView. Installation
validation remains separate and does not prove original-copy attribution.
Windows saves an executable locator, not process/window authority, and retains
process-lifetime evidence plus the non-reparse executable's file identity/read
handles denying new write/delete opens. This guard does not prove byte equality
with an already mapped image or revoke existing writable mappings. Identity/access
failures remain refusals, never reasons to elevate or choose another target.

The public window geometry does not guarantee an OS-title-bar-free macOS content
rectangle. Deeper element inspection would introduce an Accessibility permission
route outside this Change. Keep manual Game content, reusable across same-size
refreshes, rather than guess title insets.

Portable lifecycle, namespace and publication regressions, real child-protocol
smokes, and hosted shell builds are distinct from native acceptance. Windows GUI,
real target/window capture, OCR resources, picker behavior and mixed-display
scenarios require their own authorized interactive host. No native Script Start,
input, launch, focus, permission prompt, retry, continuous preview, release
packaging or full M0/M2/R6 qualification is granted by this decision.

See [desktop checkout and acceptance guidance](../desktop.md) and
[engine prerequisites](../runtime-native.md).
