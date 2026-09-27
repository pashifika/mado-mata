# ADR 0006: Saved-image recognition displays observations, not script verdicts

Status: Accepted for macOS saved-image authoring.

## Decision

Display every bounded public OCR region in engine order: returned text,
confidence, capture-pixel geometry, and originating zone. Report recognition
present or absent. Do not compare observations with an expected string, hide
unexpected text, concatenate regions, or add application-side normalization.
A recognition-present result does not establish that the text is correct.

Fine matching criteria and score policy belong to Script authors. Optional
reference text is preserved author context, including across kind changes.
Grouped OCR Copy includes it only in escaped comments, not as an engine argument,
wait condition, filter, or authoring pass/fail oracle. Template trials use validated
asset defaults and display their effective threshold, actual scores, and returned
boxes. No-match has no invented score. The authoring surface adds no fine
score-tuning controls or automatic OCR/template fallback.

This replaces the earlier proposal to display exact-match verdicts against
independently entered expected text. It follows the operator's decision after
checking the adopted engine contract.

## Evidence

At engine revision `acc5d98ae8cfc4958970be826a28011bc12185c9`,
[`RecognizedRegion::text`](https://github.com/pashifika/mado-pilot/blob/acc5d98ae8cfc4958970be826a28011bc12185c9/crates/automation/ocr/src/result.rs)
is explicitly NFC-normalized and Unicode-trimmed.
[Backend normalization](https://github.com/pashifika/mado-pilot/blob/acc5d98ae8cfc4958970be826a28011bc12185c9/crates/automation/ocr/src/normalization.rs)
happens before public results are constructed. Public confidence and geometry
are likewise engine results, not untouched inference-backend buffers. The
integrated public facade does not provide the pre-normalized text stream.

The application therefore preserves **public observations**, not backend-raw
bytes. No private backend access, alternate provider, or engine pin change is
needed to implement this decision. The engine's public grouped-request limit
remains authoritative and is unrelated to the number of saved definitions.

## Preview window

One independent, normal-level native window owns the image display, zoom, content
selection, and on-image region editing. It is not parented above the main window.
Definition lists, trial results, Save, and Copy remain in the main Edit window.
Both use the same owner and revision-bound metadata; closing the preview does
not discard drafts or release Edit.

Game content is reusable setup; Regions is the default repeated-authoring tool.
Confirmed content is reused for same-size scene images and from saved metadata
after reopening. Explicit content edits and different image dimensions require
confirmation. Equal dimensions cannot establish equal placement; the operator
must adjust content when placement changes. Every replacement still invalidates
frame observations and current crop selections. Copy source freshness is separate
from the historical trial evidence shown at Copy time.

## Image policy

Replace ADR 0003's blanket 1 MiB package capture and 2 MiB expanded replay
allowances with the shared bounds in
[`images.rs`](../../tools/runtime-comparison/src/images.rs). Image payloads
cannot borrow the non-image allowance, or the reverse. Desktop Edit, Inspect,
Check, and Start use the same package ceiling; an explicit lower CLI Plan limit
still applies.

The independent ceilings are:

| Payload | Maximum |
| --- | --- |
| Loaded PNG | 32 MiB encoded; 16,777,216 original pixels |
| Selected PNG crop | 16 MiB encoded; 4,194,304 original pixels |
| Any image side | 16,384 pixels |
| Package image assets | 64 MiB encoded PNG or packed raw bytes |
| Package non-image content | 1 MiB, including source, profiles, schemas, and JSON metadata |
| Total package content | 65 MiB |
| Aggregate decoded package images | 128 MiB RGBA8 |
| Expanded replay frames | 128 MiB RGBA8, counting repeated frame acquisitions |
| Accounted application-owned payloads | 512 MiB |

Validate image format, dimensions, and compressed/decoded bounds before admitting
pixels. Recognition and crop persistence use original pixels, not the bounded
preview raster. Payload reservations cover owned buffers and reserved child
copies through physical settlement; they are not a total process-RSS bound or
an accounting of native model/library internals. Historical replay evidence did
not test these new ceilings. Actual acceptance remains separate.

### Inventory identity

`mado-inventory-v2` hashes bounded metadata JSON followed by sorted asset names,
payload lengths, and SHA-256 payload fingerprints. Names and payload lengths use
unsigned 64-bit little-endian byte lengths. The immutable payload owner computes
its fingerprint once and shares it across validation and compilation clones;
deserialized bytes receive a fresh fingerprint, never a sender-supplied cache.

The previous decimal-JSON image representation exhausted the existing 10-second
capture deadline for an admitted 3840 × 2160 RGBA frame in local acceptance.
Repeated raw hashing also consumed the replay operation's finite preparation
budget. Fingerprinting avoids both costs and extra image copies; deadlines are
not extended.

Inventory identities are opaque and must be reacquired by Inspect after rebuilding
the desktop and both runner artifacts. This is a clean identity-version cutover,
not a compatibility alias; package file formats and saved profile schemas do not
change.

## Source Copy

Copy requires a loaded frame with confirmed geometry; a saved-sample trial does
not establish placement. Game content setup emits reusable `recognitionBasis`
data without requiring selected definitions. This is not Engine initialization;
resource paths, environment, target selection, and native authority remain outside
Script. Setup follows actual geometry values and is never trial-verified.

The original Script SDK exposed single-ROI recognition, although the public engine
already supported grouped OCR. The operator approved extending the SDK rather than
presenting sequential waits as grouping. `scan_ocr_zones` now maps every checked OCR
region relative to the pasted basis and invokes one public grouped scan over one
retained observation. The whole selection must fit the actual engine capability;
unknown bounds and excess selection are refused, not split.

The bounded result preserves every public observation, caller/engine ordering, and
explicit no-match zones. It is a plain snapshot without a result handle; generated
code releases the observation in `finally`. OCR source has no expected-text filter,
wait, input, or logging. Legacy single-ROI `recognize`, `query`, and `query_wait`
remain legitimate Script APIs, but sequential wait Copy is removed.

Grouped source freshness follows normalized definitions and checked selection.
The separately copied setup must be replaced after Game content changes.
Template Copy retains its concrete basis, selected saved pixels, metadata, rights,
and maps. Trial evidence describes observations at Copy time, never later images
or text correctness. Absence of current successful observations is unverified,
not a reason to filter or discard generated source.

Only explicit Copy publishes generated `mado-host-v1` source to the macOS
`NSPasteboard` on the main thread. The WebView does not write through
`navigator.clipboard` or receive a general clipboard capability. Publication
failure is a Copy failure, not success. No Copy edits package source, logs
recognized text, or keeps pasted code synchronized after later metadata changes.

## Verification boundary

Source evidence establishes the public API semantics, not recognition quality.
Actual WKWebView and owned-child acceptance still require authorized saved
images and supported local resources. Controlled fixtures, CI, and correct
missing-prerequisite refusals do not replace those observations or qualify
live capture/input, Windows desktop, or native target publication.
