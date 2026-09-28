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
One application-owned child keeps the original engine and target through discovery,
explicit selection and one acquisition. Discovery is bounded to 5 seconds and
64 matching processes / 64 eligible windows. Selection expires after 120 seconds
without an idle capture session or extension on selection changes.

Capture consumes the selection. Refresh means fresh discovery and explicit
selection, followed by a separate Capture. Retaining an idle capture session or
silently rediscovering the former window would violate ownership or cleanup;
neither is a compatibility path. The visual picker is metadata-only, consumes its
own input, and removes its overlays before capture admission.

One request acquires at most one frame within 10 seconds. The SDK orders the
exact frame commitment against terminal capture state. The host separately
requires clean session close, successful non-forced child reaping, current
package/binding/installation correspondence, and owner/revision/cancellation
fences. Stop must not wait for image allocation or metadata validation. Cleanup
and containment remain separate 1-second and 2-second obligations; only the
application-owned child may be terminated. A detached accepted frame is historical:
subsequent target exit does not invalidate its pixels or grant live authority.

Reuse the existing shared image budget. Native producer, detached and mapped
payloads share a 256 MiB ceiling; observable padding is charged before accepted
publication or CPU copying. Controlled allocations reserve first. Opaque GPU/driver
allocation and total RSS are not bounded by this accounting. Parent transfer,
PNG encoding/decoding and display copies remain within the aggregate 512 MiB
payload policy; no downsampling or guessed physical-copy multiplier is used.

Recognition JSON version 2 wraps existing single-image documents in stable XID
capture namespaces. Local Region IDs, asset identities, aliases, paths, basis
and rounding remain intact. Legacy metadata migrates only on explicit Save.
Limits and Undo are aggregate; exactly one original is decoded. Leaving an
unsaved, unconfirmed basis requires confirmation or explicit discard before its
only pixels can be lost. Package publication remains the existing recoverable
transaction, not a new persistence framework.

Optional original PNG caching is OFF by default. Only accepted images may be
written beneath the platform application cache. Cache failure does not erase the
accepted frame. Explicit reload restores historical pixels with fresh runtime
revisions, never native authority. Cache files and native identities are excluded
from packages, profiles, backups and routine logs.

## Boundaries and evidence

macOS uses fresh saved-bundle/signature/architecture/process correspondence.
The host passes only freshly verified PID/lifetime/executable tuples to discovery.
The SDK's retained process provenance must match a complete tuple. Mounted runtime
paths need not equal the installed bundle path; requiring that extra equality
incorrectly rejected signed correspondence in the actual macOS WebView. Installation
validation remains separate, and this does not prove original-copy attribution.
Windows selection is transient and retains process-lifetime evidence plus the
selected non-reparse executable's file identity and read handles denying new
write/delete opens. This disk-file guard does not prove byte equality with an
image mapped before selection or revoke an existing writable mapping. Access or
identity failures remain refusals, not reasons to elevate or choose another target.

Portable lifecycle, namespace and publication regressions, real child-protocol
smokes, and hosted shell builds are distinct from native acceptance. Windows GUI,
real target/window capture, OCR resources, picker behavior and mixed-display
scenarios require their own authorized interactive host. No native Script Start,
input, launch, focus, permission prompt, retry, continuous preview, release
packaging or full M0/M2/R6 qualification is granted by this decision.

See [desktop checkout and acceptance guidance](../desktop.md) and
[engine prerequisites](../runtime-native.md).
